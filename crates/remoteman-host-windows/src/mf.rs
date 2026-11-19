//! Any maker's encoder: the hardware HEVC or AV1 encoder the GPU's driver registers with Media
//! Foundation (AMD's, Intel's, NVIDIA's and Qualcomm's drivers all bring one). It is the path
//! for cards that are not NVIDIA's, and can be asked for on any card (`--encoder mf`).
//!
//! What it can't do that NVENC can: it keeps no long-term frames and can't be told to stop
//! referring to a frame, so it answers `invalidate` with an error and every loss costs a
//! keyframe. And nothing is known of how a given maker's encoder numbers its pictures (see
//! docs/PROTOCOL.md, "A stream every decoder can follow"): that has to be shown per maker, with
//! a long recording, before it is relied on.
//!
//! Such an encoder works by events: it says when it wants a picture and when it has a frame.
//! With low delay asked for, each picture in gives its frame out before the next is wanted, and
//! [`Encoder::encode`] waits for exactly that; an encoder that held a frame back would stop it
//! with an error after a second, not stall the stream in silence.

use std::mem::ManuallyDrop;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};
use remoteman_core::Codec;
use remoteman_encode::{FrameEncoder, How, Made};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncMPVDefaultBPictureCount, CODECAPI_AVEncMPVGOPSize, CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode, ICodecAPI,
    IMF2DBuffer, IMFActivate, IMFDXGIDeviceManager, IMFMediaEventGenerator, IMFMediaType, IMFSample, IMFTransform, MF_E_TRANSFORM_STREAM_CHANGE, MF_EVENT_FLAG_NO_WAIT, MF_LOW_LATENCY, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MAX_KEYFRAME_SPACING, MF_MT_MPEG2_PROFILE, MF_MT_MPEG_SEQUENCE_HEADER, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_TRANSFORM_ASYNC_UNLOCK, MF_VERSION, MFCreateDXGIDeviceManager,
    MFCreateDXGISurfaceBuffer, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFSTARTUP_FULL, MFSampleExtension_CleanPoint, MFStartup, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE,
    MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_HARDWARE_VENDOR_ID_Attribute, MFT_FRIENDLY_NAME_Attribute, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
    MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_AV1, MFVideoFormat_HEVC, MFVideoFormat_P010, METransformHaveOutput, METransformNeedInput,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, IClassFactory};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{GUID, HSTRING, Interface, PWSTR};

use crate::capture::Gpu;
use crate::encode::Settings;

/// `MFVideoInterlace_Progressive`.
const PROGRESSIVE: u32 = 2;
/// `eAVEncH265VProfile_Main_420_10`.
const HEVC_MAIN_10: u32 = 2;
/// `eAVEncCommonRateControlMode_CBR`.
const CONSTANT_RATE: u32 = 0;
/// Frames between keyframes made unasked: as good as never.
const GOP: u32 = u32::MAX;
/// `MFT_OUTPUT_STREAM_PROVIDES_SAMPLES | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES`.
const BRINGS_SAMPLES: u32 = 0x100 | 0x200;

fn pair(high: u32, low: u32) -> u64 {
    (high as u64) << 32 | low as u64
}

fn string(activate: &IMFActivate, key: &GUID) -> String {
    let (mut text, mut length) = (PWSTR::null(), 0u32);
    // SAFETY: the string comes back in memory of COM's, freed here.
    unsafe {
        if activate.GetAllocatedString(key, &mut text, &mut length).is_err() || text.is_null() {
            return String::new();
        }
        let said = text.to_string().unwrap_or_default();
        CoTaskMemFree(Some(text.0 as *const _));
        said
    }
}

/// The hardware encoders for `codec` that Media Foundation lists, as (name, maker's PCI id as
/// "VEN_10DE", the encoder to be made).
fn listed(codec: Codec) -> Result<Vec<(String, String, IMFActivate)>> {
    let out = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: if codec == Codec::Av1 { MFVideoFormat_AV1 } else { MFVideoFormat_HEVC } };
    let (mut list, mut count) = (std::ptr::null_mut::<Option<IMFActivate>>(), 0u32);
    // SAFETY: the list comes back in memory of COM's; each entry is taken out of it (so that it
    // is released once, by us) and the list itself is freed.
    unsafe {
        MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER, None, Some(&out), &mut list, &mut count).context("asking Media Foundation for hardware encoders")?;
        let mut all = Vec::new();
        for i in 0..count as usize {
            if let Some(activate) = list.add(i).read() {
                all.push((string(&activate, &MFT_FRIENDLY_NAME_Attribute), string(&activate, &MFT_ENUM_HARDWARE_VENDOR_ID_Attribute), activate));
            }
        }
        if !list.is_null() {
            CoTaskMemFree(Some(list as *const _));
        }
        Ok(all)
    }
}

/// For tests: an encoder Media Foundation doesn't list, made straight from its library.
/// `REMOTEMAN_TEST_MFT_HEVC` (or `_AV1`) is `PATH,CLSID`. A guest that borrows its GPU has the
/// card maker's encoder among the driver's files but not registered, and this is how the path
/// is exercised there without changing the guest.
fn unlisted(codec: Codec) -> Result<Option<(String, IMFTransform)>> {
    let Ok(said) = std::env::var(if codec == Codec::Av1 { "REMOTEMAN_TEST_MFT_AV1" } else { "REMOTEMAN_TEST_MFT_HEVC" }) else { return Ok(None) };
    let (path, class) = said.rsplit_once(',').context("REMOTEMAN_TEST_MFT_*: PATH,CLSID")?;
    let class = GUID::try_from(class.trim_matches(['{', '}'])).map_err(|_| anyhow!("REMOTEMAN_TEST_MFT_*: {class} is no CLSID"))?;
    type GetClassObject = unsafe extern "system" fn(*const GUID, *const GUID, *mut *mut std::ffi::c_void) -> windows::core::HRESULT;
    // SAFETY: the library's documented entry point, called as COM would call it; the factory
    // it gives is ours to release.
    unsafe {
        let library = LoadLibraryExW(&HSTRING::from(path), None, LOAD_WITH_ALTERED_SEARCH_PATH).with_context(|| format!("loading {path}"))?;
        let entry = GetProcAddress(library, windows::core::s!("DllGetClassObject")).with_context(|| format!("{path} is no COM library"))?;
        let entry: GetClassObject = std::mem::transmute(entry);
        let mut factory = std::ptr::null_mut();
        entry(&class, &IClassFactory::IID, &mut factory).ok().with_context(|| format!("{path} has no class {class:?}"))?;
        let factory = IClassFactory::from_raw(factory);
        let transform: IMFTransform = factory.CreateInstance(None).context("making the encoder")?;
        let file = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(Some((format!("{file}, not listed (test)"), transform)))
    }
}

/// The names of the hardware encoders Media Foundation lists, for `--probe`.
pub fn names() -> Vec<String> {
    start();
    [Codec::Hevc, Codec::Av1].into_iter().flat_map(|c| listed(c).unwrap_or_default().into_iter().map(move |(name, maker, _)| format!("{c:?}: {name} ({maker})"))).collect()
}

fn start() {
    // SAFETY: plain calls; both may be made more than once.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
    }
}

pub struct Encoder {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    api: Option<ICodecAPI>,
    /// Kept for as long as the encoder has the device.
    _manager: IMFDXGIDeviceManager,
    input: ID3D11Texture2D,
    /// The encoder brings the memory for its frames itself.
    brings: bool,
    room: u32,
    fps: u32,
    /// Pictures the encoder has asked for and not been given; frames it has and we haven't taken.
    wanted: u32,
    ready: u32,
    /// The parameter sets, for a keyframe that comes without them.
    header: Vec<u8>,
    hevc: bool,
    frame: Vec<u8>,
    note: String,
}

impl Encoder {
    /// An encoder of this GPU's maker for pictures drawn into `input` (P010).
    pub fn open(gpu: &Gpu, input: &ID3D11Texture2D, s: &Settings) -> Result<Self> {
        start();
        let unlisted = unlisted(s.mode.codec)?;
        let all = if unlisted.is_some() { Vec::new() } else { listed(s.mode.codec)? };
        ensure!(unlisted.is_some() || !all.is_empty(), "Media Foundation lists no hardware {:?} encoder on this computer", s.mode.codec);
        // The one of the maker whose card the desktop is on, else whatever comes first.
        let maker = format!("VEN_{:04X}", gpu.vendor);
        let names: Vec<String> = all.iter().map(|(n, m, _)| format!("{n} ({m})")).collect();
        let chosen = all.iter().find(|(_, m, _)| m.eq_ignore_ascii_case(&maker)).or(all.first());
        // SAFETY: Media Foundation calls as documented, on objects made here.
        unsafe {
            let (name, transform) = match (unlisted, chosen) {
                (Some((name, transform)), _) => (name, transform),
                (None, Some((name, _, activate))) => (name.clone(), activate.ActivateObject::<IMFTransform>().with_context(|| format!("making \"{name}\""))?),
                (None, None) => unreachable!(),
            };
            let name = &name;
            let attributes = transform.GetAttributes().context("the encoder's attributes")?;
            // Hardware encoders work by events and want to be told the caller knows.
            attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1).context("unlocking the encoder")?;
            let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);

            let (mut token, mut manager) = (0u32, None);
            MFCreateDXGIDeviceManager(&mut token, &mut manager).context("a device manager")?;
            let manager = manager.context("a device manager")?;
            manager.ResetDevice(&gpu.device, token).context("giving Media Foundation the device")?;
            transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize).context("the encoder doesn't take Direct3D 11 pictures")?;

            // Constant rate, no frames that wait for later ones, one keyframe and then none
            // until asked. Said before the pictures are described: an encoder settles how often it
            // makes keyframes when it is told what it will be given. What an encoder doesn't know of
            // these it says, and is left as it is.
            let api = transform.cast::<ICodecAPI>().ok();
            let mut refused = Vec::new();
            if let Some(api) = &api {
                let mut set = |what: &str, key: &GUID, value: VARIANT| {
                    if api.SetValue(key, &value).is_err() {
                        refused.push(what.to_owned());
                    }
                };
                set("constant rate", &CODECAPI_AVEncCommonRateControlMode, VARIANT::from(CONSTANT_RATE));
                set("the bitrate", &CODECAPI_AVEncCommonMeanBitRate, VARIANT::from(s.bitrate));
                set("low delay", &CODECAPI_AVLowLatencyMode, VARIANT::from(true));
                set("no B-frames", &CODECAPI_AVEncMPVDefaultBPictureCount, VARIANT::from(0u32));
                // (For tests: `REMOTEMAN_TEST_MF_GOP` tries another figure.)
                let gop = std::env::var("REMOTEMAN_TEST_MF_GOP").ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(GOP);
                set("no keyframes by the clock", &CODECAPI_AVEncMPVGOPSize, VARIANT::from(gop));
            }

            let common = |t: &IMFMediaType| -> windows::core::Result<()> {
                t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                t.SetUINT64(&MF_MT_FRAME_SIZE, pair(s.width, s.height))?;
                t.SetUINT64(&MF_MT_FRAME_RATE, pair(s.fps.max(1), 1))?;
                t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pair(1, 1))?;
                t.SetUINT32(&MF_MT_INTERLACE_MODE, PROGRESSIVE)
            };
            let hevc = s.mode.codec == Codec::Hevc;
            let out = MFCreateMediaType()?;
            common(&out)?;
            out.SetGUID(&MF_MT_SUBTYPE, if hevc { &MFVideoFormat_HEVC } else { &MFVideoFormat_AV1 })?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, s.bitrate)?;
            let _ = out.SetUINT32(&MF_MT_MAX_KEYFRAME_SPACING, GOP);
            if hevc {
                out.SetUINT32(&MF_MT_MPEG2_PROFILE, HEVC_MAIN_10)?;
            }
            transform.SetOutputType(0, &out, 0).with_context(|| format!("\"{name}\" refused {:?} 10-bit at {}x{} and {} Hz", s.mode.codec, s.width, s.height, s.fps))?;
            let picture = MFCreateMediaType()?;
            common(&picture)?;
            picture.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_P010)?;
            transform.SetInputType(0, &picture, 0).with_context(|| format!("\"{name}\" refused 10-bit pictures (P010)"))?;

            let info = transform.GetOutputStreamInfo(0).context("the encoder's output")?;
            let events = transform.cast::<IMFMediaEventGenerator>().context("the encoder gives no events (not a hardware encoder?)")?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            let mut note = format!("Media Foundation's \"{name}\"; a loss costs a keyframe");
            if !refused.is_empty() {
                note += &format!("; it wouldn't be told: {}", refused.join(", "));
            }
            if names.len() > 1 {
                note += &format!("; listed: {}", names.join(", "));
            }
            let mut encoder = Self { transform, events, api, _manager: manager, input: input.clone(), brings: info.dwFlags & BRINGS_SAMPLES != 0, room: info.cbSize.max(1 << 20), fps: s.fps.max(1), wanted: 0, ready: 0, header: Vec::new(), hevc, frame: Vec::new(), note };
            encoder.read_header();
            Ok(encoder)
        }
    }

    /// The parameter sets as the output type holds them, if it does.
    fn read_header(&mut self) {
        // SAFETY: the blob comes back in memory of COM's, copied and freed here.
        unsafe {
            let Ok(kind) = self.transform.GetOutputCurrentType(0) else { return };
            let (mut at, mut length) = (std::ptr::null_mut::<u8>(), 0u32);
            if kind.GetAllocatedBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut at, &mut length).is_ok() && !at.is_null() {
                self.header = std::slice::from_raw_parts(at, length as usize).to_vec();
                CoTaskMemFree(Some(at as *const _));
            }
        }
    }

    /// Take the encoder's next event, if it has one.
    fn turn(&mut self) -> Result<bool> {
        // SAFETY: plain calls on live objects.
        let event = match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
            Ok(e) => e,
            // (MF_E_NO_EVENTS_AVAILABLE, and anything else that means "not now".)
            Err(_) => return Ok(false),
        };
        // SAFETY: as above.
        let kind = unsafe { event.GetType() }?;
        if kind == METransformNeedInput.0 as u32 {
            self.wanted += 1;
        } else if kind == METransformHaveOutput.0 as u32 {
            self.ready += 1;
        }
        Ok(true)
    }

    /// Wait until `done` says so, taking the encoder's events meanwhile.
    fn wait(&mut self, what: &str, done: impl Fn(&Self) -> bool) -> Result<()> {
        let began = Instant::now();
        while !done(self) {
            if !self.turn()? {
                ensure!(began.elapsed() < Duration::from_secs(1), "the encoder {what} within a second (it holds frames back, which this host can't use)");
                std::thread::sleep(Duration::from_micros(200));
            }
        }
        Ok(())
    }

    fn encode(&mut self, number: u64, key: bool) -> Result<bool> {
        self.wait("didn't ask for a picture", |e| e.wanted > 0)?;
        // SAFETY: Media Foundation calls as documented; the texture is the converter's, which
        // isn't drawn into again until this frame is out.
        unsafe {
            if key {
                if let Some(api) = &self.api {
                    let _ = api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1u32));
                }
            }
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &self.input, 0, false).context("the picture as a Media Foundation buffer")?;
            if let Ok(length) = buffer.cast::<IMF2DBuffer>().and_then(|b| b.GetContiguousLength()) {
                let _ = buffer.SetCurrentLength(length);
            }
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            // In units of 100 ns.
            sample.SetSampleTime((number * 10_000_000 / self.fps as u64) as i64)?;
            sample.SetSampleDuration((10_000_000 / self.fps as u64) as i64)?;
            self.wanted -= 1;
            self.transform.ProcessInput(0, &sample, 0).context("giving the encoder the picture")?;
        }
        self.wait("made no frame", |e| e.ready > 0)?;
        self.ready -= 1;
        for _ in 0..2 {
            // SAFETY: the output buffer is filled in by the encoder; the sample in it is taken
            // out (and released) here whatever happens.
            unsafe {
                let ours = if self.brings {
                    None
                } else {
                    let sample = MFCreateSample()?;
                    sample.AddBuffer(&MFCreateMemoryBuffer(self.room)?)?;
                    Some(sample)
                };
                let mut out = [MFT_OUTPUT_DATA_BUFFER { dwStreamID: 0, pSample: ManuallyDrop::new(ours), dwStatus: 0, pEvents: ManuallyDrop::new(None) }];
                let mut status = 0u32;
                let made = self.transform.ProcessOutput(0, &mut out, &mut status);
                let sample: Option<IMFSample> = ManuallyDrop::take(&mut out[0].pSample);
                drop(ManuallyDrop::take(&mut out[0].pEvents));
                match made {
                    Ok(()) => {}
                    // The encoder settled its output's description: accept it and ask again.
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        let kind = self.transform.GetOutputAvailableType(0, 0)?;
                        self.transform.SetOutputType(0, &kind, 0)?;
                        self.read_header();
                        continue;
                    }
                    Err(e) => return Err(e).context("taking the encoder's frame"),
                }
                let sample = sample.context("the encoder said it had a frame and gave none")?;
                let clean = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
                let buffer = sample.ConvertToContiguousBuffer()?;
                let (mut at, mut length) = (std::ptr::null_mut::<u8>(), 0u32);
                buffer.Lock(&mut at, None, Some(&mut length))?;
                self.frame.clear();
                let data = std::slice::from_raw_parts(at, length as usize);
                // A keyframe must carry its parameter sets: an HEVC one that begins with
                // anything but the video parameter set (type 32) gets them put in front.
                if clean && self.hevc && !self.header.is_empty() && !starts_with_vps(data) {
                    self.frame.extend_from_slice(&self.header);
                }
                self.frame.extend_from_slice(data);
                let _ = buffer.Unlock();
                return Ok(clean);
            }
        }
        bail!("the encoder kept changing its output's description")
    }
}

/// Does an HEVC frame (with start codes) begin with a video parameter set?
fn starts_with_vps(data: &[u8]) -> bool {
    let at = match data {
        [0, 0, 1, ..] => 3,
        [0, 0, 0, 1, ..] => 4,
        _ => return false,
    };
    data.get(at).is_some_and(|b| (b >> 1) & 0x3f == 32)
}

impl FrameEncoder for Encoder {
    fn note(&self) -> &str {
        &self.note
    }

    fn short_term(&self) -> u32 {
        0
    }

    fn long_term(&self) -> u32 {
        0
    }

    fn encode_how(&mut self, _idx: usize, timestamp: u64, how: How) -> Result<Made<'_>> {
        let key = self.encode(timestamp, how.idr)?;
        Ok(Made { data: &self.frame, key, marked: None, referred: 0, qp: 0 })
    }

    fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        let api = self.api.as_ref().context("this encoder takes no settings while it runs")?;
        // SAFETY: a live interface and a whole VARIANT.
        unsafe { api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &VARIANT::from(bitrate)) }.context("the encoder refused the new bitrate")
    }

    fn invalidate(&mut self, _timestamp: u64) -> Result<()> {
        Err(anyhow!("this encoder can't be told to stop referring to a frame"))
    }
}
