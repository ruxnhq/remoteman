// Loads the microphone driver's binary in this process (nothing is installed, the audio server
// is not involved) and walks it the way the audio server does: the objects and their
// properties, then IO: hand samples in through the feed property and read them back.
#include <CoreAudio/AudioServerPlugIn.h>
#include <CoreFoundation/CoreFoundation.h>
#include <dlfcn.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>

static int failures = 0;
#define CHECK(what, ok) do { if (!(ok)) { failures++; printf("FAIL  %s\n", what); } else printf("ok    %s\n", what); } while (0)

int main(int argc, char **argv) {
    if (argc < 2) return 2;
    void *lib = dlopen(argv[1], RTLD_NOW);
    if (!lib) { printf("dlopen: %s\n", dlerror()); return 1; }
    void *(*create)(CFAllocatorRef, CFUUIDRef) = dlsym(lib, "RemotemanMicrophone_Create");
    CHECK("the factory is there", create != NULL);
    if (!create) return 1;
    CHECK("another type is refused", create(NULL, IUnknownUUID) == NULL);
    AudioServerPlugInDriverRef d = create(NULL, kAudioServerPlugInTypeUUID);
    CHECK("the factory gives a driver", d != NULL);
    void *again = NULL;
    CHECK("QueryInterface for the driver interface", (*d)->QueryInterface(d, CFUUIDGetUUIDBytes(kAudioServerPlugInDriverInterfaceUUID), &again) == S_OK && again == d);
    CHECK("Initialize", (*d)->Initialize(d, NULL) == 0);

    AudioObjectPropertyAddress a = {kAudioPlugInPropertyDeviceList, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
    UInt32 used = 0;
    AudioObjectID device = 0, stream = 0;
    CHECK("the plug-in lists one device", (*d)->GetPropertyData(d, kAudioObjectPlugInObject, 0, &a, 0, NULL, sizeof(device), &used, &device) == 0 && device != 0);
    CFStringRef uid = CFSTR("remoteman-microphone");
    AudioObjectID found = 0;
    a.mSelector = kAudioPlugInPropertyTranslateUIDToDevice;
    CHECK("the device is found by its UID", (*d)->GetPropertyData(d, kAudioObjectPlugInObject, 0, &a, sizeof(uid), &uid, sizeof(found), &used, &found) == 0 && found == device);
    CFStringRef name = NULL;
    a.mSelector = kAudioObjectPropertyName;
    CHECK("its name", (*d)->GetPropertyData(d, device, 0, &a, 0, NULL, sizeof(name), &used, &name) == 0 && name && CFEqual(name, CFSTR("remoteman Microphone")));
    a.mSelector = kAudioDevicePropertyStreams; a.mScope = kAudioObjectPropertyScopeInput;
    CHECK("one input stream", (*d)->GetPropertyData(d, device, 0, &a, 0, NULL, sizeof(stream), &used, &stream) == 0 && used == sizeof(stream) && stream != 0);
    a.mScope = kAudioObjectPropertyScopeOutput;
    UInt32 size = 99;
    CHECK("no output stream", (*d)->GetPropertyDataSize(d, device, 0, &a, 0, NULL, &size) == 0 && size == 0);
    a.mScope = kAudioObjectPropertyScopeGlobal;
    UInt32 v = 0;
    a.mSelector = kAudioDevicePropertyDeviceCanBeDefaultDevice; a.mScope = kAudioObjectPropertyScopeInput;
    CHECK("may be the default input", (*d)->GetPropertyData(d, device, 0, &a, 0, NULL, sizeof(v), &used, &v) == 0 && v == 1);
    a.mScope = kAudioObjectPropertyScopeGlobal;
    Float64 rate = 0;
    a.mSelector = kAudioDevicePropertyNominalSampleRate;
    CHECK("48 kHz", (*d)->GetPropertyData(d, device, 0, &a, 0, NULL, sizeof(rate), &used, &rate) == 0 && rate == 48000);
    rate = 44100;
    CHECK("another rate is refused", (*d)->SetPropertyData(d, device, 0, &a, 0, NULL, sizeof(rate), &rate) != 0);
    AudioStreamBasicDescription f = {0};
    a.mSelector = kAudioStreamPropertyVirtualFormat;
    CHECK("mono 32-bit float", (*d)->GetPropertyData(d, stream, 0, &a, 0, NULL, sizeof(f), &used, &f) == 0 && f.mChannelsPerFrame == 1 && f.mBitsPerChannel == 32 && (f.mFormatFlags & kAudioFormatFlagIsFloat));
    a.mSelector = kAudioStreamPropertyDirection;
    CHECK("the stream is an input", (*d)->GetPropertyData(d, stream, 0, &a, 0, NULL, sizeof(v), &used, &v) == 0 && v == 1);
    AudioServerPlugInCustomPropertyInfo info = {0};
    a.mSelector = kAudioObjectPropertyCustomPropertyInfoList;
    CHECK("the feed property is announced", (*d)->GetPropertyData(d, device, 0, &a, 0, NULL, sizeof(info), &used, &info) == 0 && info.mSelector == 0x726D7063);
    // Every property the device and stream say they have gives a size and a value of that size.
    AudioObjectPropertySelector all[] = {kAudioObjectPropertyBaseClass, kAudioObjectPropertyClass, kAudioObjectPropertyOwner, kAudioObjectPropertyManufacturer,
        kAudioDevicePropertyDeviceUID, kAudioDevicePropertyModelUID, kAudioDevicePropertyTransportType, kAudioDevicePropertyRelatedDevices,
        kAudioDevicePropertyClockDomain, kAudioDevicePropertyDeviceIsAlive, kAudioDevicePropertyDeviceIsRunning, kAudioDevicePropertyLatency,
        kAudioDevicePropertySafetyOffset, kAudioDevicePropertyAvailableNominalSampleRates, kAudioDevicePropertyIsHidden, kAudioDevicePropertyZeroTimeStampPeriod,
        kAudioObjectPropertyControlList, kAudioObjectPropertyOwnedObjects};
    int consistent = 1;
    for (unsigned i = 0; i < sizeof(all) / sizeof(all[0]); i++) {
        AudioObjectPropertyAddress p = {all[i], kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
        char buf[256]; UInt32 want = 0, got = 0;
        if (!(*d)->HasProperty(d, device, 0, &p)) { consistent = 0; printf("      missing %08x\n", (unsigned)all[i]); continue; }
        if ((*d)->GetPropertyDataSize(d, device, 0, &p, 0, NULL, &want) != 0 || (*d)->GetPropertyData(d, device, 0, &p, 0, NULL, sizeof(buf), &got, buf) != 0 || got != want) {
            consistent = 0; printf("      %08x: size %u, got %u\n", (unsigned)all[i], want, got);
        }
    }
    CHECK("device properties: sizes match values", consistent);
    AudioObjectPropertyAddress bogus = {0x7a7a7a7a, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
    CHECK("an unknown property is refused", !(*d)->HasProperty(d, device, 0, &bogus) && (*d)->GetPropertyDataSize(d, device, 0, &bogus, 0, NULL, &size) != 0);

    // IO. Before anything is fed: silence.
    CHECK("StartIO", (*d)->StartIO(d, device, 1) == 0);
    a.mSelector = kAudioDevicePropertyDeviceIsRunning;
    CHECK("running while IO is on", (*d)->GetPropertyData(d, device, 0, &a, 0, NULL, sizeof(v), &used, &v) == 0 && v == 1);
    Float64 st = -1; UInt64 ht = 0, seed = 0;
    CHECK("the clock answers", (*d)->GetZeroTimeStamp(d, device, 1, &st, &ht, &seed) == 0 && st == 0 && ht != 0);
    Boolean will = 0, inPlace = 0;
    CHECK("it does the read-input step", (*d)->WillDoIOOperation(d, device, 1, kAudioServerPlugInIOOperationReadInput, &will, &inPlace) == 0 && will);
    Float32 out[512];
    for (int i = 0; i < 512; i++) out[i] = 9;
    (*d)->DoIOOperation(d, device, stream, 1, kAudioServerPlugInIOOperationReadInput, 512, NULL, out, NULL);
    int silent = 1;
    for (int i = 0; i < 512; i++) if (out[i] != 0) silent = 0;
    CHECK("silence when nothing was fed", silent);

    // Feed 100 ms of a 440 Hz tone in 10 ms pieces, as the host app does, and read it back.
    AudioObjectPropertyAddress feed = {0x726D7063, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
    Float32 tone[4800];
    for (int i = 0; i < 4800; i++) tone[i] = 0.25f * sinf(2 * (float)M_PI * 440 * i / 48000);
    int fed = 1;
    for (int k = 0; k < 10; k++) {
        CFDataRef piece = CFDataCreate(NULL, (const UInt8 *)(tone + k * 480), 480 * sizeof(Float32));
        if ((*d)->SetPropertyData(d, device, 0, &feed, 0, NULL, sizeof(piece), &piece) != 0) fed = 0;
        CFRelease(piece);
    }
    CHECK("the feed takes samples", fed);
    Float32 back[4800];
    for (int k = 0; k < 4800 / 480; k++)
        (*d)->DoIOOperation(d, device, stream, 1, kAudioServerPlugInIOOperationReadInput, 480, NULL, back + k * 480, NULL);
    int same = 1;
    for (int i = 0; i < 4800; i++) if (back[i] != tone[i]) { same = 0; break; }
    CHECK("what was fed is what is read, sample for sample", same);
    (*d)->DoIOOperation(d, device, stream, 1, kAudioServerPlugInIOOperationReadInput, 512, NULL, out, NULL);
    silent = 1;
    for (int i = 0; i < 512; i++) if (out[i] != 0) silent = 0;
    CHECK("silence again once it has all been read", silent);
    // Too much at once: only the newest 100 ms are kept.
    Float32 *lots = malloc(9600 * sizeof(Float32));
    for (int i = 0; i < 9600; i++) lots[i] = (Float32)i;
    CFDataRef big = CFDataCreate(NULL, (const UInt8 *)lots, 9600 * sizeof(Float32));
    (*d)->SetPropertyData(d, device, 0, &feed, 0, NULL, sizeof(big), &big);
    CFRelease(big);
    (*d)->DoIOOperation(d, device, stream, 1, kAudioServerPlugInIOOperationReadInput, 480, NULL, back, NULL);
    CHECK("an overfull feed keeps the newest 100 ms", back[0] == 4800 && back[479] == 5279);
    // An empty feed lets go of what waits.
    CFDataRef empty = CFDataCreate(NULL, NULL, 0);
    (*d)->SetPropertyData(d, device, 0, &feed, 0, NULL, sizeof(empty), &empty);
    CFRelease(empty);
    (*d)->DoIOOperation(d, device, stream, 1, kAudioServerPlugInIOOperationReadInput, 480, NULL, back, NULL);
    silent = 1;
    for (int i = 0; i < 480; i++) if (back[i] != 0) silent = 0;
    CHECK("an empty feed clears what waits", silent);
    CFStringRef wrong = CFSTR("not data");
    CHECK("a feed that isn't data is refused", (*d)->SetPropertyData(d, device, 0, &feed, 0, NULL, sizeof(wrong), &wrong) != 0);
    CHECK("StopIO", (*d)->StopIO(d, device, 1) == 0);
    a.mSelector = kAudioDevicePropertyDeviceIsRunning;
    CHECK("not running after IO stops", (*d)->GetPropertyData(d, device, 0, &a, 0, NULL, sizeof(v), &used, &v) == 0 && v == 0);
    printf("%s (%d failed)\n", failures ? "SELF-TEST FAILED" : "self-test passed", failures);
    return failures ? 1 : 0;
}
