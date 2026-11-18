// remoteman's microphone for the Mac host: a CoreAudio server plug-in (AudioServerPlugIn) with
// one device, "remoteman Microphone", that apps on this Mac can record from. It has a single
// input stream: mono, 48 kHz, 32-bit float. The host app (remoteman-host) hands it the
// connected device's microphone as samples through a custom property of the device ('rmpc',
// a CFData of Float32), and what it was handed is what an app reads; with nothing handed,
// silence. Written for remoteman against Apple's AudioServerPlugIn interface
// (CoreAudio/AudioServerPlugIn.h); it contains no code from any other driver.
//
// It lives in /Library/Audio/Plug-Ins/HAL and runs inside the system's audio server, so it
// keeps to itself: no files, no network, no allocation while audio runs.

#include <CoreAudio/AudioServerPlugIn.h>
#include <CoreFoundation/CoreFoundation.h>
#include <mach/mach_time.h>
#include <os/lock.h>
#include <stdatomic.h>
#include <string.h>

// MARK: What there is

enum {
    kObjectID_PlugIn = kAudioObjectPlugInObject,  // 1
    kObjectID_Device = 2,
    kObjectID_Stream = 3,
};

#define kDeviceUID "remoteman-microphone"
#define kDeviceModelUID "remoteman-microphone-model"
#define kDeviceName "remoteman Microphone"
#define kManufacturer "remoteman"
#define kSampleRate 48000.0
#define kZeroTimeStampPeriod 16384u  // frames between the clock's reference points
// The host app's samples, selector 'rmpc': set with a CFData of Float32, one channel, 48 kHz.
#define kProperty_Feed 0x726D7063u
// Samples waiting to be read: at most 100 ms; reading starts once 20 ms are there.
#define kFifoFrames 4800u
#define kPrimeFrames 960u

static AudioServerPlugInHostRef gHost = NULL;
static atomic_uint gRefCount = 1;
static os_unfair_lock gStateLock = OS_UNFAIR_LOCK_INIT;
static UInt32 gIORunning = 0;        // how many clients have IO going
static UInt64 gAnchorHostTime = 0;   // when the clock started
static UInt64 gPeriods = 0;          // reference points passed since then
static Float64 gHostTicksPerFrame = 0;
static bool gStreamActive = true;

static os_unfair_lock gFifoLock = OS_UNFAIR_LOCK_INIT;
static Float32 gFifo[kFifoFrames];
static UInt32 gFifoRead = 0, gFifoCount = 0;
static bool gFifoPrimed = false;

static AudioStreamBasicDescription Format(void) {
    AudioStreamBasicDescription f = {0};
    f.mSampleRate = kSampleRate;
    f.mFormatID = kAudioFormatLinearPCM;
    f.mFormatFlags = kAudioFormatFlagIsFloat | kAudioFormatFlagsNativeEndian | kAudioFormatFlagIsPacked;
    f.mBytesPerPacket = sizeof(Float32);
    f.mFramesPerPacket = 1;
    f.mBytesPerFrame = sizeof(Float32);
    f.mChannelsPerFrame = 1;
    f.mBitsPerChannel = 32;
    return f;
}

// MARK: The samples

static void FifoWrite(const Float32 *samples, UInt32 frames) {
    if (frames > kFifoFrames) {  // only the newest fit
        samples += frames - kFifoFrames;
        frames = kFifoFrames;
    }
    os_unfair_lock_lock(&gFifoLock);
    if (gFifoCount + frames > kFifoFrames) {  // the oldest make room
        UInt32 drop = gFifoCount + frames - kFifoFrames;
        gFifoRead = (gFifoRead + drop) % kFifoFrames;
        gFifoCount -= drop;
    }
    UInt32 at = (gFifoRead + gFifoCount) % kFifoFrames;
    for (UInt32 i = 0; i < frames; i++) {
        gFifo[at] = samples[i];
        at = (at + 1) % kFifoFrames;
    }
    gFifoCount += frames;
    os_unfair_lock_unlock(&gFifoLock);
}

static void FifoRead(Float32 *out, UInt32 frames) {
    UInt32 got = 0;
    os_unfair_lock_lock(&gFifoLock);
    if (!gFifoPrimed && gFifoCount >= kPrimeFrames) gFifoPrimed = true;
    if (gFifoPrimed) {
        got = frames < gFifoCount ? frames : gFifoCount;
        for (UInt32 i = 0; i < got; i++) {
            out[i] = gFifo[gFifoRead];
            gFifoRead = (gFifoRead + 1) % kFifoFrames;
        }
        gFifoCount -= got;
        if (got < frames) gFifoPrimed = false;  // ran dry: wait for a little to gather again
    }
    os_unfair_lock_unlock(&gFifoLock);
    if (got < frames) memset(out + got, 0, (frames - got) * sizeof(Float32));
}

static void FifoClear(void) {
    os_unfair_lock_lock(&gFifoLock);
    gFifoRead = 0;
    gFifoCount = 0;
    gFifoPrimed = false;
    os_unfair_lock_unlock(&gFifoLock);
}

// MARK: COM

static HRESULT QueryInterface(void *driver, REFIID iid, LPVOID *out);
static ULONG AddRef(void *driver);
static ULONG Release(void *driver);
static OSStatus Initialize(AudioServerPlugInDriverRef d, AudioServerPlugInHostRef host);
static OSStatus CreateDevice(AudioServerPlugInDriverRef d, CFDictionaryRef desc, const AudioServerPlugInClientInfo *c, AudioObjectID *out);
static OSStatus DestroyDevice(AudioServerPlugInDriverRef d, AudioObjectID device);
static OSStatus AddDeviceClient(AudioServerPlugInDriverRef d, AudioObjectID device, const AudioServerPlugInClientInfo *c);
static OSStatus RemoveDeviceClient(AudioServerPlugInDriverRef d, AudioObjectID device, const AudioServerPlugInClientInfo *c);
static OSStatus PerformDeviceConfigurationChange(AudioServerPlugInDriverRef d, AudioObjectID device, UInt64 action, void *info);
static OSStatus AbortDeviceConfigurationChange(AudioServerPlugInDriverRef d, AudioObjectID device, UInt64 action, void *info);
static Boolean HasProperty(AudioServerPlugInDriverRef d, AudioObjectID o, pid_t pid, const AudioObjectPropertyAddress *a);
static OSStatus IsPropertySettable(AudioServerPlugInDriverRef d, AudioObjectID o, pid_t pid, const AudioObjectPropertyAddress *a, Boolean *out);
static OSStatus GetPropertyDataSize(AudioServerPlugInDriverRef d, AudioObjectID o, pid_t pid, const AudioObjectPropertyAddress *a, UInt32 qSize, const void *q, UInt32 *out);
static OSStatus GetPropertyData(AudioServerPlugInDriverRef d, AudioObjectID o, pid_t pid, const AudioObjectPropertyAddress *a, UInt32 qSize, const void *q, UInt32 size, UInt32 *used, void *data);
static OSStatus SetPropertyData(AudioServerPlugInDriverRef d, AudioObjectID o, pid_t pid, const AudioObjectPropertyAddress *a, UInt32 qSize, const void *q, UInt32 size, const void *data);
static OSStatus StartIO(AudioServerPlugInDriverRef d, AudioObjectID device, UInt32 client);
static OSStatus StopIO(AudioServerPlugInDriverRef d, AudioObjectID device, UInt32 client);
static OSStatus GetZeroTimeStamp(AudioServerPlugInDriverRef d, AudioObjectID device, UInt32 client, Float64 *sampleTime, UInt64 *hostTime, UInt64 *seed);
static OSStatus WillDoIOOperation(AudioServerPlugInDriverRef d, AudioObjectID device, UInt32 client, UInt32 op, Boolean *will, Boolean *inPlace);
static OSStatus BeginIOOperation(AudioServerPlugInDriverRef d, AudioObjectID device, UInt32 client, UInt32 op, UInt32 frames, const AudioServerPlugInIOCycleInfo *cycle);
static OSStatus DoIOOperation(AudioServerPlugInDriverRef d, AudioObjectID device, AudioObjectID stream, UInt32 client, UInt32 op, UInt32 frames, const AudioServerPlugInIOCycleInfo *cycle, void *main, void *secondary);
static OSStatus EndIOOperation(AudioServerPlugInDriverRef d, AudioObjectID device, UInt32 client, UInt32 op, UInt32 frames, const AudioServerPlugInIOCycleInfo *cycle);

static AudioServerPlugInDriverInterface gInterface = {
    NULL,
    QueryInterface, AddRef, Release,
    Initialize, CreateDevice, DestroyDevice, AddDeviceClient, RemoveDeviceClient,
    PerformDeviceConfigurationChange, AbortDeviceConfigurationChange,
    HasProperty, IsPropertySettable, GetPropertyDataSize, GetPropertyData, SetPropertyData,
    StartIO, StopIO, GetZeroTimeStamp, WillDoIOOperation, BeginIOOperation, DoIOOperation, EndIOOperation,
};
static AudioServerPlugInDriverInterface *gInterfacePtr = &gInterface;
static AudioServerPlugInDriverRef gDriverRef = &gInterfacePtr;

// The factory named in Info.plist: the audio server calls it to get the driver.
void *RemotemanMicrophone_Create(CFAllocatorRef allocator, CFUUIDRef requestedType) {
    (void)allocator;
    if (!CFEqual(requestedType, kAudioServerPlugInTypeUUID)) return NULL;
    return gDriverRef;
}

static HRESULT QueryInterface(void *driver, REFIID iid, LPVOID *out) {
    if (driver != gDriverRef || out == NULL) return kAudioHardwareBadObjectError;
    CFUUIDRef requested = CFUUIDCreateFromUUIDBytes(NULL, iid);
    HRESULT result = E_NOINTERFACE;
    if (CFEqual(requested, IUnknownUUID) || CFEqual(requested, kAudioServerPlugInDriverInterfaceUUID)) {
        atomic_fetch_add(&gRefCount, 1);
        *out = gDriverRef;
        result = S_OK;
    }
    CFRelease(requested);
    return result;
}

static ULONG AddRef(void *driver) {
    if (driver != gDriverRef) return 0;
    return atomic_fetch_add(&gRefCount, 1) + 1;
}

static ULONG Release(void *driver) {
    if (driver != gDriverRef) return 0;
    UInt32 now = atomic_load(&gRefCount);
    if (now > 0) now = atomic_fetch_sub(&gRefCount, 1) - 1;
    return now;
}

// MARK: Setting up

static OSStatus Initialize(AudioServerPlugInDriverRef d, AudioServerPlugInHostRef host) {
    if (d != gDriverRef) return kAudioHardwareBadObjectError;
    gHost = host;
    struct mach_timebase_info tb;
    mach_timebase_info(&tb);
    Float64 ticksPerSecond = 1000000000.0 * (Float64)tb.denom / (Float64)tb.numer;
    gHostTicksPerFrame = ticksPerSecond / kSampleRate;
    return kAudioHardwareNoError;
}

// The one device is always there; none is made or taken away on request.
static OSStatus CreateDevice(AudioServerPlugInDriverRef d, CFDictionaryRef desc, const AudioServerPlugInClientInfo *c, AudioObjectID *out) {
    (void)d; (void)desc; (void)c; (void)out;
    return kAudioHardwareUnsupportedOperationError;
}
static OSStatus DestroyDevice(AudioServerPlugInDriverRef d, AudioObjectID device) {
    (void)d; (void)device;
    return kAudioHardwareUnsupportedOperationError;
}
static OSStatus AddDeviceClient(AudioServerPlugInDriverRef d, AudioObjectID device, const AudioServerPlugInClientInfo *c) {
    (void)c;
    return (d == gDriverRef && device == kObjectID_Device) ? kAudioHardwareNoError : kAudioHardwareBadObjectError;
}
static OSStatus RemoveDeviceClient(AudioServerPlugInDriverRef d, AudioObjectID device, const AudioServerPlugInClientInfo *c) {
    (void)c;
    return (d == gDriverRef && device == kObjectID_Device) ? kAudioHardwareNoError : kAudioHardwareBadObjectError;
}
// Nothing about the device can change (one rate, one format), so there is nothing to perform.
static OSStatus PerformDeviceConfigurationChange(AudioServerPlugInDriverRef d, AudioObjectID device, UInt64 action, void *info) {
    (void)action; (void)info;
    return (d == gDriverRef && device == kObjectID_Device) ? kAudioHardwareNoError : kAudioHardwareBadObjectError;
}
static OSStatus AbortDeviceConfigurationChange(AudioServerPlugInDriverRef d, AudioObjectID device, UInt64 action, void *info) {
    (void)action; (void)info;
    return (d == gDriverRef && device == kObjectID_Device) ? kAudioHardwareNoError : kAudioHardwareBadObjectError;
}

// MARK: Properties

static Boolean HasProperty(AudioServerPlugInDriverRef d, AudioObjectID o, pid_t pid, const AudioObjectPropertyAddress *a) {
    (void)pid;
    if (d != gDriverRef || a == NULL) return false;
    switch (o) {
        case kObjectID_PlugIn:
            switch (a->mSelector) {
                case kAudioObjectPropertyBaseClass: case kAudioObjectPropertyClass: case kAudioObjectPropertyOwner:
                case kAudioObjectPropertyManufacturer: case kAudioObjectPropertyOwnedObjects: case kAudioPlugInPropertyDeviceList:
                case kAudioPlugInPropertyTranslateUIDToDevice: case kAudioPlugInPropertyResourceBundle:
                    return true;
            }
            return false;
        case kObjectID_Device:
            switch (a->mSelector) {
                case kAudioObjectPropertyBaseClass: case kAudioObjectPropertyClass: case kAudioObjectPropertyOwner:
                case kAudioObjectPropertyName: case kAudioObjectPropertyManufacturer: case kAudioObjectPropertyOwnedObjects:
                case kAudioDevicePropertyDeviceUID: case kAudioDevicePropertyModelUID: case kAudioDevicePropertyTransportType:
                case kAudioDevicePropertyRelatedDevices: case kAudioDevicePropertyClockDomain: case kAudioDevicePropertyDeviceIsAlive:
                case kAudioDevicePropertyDeviceIsRunning: case kAudioDevicePropertyDeviceCanBeDefaultDevice:
                case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice: case kAudioDevicePropertyLatency: case kAudioDevicePropertyStreams:
                case kAudioObjectPropertyControlList: case kAudioDevicePropertySafetyOffset: case kAudioDevicePropertyNominalSampleRate:
                case kAudioDevicePropertyAvailableNominalSampleRates: case kAudioDevicePropertyIsHidden:
                case kAudioDevicePropertyZeroTimeStampPeriod: case kAudioObjectPropertyCustomPropertyInfoList: case kProperty_Feed:
                    return true;
            }
            return false;
        case kObjectID_Stream:
            switch (a->mSelector) {
                case kAudioObjectPropertyBaseClass: case kAudioObjectPropertyClass: case kAudioObjectPropertyOwner:
                case kAudioObjectPropertyOwnedObjects: case kAudioStreamPropertyIsActive: case kAudioStreamPropertyDirection:
                case kAudioStreamPropertyTerminalType: case kAudioStreamPropertyStartingChannel: case kAudioStreamPropertyLatency:
                case kAudioStreamPropertyVirtualFormat: case kAudioStreamPropertyPhysicalFormat:
                case kAudioStreamPropertyAvailableVirtualFormats: case kAudioStreamPropertyAvailablePhysicalFormats:
                    return true;
            }
            return false;
    }
    return false;
}

static OSStatus IsPropertySettable(AudioServerPlugInDriverRef d, AudioObjectID o, pid_t pid, const AudioObjectPropertyAddress *a, Boolean *out) {
    if (!HasProperty(d, o, pid, a)) return kAudioHardwareUnknownPropertyError;
    if (out == NULL) return kAudioHardwareIllegalOperationError;
    *out = false;
    if (o == kObjectID_Device && (a->mSelector == kAudioDevicePropertyNominalSampleRate || a->mSelector == kProperty_Feed)) *out = true;
    if (o == kObjectID_Stream && (a->mSelector == kAudioStreamPropertyIsActive || a->mSelector == kAudioStreamPropertyVirtualFormat ||
                                  a->mSelector == kAudioStreamPropertyPhysicalFormat)) *out = true;
    return kAudioHardwareNoError;
}

// The device has one stream and it is an input: other scopes own nothing.
static UInt32 StreamCount(AudioObjectPropertyScope scope) {
    return (scope == kAudioObjectPropertyScopeGlobal || scope == kAudioObjectPropertyScopeInput) ? 1 : 0;
}

static OSStatus GetPropertyDataSize(AudioServerPlugInDriverRef d, AudioObjectID o, pid_t pid, const AudioObjectPropertyAddress *a, UInt32 qSize, const void *q, UInt32 *out) {
    (void)qSize; (void)q;
    if (!HasProperty(d, o, pid, a)) return kAudioHardwareUnknownPropertyError;
    if (out == NULL) return kAudioHardwareIllegalOperationError;
    switch (a->mSelector) {
        case kAudioObjectPropertyBaseClass: case kAudioObjectPropertyClass: *out = sizeof(AudioClassID); break;
        case kAudioObjectPropertyOwner: *out = sizeof(AudioObjectID); break;
        case kAudioObjectPropertyName: case kAudioObjectPropertyManufacturer: case kAudioDevicePropertyDeviceUID:
        case kAudioDevicePropertyModelUID: case kAudioPlugInPropertyResourceBundle:
            *out = sizeof(CFStringRef); break;
        case kAudioObjectPropertyOwnedObjects:
            if (o == kObjectID_PlugIn) *out = sizeof(AudioObjectID);
            else if (o == kObjectID_Device) *out = StreamCount(a->mScope) * sizeof(AudioObjectID);
            else *out = 0;
            break;
        case kAudioPlugInPropertyDeviceList: case kAudioPlugInPropertyTranslateUIDToDevice: case kAudioDevicePropertyRelatedDevices:
            *out = sizeof(AudioObjectID); break;
        case kAudioDevicePropertyStreams: *out = StreamCount(a->mScope) * sizeof(AudioObjectID); break;
        case kAudioObjectPropertyControlList: *out = 0; break;
        case kAudioDevicePropertyNominalSampleRate: *out = sizeof(Float64); break;
        case kAudioDevicePropertyAvailableNominalSampleRates: *out = sizeof(AudioValueRange); break;
        case kAudioObjectPropertyCustomPropertyInfoList: *out = sizeof(AudioServerPlugInCustomPropertyInfo); break;
        case kProperty_Feed: *out = sizeof(CFPropertyListRef); break;
        case kAudioStreamPropertyVirtualFormat: case kAudioStreamPropertyPhysicalFormat: *out = sizeof(AudioStreamBasicDescription); break;
        case kAudioStreamPropertyAvailableVirtualFormats: case kAudioStreamPropertyAvailablePhysicalFormats:
            *out = sizeof(AudioStreamRangedDescription); break;
        default: *out = sizeof(UInt32); break;  // every other property here is one UInt32
    }
    return kAudioHardwareNoError;
}

#define RETURN_VALUE(type, value) \
    do { \
        if (size < sizeof(type)) return kAudioHardwareBadPropertySizeError; \
        *(type *)data = (value); \
        *used = sizeof(type); \
        return kAudioHardwareNoError; \
    } while (0)

static OSStatus GetPropertyData(AudioServerPlugInDriverRef d, AudioObjectID o, pid_t pid, const AudioObjectPropertyAddress *a, UInt32 qSize, const void *q, UInt32 size, UInt32 *used, void *data) {
    if (!HasProperty(d, o, pid, a)) return kAudioHardwareUnknownPropertyError;
    if (used == NULL || data == NULL) return kAudioHardwareIllegalOperationError;
    if (o == kObjectID_PlugIn) {
        switch (a->mSelector) {
            case kAudioObjectPropertyBaseClass: RETURN_VALUE(AudioClassID, kAudioObjectClassID);
            case kAudioObjectPropertyClass: RETURN_VALUE(AudioClassID, kAudioPlugInClassID);
            case kAudioObjectPropertyOwner: RETURN_VALUE(AudioObjectID, kAudioObjectUnknown);
            case kAudioObjectPropertyManufacturer: RETURN_VALUE(CFStringRef, CFSTR(kManufacturer));
            case kAudioPlugInPropertyResourceBundle: RETURN_VALUE(CFStringRef, CFSTR(""));
            case kAudioObjectPropertyOwnedObjects: case kAudioPlugInPropertyDeviceList:
                if (size < sizeof(AudioObjectID)) { *used = 0; return kAudioHardwareNoError; }
                RETURN_VALUE(AudioObjectID, kObjectID_Device);
            case kAudioPlugInPropertyTranslateUIDToDevice: {
                if (qSize != sizeof(CFStringRef) || q == NULL) return kAudioHardwareBadPropertySizeError;
                CFStringRef uid = *(const CFStringRef *)q;
                RETURN_VALUE(AudioObjectID, (uid && CFEqual(uid, CFSTR(kDeviceUID))) ? kObjectID_Device : kAudioObjectUnknown);
            }
        }
    } else if (o == kObjectID_Device) {
        switch (a->mSelector) {
            case kAudioObjectPropertyBaseClass: RETURN_VALUE(AudioClassID, kAudioObjectClassID);
            case kAudioObjectPropertyClass: RETURN_VALUE(AudioClassID, kAudioDeviceClassID);
            case kAudioObjectPropertyOwner: RETURN_VALUE(AudioObjectID, kObjectID_PlugIn);
            case kAudioObjectPropertyName: RETURN_VALUE(CFStringRef, CFSTR(kDeviceName));
            case kAudioObjectPropertyManufacturer: RETURN_VALUE(CFStringRef, CFSTR(kManufacturer));
            case kAudioDevicePropertyDeviceUID: RETURN_VALUE(CFStringRef, CFSTR(kDeviceUID));
            case kAudioDevicePropertyModelUID: RETURN_VALUE(CFStringRef, CFSTR(kDeviceModelUID));
            case kAudioDevicePropertyTransportType: RETURN_VALUE(UInt32, kAudioDeviceTransportTypeVirtual);
            case kAudioDevicePropertyRelatedDevices:
                if (size < sizeof(AudioObjectID)) { *used = 0; return kAudioHardwareNoError; }
                RETURN_VALUE(AudioObjectID, kObjectID_Device);
            case kAudioDevicePropertyClockDomain: RETURN_VALUE(UInt32, 0);
            case kAudioDevicePropertyDeviceIsAlive: RETURN_VALUE(UInt32, 1);
            case kAudioDevicePropertyDeviceIsRunning: {
                os_unfair_lock_lock(&gStateLock);
                UInt32 running = gIORunning > 0 ? 1 : 0;
                os_unfair_lock_unlock(&gStateLock);
                RETURN_VALUE(UInt32, running);
            }
            // It may be chosen as the Mac's input; it is never the device alerts play through.
            case kAudioDevicePropertyDeviceCanBeDefaultDevice: RETURN_VALUE(UInt32, a->mScope == kAudioObjectPropertyScopeOutput ? 0 : 1);
            case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice: RETURN_VALUE(UInt32, 0);
            case kAudioDevicePropertyLatency: RETURN_VALUE(UInt32, 0);
            case kAudioDevicePropertySafetyOffset: RETURN_VALUE(UInt32, 0);
            case kAudioDevicePropertyIsHidden: RETURN_VALUE(UInt32, 0);
            case kAudioDevicePropertyZeroTimeStampPeriod: RETURN_VALUE(UInt32, kZeroTimeStampPeriod);
            case kAudioObjectPropertyOwnedObjects: case kAudioDevicePropertyStreams:
                if (StreamCount(a->mScope) == 0 || size < sizeof(AudioObjectID)) { *used = 0; return kAudioHardwareNoError; }
                RETURN_VALUE(AudioObjectID, kObjectID_Stream);
            case kAudioObjectPropertyControlList: *used = 0; return kAudioHardwareNoError;
            case kAudioDevicePropertyNominalSampleRate: RETURN_VALUE(Float64, kSampleRate);
            case kAudioDevicePropertyAvailableNominalSampleRates: {
                if (size < sizeof(AudioValueRange)) { *used = 0; return kAudioHardwareNoError; }
                AudioValueRange r = {kSampleRate, kSampleRate};
                RETURN_VALUE(AudioValueRange, r);
            }
            case kAudioObjectPropertyCustomPropertyInfoList: {
                if (size < sizeof(AudioServerPlugInCustomPropertyInfo)) { *used = 0; return kAudioHardwareNoError; }
                AudioServerPlugInCustomPropertyInfo info = {kProperty_Feed, kAudioServerPlugInCustomPropertyDataTypeCFPropertyList,
                                                            kAudioServerPlugInCustomPropertyDataTypeNone};
                RETURN_VALUE(AudioServerPlugInCustomPropertyInfo, info);
            }
            // Reading the feed gives nothing back: what was handed in is only for apps to record.
            case kProperty_Feed: RETURN_VALUE(CFPropertyListRef, CFDataCreate(NULL, NULL, 0));
        }
    } else if (o == kObjectID_Stream) {
        switch (a->mSelector) {
            case kAudioObjectPropertyBaseClass: RETURN_VALUE(AudioClassID, kAudioObjectClassID);
            case kAudioObjectPropertyClass: RETURN_VALUE(AudioClassID, kAudioStreamClassID);
            case kAudioObjectPropertyOwner: RETURN_VALUE(AudioObjectID, kObjectID_Device);
            case kAudioObjectPropertyOwnedObjects: *used = 0; return kAudioHardwareNoError;
            case kAudioStreamPropertyIsActive: {
                os_unfair_lock_lock(&gStateLock);
                UInt32 active = gStreamActive ? 1 : 0;
                os_unfair_lock_unlock(&gStateLock);
                RETURN_VALUE(UInt32, active);
            }
            case kAudioStreamPropertyDirection: RETURN_VALUE(UInt32, 1);  // an input
            case kAudioStreamPropertyTerminalType: RETURN_VALUE(UInt32, kAudioStreamTerminalTypeMicrophone);
            case kAudioStreamPropertyStartingChannel: RETURN_VALUE(UInt32, 1);
            case kAudioStreamPropertyLatency: RETURN_VALUE(UInt32, 0);
            case kAudioStreamPropertyVirtualFormat: case kAudioStreamPropertyPhysicalFormat:
                RETURN_VALUE(AudioStreamBasicDescription, Format());
            case kAudioStreamPropertyAvailableVirtualFormats: case kAudioStreamPropertyAvailablePhysicalFormats: {
                if (size < sizeof(AudioStreamRangedDescription)) { *used = 0; return kAudioHardwareNoError; }
                AudioStreamRangedDescription r;
                r.mFormat = Format();
                r.mSampleRateRange.mMinimum = kSampleRate;
                r.mSampleRateRange.mMaximum = kSampleRate;
                RETURN_VALUE(AudioStreamRangedDescription, r);
            }
        }
    }
    return kAudioHardwareUnknownPropertyError;
}

static OSStatus SetPropertyData(AudioServerPlugInDriverRef d, AudioObjectID o, pid_t pid, const AudioObjectPropertyAddress *a, UInt32 qSize, const void *q, UInt32 size, const void *data) {
    (void)qSize; (void)q;
    if (!HasProperty(d, o, pid, a)) return kAudioHardwareUnknownPropertyError;
    if (data == NULL) return kAudioHardwareIllegalOperationError;
    if (o == kObjectID_Device && a->mSelector == kProperty_Feed) {
        if (size != sizeof(CFPropertyListRef)) return kAudioHardwareBadPropertySizeError;
        CFPropertyListRef value = *(const CFPropertyListRef *)data;
        if (value == NULL || CFGetTypeID(value) != CFDataGetTypeID()) return kAudioHardwareIllegalOperationError;
        CFIndex bytes = CFDataGetLength((CFDataRef)value);
        // An empty feed says the device's microphone went off: what is waiting is let go.
        if (bytes == 0) FifoClear();
        else FifoWrite((const Float32 *)CFDataGetBytePtr((CFDataRef)value), (UInt32)(bytes / sizeof(Float32)));
        return kAudioHardwareNoError;
    }
    if (o == kObjectID_Device && a->mSelector == kAudioDevicePropertyNominalSampleRate) {
        if (size != sizeof(Float64)) return kAudioHardwareBadPropertySizeError;
        return *(const Float64 *)data == kSampleRate ? kAudioHardwareNoError : kAudioHardwareIllegalOperationError;
    }
    if (o == kObjectID_Stream && a->mSelector == kAudioStreamPropertyIsActive) {
        if (size != sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
        os_unfair_lock_lock(&gStateLock);
        gStreamActive = *(const UInt32 *)data != 0;
        os_unfair_lock_unlock(&gStateLock);
        return kAudioHardwareNoError;
    }
    if (o == kObjectID_Stream && (a->mSelector == kAudioStreamPropertyVirtualFormat || a->mSelector == kAudioStreamPropertyPhysicalFormat)) {
        if (size != sizeof(AudioStreamBasicDescription)) return kAudioHardwareBadPropertySizeError;
        const AudioStreamBasicDescription *f = data;
        AudioStreamBasicDescription mine = Format();
        // The one format, or nothing.
        return (f->mSampleRate == mine.mSampleRate && f->mFormatID == mine.mFormatID && f->mChannelsPerFrame == mine.mChannelsPerFrame &&
                f->mBitsPerChannel == mine.mBitsPerChannel)
            ? kAudioHardwareNoError : kAudioDeviceUnsupportedFormatError;
    }
    return kAudioHardwareUnsupportedOperationError;
}

// MARK: Running

static OSStatus StartIO(AudioServerPlugInDriverRef d, AudioObjectID device, UInt32 client) {
    (void)client;
    if (d != gDriverRef || device != kObjectID_Device) return kAudioHardwareBadObjectError;
    os_unfair_lock_lock(&gStateLock);
    if (gIORunning == 0) {  // the first: the clock starts now
        gPeriods = 0;
        gAnchorHostTime = mach_absolute_time();
    }
    gIORunning += 1;
    os_unfair_lock_unlock(&gStateLock);
    return kAudioHardwareNoError;
}

static OSStatus StopIO(AudioServerPlugInDriverRef d, AudioObjectID device, UInt32 client) {
    (void)client;
    if (d != gDriverRef || device != kObjectID_Device) return kAudioHardwareBadObjectError;
    os_unfair_lock_lock(&gStateLock);
    if (gIORunning > 0) gIORunning -= 1;
    os_unfair_lock_unlock(&gStateLock);
    return kAudioHardwareNoError;
}

// The device's clock: a reference point every kZeroTimeStampPeriod frames, counted off the
// Mac's own clock from the moment IO started.
static OSStatus GetZeroTimeStamp(AudioServerPlugInDriverRef d, AudioObjectID device, UInt32 client, Float64 *sampleTime, UInt64 *hostTime, UInt64 *seed) {
    (void)client;
    if (d != gDriverRef || device != kObjectID_Device) return kAudioHardwareBadObjectError;
    os_unfair_lock_lock(&gStateLock);
    UInt64 now = mach_absolute_time();
    Float64 ticksPerPeriod = gHostTicksPerFrame * (Float64)kZeroTimeStampPeriod;
    UInt64 next = gAnchorHostTime + (UInt64)((Float64)(gPeriods + 1) * ticksPerPeriod);
    if (next <= now) gPeriods += 1;
    *sampleTime = (Float64)(gPeriods * kZeroTimeStampPeriod);
    *hostTime = gAnchorHostTime + (UInt64)((Float64)gPeriods * ticksPerPeriod);
    *seed = 1;
    os_unfair_lock_unlock(&gStateLock);
    return kAudioHardwareNoError;
}

static OSStatus WillDoIOOperation(AudioServerPlugInDriverRef d, AudioObjectID device, UInt32 client, UInt32 op, Boolean *will, Boolean *inPlace) {
    (void)client;
    if (d != gDriverRef || device != kObjectID_Device) return kAudioHardwareBadObjectError;
    if (will) *will = op == kAudioServerPlugInIOOperationReadInput;
    if (inPlace) *inPlace = true;
    return kAudioHardwareNoError;
}

static OSStatus BeginIOOperation(AudioServerPlugInDriverRef d, AudioObjectID device, UInt32 client, UInt32 op, UInt32 frames, const AudioServerPlugInIOCycleInfo *cycle) {
    (void)client; (void)op; (void)frames; (void)cycle;
    return (d == gDriverRef && device == kObjectID_Device) ? kAudioHardwareNoError : kAudioHardwareBadObjectError;
}

static OSStatus DoIOOperation(AudioServerPlugInDriverRef d, AudioObjectID device, AudioObjectID stream, UInt32 client, UInt32 op, UInt32 frames, const AudioServerPlugInIOCycleInfo *cycle, void *main, void *secondary) {
    (void)client; (void)cycle; (void)secondary;
    if (d != gDriverRef || device != kObjectID_Device || stream != kObjectID_Stream) return kAudioHardwareBadObjectError;
    if (op == kAudioServerPlugInIOOperationReadInput && main != NULL) FifoRead((Float32 *)main, frames);
    return kAudioHardwareNoError;
}

static OSStatus EndIOOperation(AudioServerPlugInDriverRef d, AudioObjectID device, UInt32 client, UInt32 op, UInt32 frames, const AudioServerPlugInIOCycleInfo *cycle) {
    (void)client; (void)op; (void)frames; (void)cycle;
    return (d == gDriverRef && device == kObjectID_Device) ? kAudioHardwareNoError : kAudioHardwareBadObjectError;
}
