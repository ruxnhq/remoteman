//! The media source: what the Frame Server loads when a program opens "remoteman camera".
//!
//! Built after Microsoft's VirtualCamera sample (Windows-Camera, SimpleMediaSource): one
//! stream (NV12, [`WIDTH`]x[`HEIGHT`] at [`FPS`]) on the capture pin, marked as shared by the
//! Frame Server; `RequestSample` answered with the newest picture in the ring, or black.
//!
//! The one COM class is both the media source (`IMFMediaSourceEx`, with `IMFGetService` and
//! `IKsControl`) and an `IMFActivate` that hands out itself: the Frame Server has been seen to ask
//! a registered class for either, and this answers both.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{E_NOTIMPL, S_OK};
use windows::Win32::Media::KernelStreaming::{IKsControl, IKsControl_Impl, KSIDENTIFIER, PINNAME_VIDEO_CAPTURE};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::core::{BOOL, GUID, HRESULT, IUnknown, Interface, PCWSTR, PWSTR, Ref, Result, implement};
use windows_core::IUnknownImpl;

use crate::ring::{self, Ring};
use crate::shm::{self, Mapping};

/// The one picture size and rate on offer.
pub const WIDTH: u32 = 1280;
pub const HEIGHT: u32 = 720;
pub const FPS: u32 = 30;

/// Objects alive in this DLL (for `DllCanUnloadNow`).
pub static LIVE: AtomicI32 = AtomicI32::new(0);

fn err<T>(hr: HRESULT) -> Result<T> {
    Err(hr.into())
}

fn set_not_found() -> windows::core::Error {
    HRESULT::from_win32(windows::Win32::Foundation::ERROR_SET_NOT_FOUND.0).into()
}

fn pack(hi: u32, lo: u32) -> u64 {
    (hi as u64) << 32 | lo as u64
}

// ---------------------------------------------------------------------------------------------
// Pictures: the ring, or black.

/// Where the stream gets its pictures.
struct Pictures {
    mapping: Option<Mapping>,
    ring: Option<Ring>,
    tried: Option<Instant>,
    black: Vec<u8>,
    current: Vec<u8>,
}

impl Pictures {
    fn new() -> Pictures {
        let n = ring::picture_bytes(WIDTH, HEIGHT);
        let mut black = vec![128u8; n];
        black[..(WIDTH * HEIGHT) as usize].fill(16);
        Pictures { mapping: None, ring: None, tried: None, current: black.clone(), black }
    }

    /// The picture to send now.
    fn next(&mut self) -> &[u8] {
        let now = shm::now_ms();
        // Attach (again, now and then, so that a host that came after, or made another ring, is
        // found).
        if self.tried.is_none_or(|t| t.elapsed() > Duration::from_millis(1000)) {
            self.tried = Some(Instant::now());
            let usable = self.ring.as_ref().is_some_and(|r| r.width() == WIDTH && r.height() == HEIGHT && r.live(now));
            if !usable {
                self.ring = None;
                self.mapping = None;
                if let Ok(mapping) = Mapping::open_ring() {
                    // SAFETY: the mapping lives as long as the ring (both are dropped together).
                    match unsafe { Ring::open(mapping.ptr(), mapping.len()) } {
                        Ok(r) if r.width() == WIDTH && r.height() == HEIGHT => {
                            crate::log("the ring is attached");
                            self.ring = Some(r);
                            self.mapping = Some(mapping);
                        }
                        _ => {}
                    }
                }
            }
        }
        match &self.ring {
            Some(r) if r.live(now) => {
                let _ = r.read(&mut self.current);
                &self.current
            }
            _ => {
                self.current.copy_from_slice(&self.black);
                &self.current
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The stream.

struct Inner {
    running: bool,
    shutdown: bool,
    tokens: VecDeque<Option<IUnknown>>,
    state: MF_STREAM_STATE,
    thread: bool,
}

struct StreamCore {
    queue: IMFMediaEventQueue,
    descriptor: IMFStreamDescriptor,
    source: Mutex<Option<IMFMediaSource>>,
    inner: Mutex<Inner>,
    cv: Condvar,
}

impl StreamCore {
    fn queue_var(&self, met: MF_EVENT_TYPE) {
        let value = PROPVARIANT::default();
        unsafe {
            let _ = self.queue.QueueEventParamVar(met.0 as u32, &GUID::zeroed(), S_OK, &value);
        }
    }

    /// The source was started: begin answering requests.
    fn start(self: &Arc<Self>) {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.running = true;
            inner.state = MF_STREAM_STATE_RUNNING;
            if !inner.thread {
                inner.thread = true;
                let core = self.clone();
                std::thread::Builder::new().name("vcam-stream".into()).spawn(move || core.run()).ok();
            }
        }
        self.cv.notify_all();
        self.queue_var(MEStreamStarted);
    }

    fn stop(&self) {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.running = false;
            inner.state = MF_STREAM_STATE_STOPPED;
            inner.tokens.clear();
        }
        self.queue_var(MEStreamStopped);
    }

    fn shutdown(&self) {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.shutdown = true;
            inner.running = false;
            inner.tokens.clear();
        }
        self.cv.notify_all();
        unsafe {
            let _ = self.queue.Shutdown();
        }
        *self.source.lock().unwrap() = None;
    }

    /// The thread that answers `RequestSample`: one picture per request, no faster than the frame
    /// rate.
    fn run(self: Arc<Self>) {
        let interval = Duration::from_micros(1_000_000 / FPS as u64);
        let mut pictures = Pictures::new();
        let mut next = Instant::now();
        loop {
            let token = {
                let mut inner = self.inner.lock().unwrap();
                loop {
                    if inner.shutdown {
                        return;
                    }
                    if inner.running {
                        if let Some(token) = inner.tokens.pop_front() {
                            break token;
                        }
                    }
                    inner = self.cv.wait(inner).unwrap();
                }
            };
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else if now - next > interval * 2 {
                next = now;
            }
            next += interval;
            if let Err(e) = self.send(pictures.next(), token) {
                crate::log(&format!("a sample couldn't be sent: {e}"));
            }
        }
    }

    fn send(&self, picture: &[u8], token: Option<IUnknown>) -> Result<()> {
        unsafe {
            let buffer = MFCreateMemoryBuffer(picture.len() as u32)?;
            let mut at = std::ptr::null_mut();
            buffer.Lock(&mut at, None, None)?;
            std::ptr::copy_nonoverlapping(picture.as_ptr(), at, picture.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(picture.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(MFGetSystemTime())?;
            sample.SetSampleDuration(10_000_000 / FPS as i64)?;
            if let Some(token) = &token {
                sample.SetUnknown(&MFSampleExtension_Token, token)?;
            }
            self.queue.QueueEventParamUnk(MEMediaSample.0 as u32, &GUID::zeroed(), S_OK, &sample)
        }
    }
}

// SAFETY: the event queue, the descriptors and the tokens are Media Foundation objects, which are
// free-threaded (the Frame Server calls the source from its own threads); the rest is behind
// mutexes.
unsafe impl Send for StreamCore {}
unsafe impl Sync for StreamCore {}

#[implement(IMFMediaStream2, IKsControl)]
struct MediaStream {
    core: Arc<StreamCore>,
}

impl IMFMediaEventGenerator_Impl for MediaStream_Impl {
    fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        unsafe { self.core.queue.GetEvent(flags.0) }
    }
    fn BeginGetEvent(&self, callback: Ref<IMFAsyncCallback>, state: Ref<IUnknown>) -> Result<()> {
        unsafe { self.core.queue.BeginGetEvent(callback.as_ref(), state.as_ref()) }
    }
    fn EndGetEvent(&self, result: Ref<IMFAsyncResult>) -> Result<IMFMediaEvent> {
        unsafe { self.core.queue.EndGetEvent(result.as_ref()) }
    }
    fn QueueEvent(&self, met: u32, extended: *const GUID, status: HRESULT, value: *const PROPVARIANT) -> Result<()> {
        unsafe { self.core.queue.QueueEventParamVar(met, extended, status, value) }
    }
}

impl IMFMediaStream_Impl for MediaStream_Impl {
    fn GetMediaSource(&self) -> Result<IMFMediaSource> {
        self.core.source.lock().unwrap().clone().ok_or_else(|| MF_E_SHUTDOWN.into())
    }
    fn GetStreamDescriptor(&self) -> Result<IMFStreamDescriptor> {
        Ok(self.core.descriptor.clone())
    }
    fn RequestSample(&self, token: Ref<IUnknown>) -> Result<()> {
        {
            let mut inner = self.core.inner.lock().unwrap();
            if inner.shutdown {
                return err(MF_E_SHUTDOWN);
            }
            // A consumer that asks and asks gets the newest pictures, not a backlog of old ones.
            if inner.tokens.len() < 8 {
                inner.tokens.push_back(token.as_ref().cloned());
            }
        }
        self.core.cv.notify_all();
        Ok(())
    }
}

impl IMFMediaStream2_Impl for MediaStream_Impl {
    fn SetStreamState(&self, value: MF_STREAM_STATE) -> Result<()> {
        {
            let mut inner = self.core.inner.lock().unwrap();
            if inner.shutdown {
                return err(MF_E_SHUTDOWN);
            }
            inner.state = value;
            inner.running = value == MF_STREAM_STATE_RUNNING;
            if !inner.running {
                inner.tokens.clear();
            }
        }
        self.core.cv.notify_all();
        Ok(())
    }
    fn GetStreamState(&self) -> Result<MF_STREAM_STATE> {
        Ok(self.core.inner.lock().unwrap().state)
    }
}

impl IKsControl_Impl for MediaStream_Impl {
    fn KsProperty(&self, _: *const KSIDENTIFIER, _: u32, _: *mut core::ffi::c_void, _: u32, _: *mut u32) -> Result<()> {
        Err(set_not_found())
    }
    fn KsMethod(&self, _: *const KSIDENTIFIER, _: u32, _: *mut core::ffi::c_void, _: u32, _: *mut u32) -> Result<()> {
        Err(set_not_found())
    }
    fn KsEvent(&self, _: *const KSIDENTIFIER, _: u32, _: *mut core::ffi::c_void, _: u32, _: *mut u32) -> Result<()> {
        Err(set_not_found())
    }
}

// ---------------------------------------------------------------------------------------------
// The source.

fn media_type() -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(WIDTH, HEIGHT))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack(FPS, 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        t.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        t.SetUINT32(&MF_MT_DEFAULT_STRIDE, WIDTH)?;
        t.SetUINT32(&MF_MT_SAMPLE_SIZE, ring::picture_bytes(WIDTH, HEIGHT) as u32)?;
        t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
        t.SetUINT32(&MF_MT_FIXED_SIZE_SAMPLES, 1)?;
        t.SetUINT32(&MF_MT_AVG_BITRATE, (ring::picture_bytes(WIDTH, HEIGHT) as u32).saturating_mul(8 * FPS))?;
        Ok(t)
    }
}

#[implement(IMFMediaSourceEx, IMFActivate, IMFGetService, IKsControl)]
pub struct MediaSource {
    /// The attributes `IMFActivate` carries.
    attrs: IMFAttributes,
    /// What `GetSourceAttributes` returns.
    source_attrs: IMFAttributes,
    queue: IMFMediaEventQueue,
    descriptor: IMFPresentationDescriptor,
    stream: IMFMediaStream2,
    stream_core: Arc<StreamCore>,
    started_before: AtomicBool,
    shut: AtomicBool,
}

impl MediaSource {
    /// A source with its one stream, not yet started. Media Foundation must be running.
    pub fn create() -> Result<IMFMediaSourceEx> {
        unsafe {
            let attrs = {
                let mut a = None;
                MFCreateAttributes(&mut a, 4)?;
                a.ok_or_else(|| windows::core::Error::from(E_NOTIMPL))?
            };
            let source_attrs = {
                let mut a = None;
                MFCreateAttributes(&mut a, 4)?;
                a.ok_or_else(|| windows::core::Error::from(E_NOTIMPL))?
            };
            let ty = media_type()?;
            let sd = MFCreateStreamDescriptor(0, &[Some(ty.clone())])?;
            sd.GetMediaTypeHandler()?.SetCurrentMediaType(&ty)?;
            sd.SetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY, &PINNAME_VIDEO_CAPTURE)?;
            sd.SetUINT32(&MF_DEVICESTREAM_STREAM_ID, 0)?;
            sd.SetUINT32(&MF_DEVICESTREAM_FRAMESERVER_SHARED, 1)?;
            sd.SetUINT32(&MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES, MFFrameSourceTypes_Color.0 as u32)?;
            let pd = MFCreatePresentationDescriptor(Some(&[Some(sd.clone())]))?;
            pd.SelectStream(0)?;

            let stream_core = Arc::new(StreamCore {
                queue: MFCreateEventQueue()?,
                descriptor: sd,
                source: Mutex::new(None),
                inner: Mutex::new(Inner { running: false, shutdown: false, tokens: VecDeque::new(), state: MF_STREAM_STATE_STOPPED, thread: false }),
                cv: Condvar::new(),
            });
            let stream: IMFMediaStream2 = MediaStream { core: stream_core.clone() }.into();
            LIVE.fetch_add(1, Ordering::SeqCst);
            let source: IMFMediaSourceEx = MediaSource { attrs, source_attrs, queue: MFCreateEventQueue()?, descriptor: pd, stream, stream_core: stream_core.clone(), started_before: AtomicBool::new(false), shut: AtomicBool::new(false) }.into();
            *stream_core.source.lock().unwrap() = Some(source.cast()?);
            Ok(source)
        }
    }

    fn check(&self) -> Result<()> {
        if self.shut.load(Ordering::SeqCst) { err(MF_E_SHUTDOWN) } else { Ok(()) }
    }
}

impl Drop for MediaSource {
    fn drop(&mut self) {
        self.stream_core.shutdown();
        LIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

impl IMFMediaEventGenerator_Impl for MediaSource_Impl {
    fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        self.check()?;
        unsafe { self.queue.GetEvent(flags.0) }
    }
    fn BeginGetEvent(&self, callback: Ref<IMFAsyncCallback>, state: Ref<IUnknown>) -> Result<()> {
        self.check()?;
        unsafe { self.queue.BeginGetEvent(callback.as_ref(), state.as_ref()) }
    }
    fn EndGetEvent(&self, result: Ref<IMFAsyncResult>) -> Result<IMFMediaEvent> {
        self.check()?;
        unsafe { self.queue.EndGetEvent(result.as_ref()) }
    }
    fn QueueEvent(&self, met: u32, extended: *const GUID, status: HRESULT, value: *const PROPVARIANT) -> Result<()> {
        self.check()?;
        unsafe { self.queue.QueueEventParamVar(met, extended, status, value) }
    }
}

impl IMFMediaSource_Impl for MediaSource_Impl {
    fn GetCharacteristics(&self) -> Result<u32> {
        self.check()?;
        Ok(MFMEDIASOURCE_IS_LIVE.0 as u32)
    }
    fn CreatePresentationDescriptor(&self) -> Result<IMFPresentationDescriptor> {
        self.check()?;
        unsafe { self.descriptor.Clone() }
    }
    fn Start(&self, _descriptor: Ref<IMFPresentationDescriptor>, _time_format: *const GUID, start: *const PROPVARIANT) -> Result<()> {
        self.check()?;
        crate::log("Start");
        unsafe {
            let empty = PROPVARIANT::default();
            let start = if start.is_null() { &empty as *const PROPVARIANT } else { start };
            let first = !self.started_before.swap(true, Ordering::SeqCst);
            let met = if first { MENewStream } else { MEUpdatedStream };
            self.queue.QueueEventParamUnk(met.0 as u32, &GUID::zeroed(), S_OK, &self.stream)?;
            self.queue.QueueEventParamVar(MESourceStarted.0 as u32, &GUID::zeroed(), S_OK, start)?;
        }
        self.stream_core.start();
        Ok(())
    }
    fn Stop(&self) -> Result<()> {
        self.check()?;
        crate::log("Stop");
        self.stream_core.stop();
        unsafe { self.queue.QueueEventParamVar(MESourceStopped.0 as u32, &GUID::zeroed(), S_OK, &PROPVARIANT::default()) }
    }
    fn Pause(&self) -> Result<()> {
        self.check()?;
        err(MF_E_INVALID_STATE_TRANSITION)
    }
    fn Shutdown(&self) -> Result<()> {
        crate::log("Shutdown");
        if !self.shut.swap(true, Ordering::SeqCst) {
            self.stream_core.shutdown();
            unsafe {
                let _ = self.queue.Shutdown();
            }
        }
        Ok(())
    }
}

impl IMFMediaSourceEx_Impl for MediaSource_Impl {
    fn GetSourceAttributes(&self) -> Result<IMFAttributes> {
        self.check()?;
        Ok(self.source_attrs.clone())
    }
    fn GetStreamAttributes(&self, id: u32) -> Result<IMFAttributes> {
        self.check()?;
        if id != 0 {
            return err(MF_E_INVALIDSTREAMNUMBER);
        }
        self.stream_core.descriptor.cast()
    }
    fn SetD3DManager(&self, _: Ref<IUnknown>) -> Result<()> {
        err(E_NOTIMPL)
    }
}

impl IMFGetService_Impl for MediaSource_Impl {
    fn GetService(&self, _service: *const GUID, riid: *const GUID, ppv: *mut *mut core::ffi::c_void) -> Result<()> {
        // The Frame Server asks for the source's own control interface this way.
        if !riid.is_null() && unsafe { *riid } == IKsControl::IID {
            return unsafe { self.QueryInterface(riid, ppv) }.ok();
        }
        err(MF_E_UNSUPPORTED_SERVICE)
    }
}

impl IKsControl_Impl for MediaSource_Impl {
    fn KsProperty(&self, _: *const KSIDENTIFIER, _: u32, _: *mut core::ffi::c_void, _: u32, _: *mut u32) -> Result<()> {
        Err(set_not_found())
    }
    fn KsMethod(&self, _: *const KSIDENTIFIER, _: u32, _: *mut core::ffi::c_void, _: u32, _: *mut u32) -> Result<()> {
        Err(set_not_found())
    }
    fn KsEvent(&self, _: *const KSIDENTIFIER, _: u32, _: *mut core::ffi::c_void, _: u32, _: *mut u32) -> Result<()> {
        Err(set_not_found())
    }
}

impl IMFActivate_Impl for MediaSource_Impl {
    fn ActivateObject(&self, riid: *const GUID, ppv: *mut *mut core::ffi::c_void) -> Result<()> {
        crate::log("ActivateObject");
        unsafe { self.QueryInterface(riid, ppv) }.ok()
    }
    fn ShutdownObject(&self) -> Result<()> {
        self.Shutdown()
    }
    fn DetachObject(&self) -> Result<()> {
        Ok(())
    }
}

/// `IMFActivate` is an attribute store: all of it goes to the one made for it.
macro_rules! forward {
    ($s:ident . $m:ident ( $($a:expr),* )) => {{
        let attrs = &$s.attrs;
        unsafe { (Interface::vtable(attrs).$m)(Interface::as_raw(attrs), $($a),*) }
    }};
}

impl IMFAttributes_Impl for MediaSource_Impl {
    fn GetItem(&self, key: *const GUID, value: *mut PROPVARIANT) -> Result<()> {
        forward!(self.GetItem(key, value)).ok()
    }
    fn GetItemType(&self, key: *const GUID) -> Result<MF_ATTRIBUTE_TYPE> {
        let mut out = MF_ATTRIBUTE_TYPE::default();
        forward!(self.GetItemType(key, &mut out)).ok()?;
        Ok(out)
    }
    fn CompareItem(&self, key: *const GUID, value: *const PROPVARIANT) -> Result<BOOL> {
        let mut out = BOOL::default();
        forward!(self.CompareItem(key, value, &mut out)).ok()?;
        Ok(out)
    }
    fn Compare(&self, theirs: Ref<IMFAttributes>, kind: MF_ATTRIBUTES_MATCH_TYPE) -> Result<BOOL> {
        unsafe { self.attrs.Compare(theirs.as_ref(), kind) }
    }
    fn GetUINT32(&self, key: *const GUID) -> Result<u32> {
        let mut out = 0;
        forward!(self.GetUINT32(key, &mut out)).ok()?;
        Ok(out)
    }
    fn GetUINT64(&self, key: *const GUID) -> Result<u64> {
        let mut out = 0;
        forward!(self.GetUINT64(key, &mut out)).ok()?;
        Ok(out)
    }
    fn GetDouble(&self, key: *const GUID) -> Result<f64> {
        let mut out = 0.0;
        forward!(self.GetDouble(key, &mut out)).ok()?;
        Ok(out)
    }
    fn GetGUID(&self, key: *const GUID) -> Result<GUID> {
        let mut out = GUID::zeroed();
        forward!(self.GetGUID(key, &mut out)).ok()?;
        Ok(out)
    }
    fn GetStringLength(&self, key: *const GUID) -> Result<u32> {
        let mut out = 0;
        forward!(self.GetStringLength(key, &mut out)).ok()?;
        Ok(out)
    }
    fn GetString(&self, key: *const GUID, value: PWSTR, size: u32, length: *mut u32) -> Result<()> {
        forward!(self.GetString(key, value, size, length)).ok()
    }
    fn GetAllocatedString(&self, key: *const GUID, value: *mut PWSTR, length: *mut u32) -> Result<()> {
        forward!(self.GetAllocatedString(key, value, length)).ok()
    }
    fn GetBlobSize(&self, key: *const GUID) -> Result<u32> {
        let mut out = 0;
        forward!(self.GetBlobSize(key, &mut out)).ok()?;
        Ok(out)
    }
    fn GetBlob(&self, key: *const GUID, buf: *mut u8, size: u32, written: *mut u32) -> Result<()> {
        forward!(self.GetBlob(key, buf, size, written)).ok()
    }
    fn GetAllocatedBlob(&self, key: *const GUID, buf: *mut *mut u8, size: *mut u32) -> Result<()> {
        forward!(self.GetAllocatedBlob(key, buf, size)).ok()
    }
    fn GetUnknown(&self, key: *const GUID, riid: *const GUID, ppv: *mut *mut core::ffi::c_void) -> Result<()> {
        forward!(self.GetUnknown(key, riid, ppv)).ok()
    }
    fn SetItem(&self, key: *const GUID, value: *const PROPVARIANT) -> Result<()> {
        forward!(self.SetItem(key, value)).ok()
    }
    fn DeleteItem(&self, key: *const GUID) -> Result<()> {
        forward!(self.DeleteItem(key)).ok()
    }
    fn DeleteAllItems(&self) -> Result<()> {
        forward!(self.DeleteAllItems()).ok()
    }
    fn SetUINT32(&self, key: *const GUID, value: u32) -> Result<()> {
        forward!(self.SetUINT32(key, value)).ok()
    }
    fn SetUINT64(&self, key: *const GUID, value: u64) -> Result<()> {
        forward!(self.SetUINT64(key, value)).ok()
    }
    fn SetDouble(&self, key: *const GUID, value: f64) -> Result<()> {
        forward!(self.SetDouble(key, value)).ok()
    }
    fn SetGUID(&self, key: *const GUID, value: *const GUID) -> Result<()> {
        forward!(self.SetGUID(key, value)).ok()
    }
    fn SetString(&self, key: *const GUID, value: &PCWSTR) -> Result<()> {
        forward!(self.SetString(key, *value)).ok()
    }
    fn SetBlob(&self, key: *const GUID, buf: *const u8, size: u32) -> Result<()> {
        forward!(self.SetBlob(key, buf, size)).ok()
    }
    fn SetUnknown(&self, key: *const GUID, value: Ref<IUnknown>) -> Result<()> {
        unsafe { self.attrs.SetUnknown(key, value.as_ref()) }
    }
    fn LockStore(&self) -> Result<()> {
        forward!(self.LockStore()).ok()
    }
    fn UnlockStore(&self) -> Result<()> {
        forward!(self.UnlockStore()).ok()
    }
    fn GetCount(&self) -> Result<u32> {
        let mut out = 0;
        forward!(self.GetCount(&mut out)).ok()?;
        Ok(out)
    }
    fn GetItemByIndex(&self, index: u32, key: *mut GUID, value: *mut PROPVARIANT) -> Result<()> {
        forward!(self.GetItemByIndex(index, key, value)).ok()
    }
    fn CopyAllItems(&self, dest: Ref<IMFAttributes>) -> Result<()> {
        unsafe { self.attrs.CopyAllItems(dest.as_ref()) }
    }
}
