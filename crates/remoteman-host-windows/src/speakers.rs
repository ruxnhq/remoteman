//! The PC's own speakers go quiet while a device listens. Without Steam's speakers they are
//! muted: the default output's mute switch (`IAudioEndpointVolume`) goes on, and what it was
//! comes back when the sound stops. WASAPI loopback still records the sound while the output is
//! muted (measured: the same level muted and not), so the device hears it all the same. What
//! was muted is kept in `speakers-mute.json` until it is undone, like the state below.
//!
//! If Steam's "Steam Streaming Speakers"
//! exists, it becomes the default output (for the console's user) while the device has sound
//! on, and the output that was the default comes back when the sound stops, the session ends or
//! the host starts again after a crash. The desktop's sound is recorded from Steam's speakers
//! meanwhile (`sound::RECORD_FROM`), whatever Windows says the default is for the host's own
//! process.
//!
//! Windows has no documented way to change the default output; this uses `IPolicyConfig`, the
//! interface its own Sound settings use (undocumented, unchanged since Vista).
//!
//! What was the default is written to `speakers.json` in the state folder before anything is
//! changed, and removed once it is back: a host that dies in between puts it back at its next
//! start ([`init`]), unless the user has picked another output since.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{DEVICE_STATE_ACTIVE, ERole, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, eCommunications, eConsole, eMultimedia, eRender};
use windows::Win32::Security::{ImpersonateLoggedOnUser, RevertToSelf};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, STGM_READ};
use windows::core::{BSTR, GUID, HRESULT, IUnknown_Vtbl, Interface, PCWSTR};

/// What the output is called, lower case: Valve's virtual speakers.
const STEAM_SPEAKERS: &str = "steam streaming speakers";

/// `PolicyConfigClient`.
const POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

/// `IPolicyConfig` (the Vista-and-later one), as its table of methods. Only
/// `SetDefaultEndpoint` is called; the ten before it only put it in its place. (Written out
/// rather than with the `interface` macro, which needs the `windows-core` crate by name.)
const POLICY_CONFIG: GUID = GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);

#[repr(C)]
struct PolicyConfigVtbl {
    base: IUnknown_Vtbl,
    /// GetMixFormat .. SetPropertyValue.
    before: [*const c_void; 10],
    set_default_endpoint: unsafe extern "system" fn(this: *mut c_void, device: PCWSTR, role: i32) -> HRESULT,
}

/// The roles a default output is kept for: what programs play to ("console" and "multimedia")
/// and what calls use.
const ROLES: [ERole; 3] = [eConsole, eMultimedia, eCommunications];
const ROLE_NAMES: [&str; 3] = ["console", "multimedia", "communications"];

static STATE: OnceLock<PathBuf> = OnceLock::new();

fn file() -> Option<PathBuf> {
    STATE.get().map(|s| s.join("speakers.json"))
}

// ---- What is decided, apart from Windows. ----

/// Is this output Steam's virtual speakers?
fn is_steam_speakers(name: &str) -> bool {
    name.to_lowercase().contains(STEAM_SPEAKERS)
}

/// What a session switched: Steam's speakers, and the defaults they replaced (per role).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Saved {
    steam: String,
    previous: [Option<String>; 3],
}

impl Saved {
    fn to_json(&self) -> String {
        serde_json::json!({ "steam": self.steam, "previous": self.previous }).to_string()
    }

    fn from_json(text: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        let steam = v.get("steam")?.as_str()?.to_string();
        let list = v.get("previous")?.as_array()?;
        let at = |i: usize| list.get(i).and_then(|x| x.as_str()).map(String::from);
        Some(Self { steam, previous: [at(0), at(1), at(2)] })
    }
}

/// With these outputs (id, name) and these defaults, does a session switch, and from what? Not
/// where there is no Steam output, or it already is the default for every role.
fn plan(outputs: &[(String, String)], defaults: &[Option<String>; 3]) -> Option<Saved> {
    let steam = outputs.iter().find(|(_, name)| is_steam_speakers(name))?.0.clone();
    if defaults.iter().all(|d| d.as_deref() == Some(steam.as_str())) {
        return None;
    }
    Some(Saved { steam: steam.clone(), previous: defaults.clone().map(|d| d.filter(|d| *d != steam)) })
}

/// What to set back, given what is the default now: a role still on Steam's output goes back to
/// what it was; one the user has moved elsewhere since is left alone.
fn restore_plan(saved: &Saved, now: &[Option<String>; 3]) -> Vec<(usize, String)> {
    (0..3).filter(|&i| now[i].as_deref() == Some(saved.steam.as_str())).filter_map(|i| saved.previous[i].clone().map(|p| (i, p))).collect()
}

/// What muting the PC's speakers changed: which output, so it is unmuted again. (An output that
/// was muted already is left alone and nothing is kept.)
#[derive(Debug, Clone, PartialEq, Eq)]
struct MuteSaved {
    endpoint: String,
}

impl MuteSaved {
    fn to_json(&self) -> String {
        serde_json::json!({ "muted_endpoint": self.endpoint }).to_string()
    }

    fn from_json(text: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        Some(Self { endpoint: v.get("muted_endpoint")?.as_str()?.to_string() })
    }
}

fn mute_file() -> Option<PathBuf> {
    STATE.get().map(|s| s.join("speakers-mute.json"))
}

// ---- Windows. ----

/// Runs on a thread of its own with COM, and as the user signed in at the console where this is
/// the service's host (the default output is that user's, not SYSTEM's).
fn as_console_user<T: Send>(work: impl FnOnce() -> Result<T> + Send) -> Result<T> {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("speakers".into())
            .spawn_scoped(scope, || {
                // SAFETY: plain calls, undone at the end of this thread's work.
                let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
                let token = if crate::desk::is_system() { crate::desk::console_user() } else { None };
                let impersonating = token.as_ref().is_some_and(|t| unsafe { ImpersonateLoggedOnUser(t.0) }.is_ok());
                let result = work();
                if impersonating {
                    // SAFETY: undoes the impersonation above.
                    let _ = unsafe { RevertToSelf() };
                }
                drop(token);
                if com {
                    // SAFETY: balances the CoInitializeEx above.
                    unsafe { CoUninitialize() };
                }
                result
            })
            .context("a thread for the sound settings")?
            .join()
            .map_err(|_| anyhow::anyhow!("the sound settings' thread died"))?
    })
}

fn enumerator() -> Result<IMMDeviceEnumerator> {
    // SAFETY: COM is set up on this thread.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.context("Windows' list of sound devices")
}

fn id_of(device: &IMMDevice) -> Option<String> {
    // SAFETY: the id is Windows' text, read and freed.
    unsafe {
        let id = device.GetId().ok()?;
        let text = id.to_string().ok();
        CoTaskMemFree(Some(id.0 as *const _));
        text
    }
}

fn name_of(device: &IMMDevice) -> String {
    // SAFETY: plain COM calls on a live device.
    unsafe { device.OpenPropertyStore(STGM_READ).and_then(|store| store.GetValue(&windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName)) }.ok().and_then(|v| BSTR::try_from(&v).ok()).map(|b| b.to_string()).unwrap_or_default()
}

/// Every active output: its id and its name.
fn outputs() -> Result<Vec<(String, String)>> {
    // SAFETY: COM calls on this thread's own objects.
    unsafe {
        let all = enumerator()?.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        Ok((0..all.GetCount()?).filter_map(|i| all.Item(i).ok()).filter_map(|d| Some((id_of(&d)?, name_of(&d)))).collect())
    }
}

fn defaults() -> Result<[Option<String>; 3]> {
    let e = enumerator()?;
    // SAFETY: COM calls on this thread's own objects.
    Ok(ROLES.map(|role| unsafe { e.GetDefaultAudioEndpoint(eRender, role) }.ok().and_then(|d| id_of(&d))))
}

fn set_default(id: &str, role: usize) -> Result<()> {
    // SAFETY: COM calls on this thread's own objects; the text outlives the call.
    unsafe {
        let object: windows::core::IUnknown = CoCreateInstance(&POLICY_CONFIG_CLIENT, None, CLSCTX_ALL).context("Windows' sound settings")?;
        let mut policy = std::ptr::null_mut::<c_void>();
        object.query(&POLICY_CONFIG, &mut policy).ok().context("Windows' sound settings don't take this call")?;
        let text = windows::core::HSTRING::from(id);
        let vtbl = *(policy as *mut *const PolicyConfigVtbl);
        let said = ((*vtbl).set_default_endpoint)(policy, PCWSTR(text.as_ptr()), ROLES[role].0);
        ((*vtbl).base.Release)(policy);
        said.ok().with_context(|| format!("setting the {} output", ROLE_NAMES[role]))
    }
}

fn endpoint_volume(device: &IMMDevice) -> Result<IAudioEndpointVolume> {
    // SAFETY: plain COM call on a live device.
    unsafe { device.Activate(CLSCTX_ALL, None) }.context("the sound output's volume")
}

/// The default output as the console's user has it: its id, whether it is muted, its name.
/// Run as that user ([`as_console_user`]): the default is theirs.
fn default_output() -> Result<(String, bool, String)> {
    // SAFETY: COM calls on this thread's own objects.
    let device = unsafe { enumerator()?.GetDefaultAudioEndpoint(eRender, eConsole) }.context("this computer has no sound output")?;
    let id = id_of(&device).context("the sound output has no id")?;
    // SAFETY: plain COM call on a live endpoint volume.
    let muted = unsafe { endpoint_volume(&device)?.GetMute() }.context("the sound output's mute switch")?.as_bool();
    Ok((id, muted, name_of(&device)))
}

fn set_mute(endpoint: &str, on: bool) -> Result<()> {
    // SAFETY: COM calls on this thread's own objects.
    let device = unsafe { enumerator()?.GetDevice(&windows::core::HSTRING::from(endpoint)) }.context("the sound output has gone")?;
    // SAFETY: plain COM call on a live endpoint volume.
    unsafe { endpoint_volume(&device)?.SetMute(on, std::ptr::null()) }.context("the sound output's mute switch")
}

/// Mute the default output unless it already is; `Some` says which one was muted (to undo).
/// The output is looked up and muted as the console's user (whose default it is), and what was
/// muted is written down in between by the host itself: the state folder is the
/// administrators', and a thread acting as the user may not write there (seen 2026-11-20: the
/// write was refused and nothing was muted).
fn mute_default() -> Result<Option<MuteSaved>> {
    let (endpoint, muted, name) = as_console_user(default_output)?;
    if muted {
        tracing::info!(output = name, "the PC's speakers are muted already");
        return Ok(None);
    }
    let saved = MuteSaved { endpoint };
    // Written first: a host that dies from here on unmutes at its next start.
    let file = mute_file();
    if let Some(f) = &file {
        std::fs::write(f, saved.to_json()).with_context(|| format!("writing {}", f.display()))?;
    }
    if let Err(e) = as_console_user(|| set_mute(&saved.endpoint, true)) {
        if let Some(f) = &file {
            let _ = std::fs::remove_file(f);
        }
        return Err(e.context("muting the sound output"));
    }
    tracing::info!(output = name, "a device listens: the PC's speakers are muted (the sound is still recorded)");
    Ok(Some(saved))
}

fn unmute(saved: &MuteSaved, why: &str) {
    let result = as_console_user(|| {
        // SAFETY: COM calls on this thread's own objects.
        let device = unsafe { enumerator()?.GetDevice(&windows::core::HSTRING::from(saved.endpoint.as_str())) }.context("the muted output has gone")?;
        // SAFETY: plain COM call on a live endpoint volume.
        unsafe { endpoint_volume(&device)?.SetMute(false, std::ptr::null()) }.context("unmuting the sound output")
    });
    match result {
        Ok(()) => tracing::info!(why, "the PC's speakers are unmuted again"),
        Err(e) => tracing::warn!("the PC's speakers couldn't be unmuted: {e:#}"),
    }
    if let Some(f) = mute_file() {
        let _ = std::fs::remove_file(f);
    }
}

/// The output names on this PC, and whether Steam's speakers are among them (for tests and
/// the log): no change is made.
pub fn enumerate() -> (Vec<String>, bool) {
    let names: Vec<String> = as_console_user(outputs).unwrap_or_default().into_iter().map(|(_, n)| n).collect();
    let steam = names.iter().any(|n| is_steam_speakers(n));
    (names, steam)
}

fn put_back(saved: &Saved, why: &str) {
    let result = as_console_user(|| {
        let now = defaults()?;
        let mut back = Vec::new();
        for (role, id) in restore_plan(saved, &now) {
            match set_default(&id, role) {
                Ok(()) => back.push(ROLE_NAMES[role]),
                Err(e) => tracing::warn!("the {} output couldn't be put back: {e:#}", ROLE_NAMES[role]),
            }
        }
        Ok(back)
    });
    match result {
        Ok(back) => tracing::info!(roles = ?back, why, "the PC's speakers are the default output again"),
        Err(e) => tracing::warn!("the PC's speakers couldn't be put back: {e:#}"),
    }
    if let Some(f) = file() {
        let _ = std::fs::remove_file(f);
    }
    *crate::sound::RECORD_FROM.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// At the host's start: remember where the state folder is, and put the speakers back if the
/// host that ran before left them switched.
pub fn init(state: &Path) {
    let _ = STATE.set(state.to_path_buf());
    if let Some(saved) = file().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|t| Saved::from_json(&t)) {
        put_back(&saved, "the host that switched them is gone");
    }
    if let Some(saved) = mute_file().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|t| MuteSaved::from_json(&t)) {
        unmute(&saved, "the host that muted them is gone");
    }
    let (names, steam) = enumerate();
    tracing::info!(outputs = ?names, steam, "the PC's sound outputs: the PC's speakers go quiet while a device listens (if the setting is on), {}", if steam && std::env::var("REMOTEMAN_SWITCH_TO_STEAM").as_deref() == Ok("1") { "by Steam's speakers becoming the default output" } else { "by muting the default output" });
}

/// While this lives, the PC's speakers are quiet: Steam's speakers are the default output or,
/// without them, the default output is muted. Dropping it undoes that.
pub struct Switched {
    how: How,
}

enum Engaged {
    Steam(Saved, usize),
    Muted(MuteSaved),
}

enum How {
    Steam(Saved),
    Muted(MuteSaved),
}

impl Switched {
    /// Switch, if Steam's speakers exist. `None`: nothing was changed (and the sound goes to
    /// whatever the default is).
    pub fn engage() -> Option<Self> {
        // Each step that needs the user's view of the outputs runs as the console's user; what
        // is written to the state folder is written by the host itself, between them.
        let made = (|| -> Result<Option<Engaged>> {
            // Muting is what's used, Steam's speakers or not: it was measured to work (loopback
            // records the same muted), and it leaves every program on its usual output. Switching
            // to Steam's speakers stays for REMOTEMAN_SWITCH_TO_STEAM=1.
            if std::env::var("REMOTEMAN_SWITCH_TO_STEAM").as_deref() != Ok("1") {
                return Ok(mute_default()?.map(Engaged::Muted));
            }
            let (planned, steam) = as_console_user(|| {
                let outputs = outputs()?;
                Ok((plan(&outputs, &defaults()?), outputs.iter().any(|(_, n)| is_steam_speakers(n))))
            })?;
            let Some(saved) = planned else {
                if !steam {
                    // No Steam output: the default output is muted instead.
                    return Ok(mute_default()?.map(Engaged::Muted));
                }
                return Ok(None);
            };
            // Written first: a host that dies from here on is put right at its next start.
            if let Some(f) = file() {
                std::fs::write(&f, saved.to_json()).with_context(|| format!("writing {}", f.display()))?;
            }
            let switched = as_console_user(|| {
                let mut switched = 0;
                for role in 0..3 {
                    match set_default(&saved.steam, role) {
                        Ok(()) => switched += 1,
                        Err(e) => tracing::warn!("{e:#}"),
                    }
                }
                Ok(switched)
            })?;
            Ok(Some(Engaged::Steam(saved, switched)))
        })();
        match made {
            Ok(Some(Engaged::Muted(saved))) => Some(Self { how: How::Muted(saved) }),
            Ok(Some(Engaged::Steam(saved, switched))) if switched > 0 => {
                *crate::sound::RECORD_FROM.lock().unwrap_or_else(|e| e.into_inner()) = Some(saved.steam.clone());
                tracing::info!("a device listens: \"Steam Streaming Speakers\" is the default output, the PC's own speakers are quiet");
                Some(Self { how: How::Steam(saved) })
            }
            Ok(Some(Engaged::Steam(saved, _))) => {
                put_back(&saved, "none could be switched");
                None
            }
            Ok(None) => None,
            Err(e) => {
                tracing::warn!("couldn't switch the PC's speakers: {e:#}");
                None
            }
        }
    }
}

impl Drop for Switched {
    fn drop(&mut self) {
        match &self.how {
            How::Steam(saved) => put_back(saved, "the device stopped listening"),
            How::Muted(saved) => unmute(saved, "the device stopped listening"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(id: &str, name: &str) -> (String, String) {
        (id.into(), name.into())
    }
    fn some(id: &str) -> Option<String> {
        Some(id.into())
    }

    #[test]
    fn steam_speakers_are_found_by_name() {
        assert!(is_steam_speakers("Speakers (Steam Streaming Speakers)"));
        assert!(!is_steam_speakers("Speakers (Steam Streaming Microphone)"));
        assert!(!is_steam_speakers("Realtek Speakers"));
    }

    #[test]
    fn a_switch_is_planned_only_where_there_is_something_to_switch() {
        let outputs = [out("a", "Realtek Speakers"), out("s", "Speakers (Steam Streaming Speakers)")];
        let saved = plan(&outputs, &[some("a"), some("a"), some("b")]).unwrap();
        assert_eq!(saved, Saved { steam: "s".into(), previous: [some("a"), some("a"), some("b")] });
        // No Steam output: nothing.
        assert!(plan(&outputs[..1], &[some("a"), some("a"), some("a")]).is_none());
        // Already the default everywhere: nothing (and nothing to put back to itself).
        assert!(plan(&outputs, &[some("s"), some("s"), some("s")]).is_none());
        // On Steam's for one role only: the others are switched, that one has no "before".
        let saved = plan(&outputs, &[some("s"), some("a"), some("a")]).unwrap();
        assert_eq!(saved.previous, [None, some("a"), some("a")]);
    }

    #[test]
    fn what_was_switched_is_put_back_unless_the_user_moved_on() {
        let saved = Saved { steam: "s".into(), previous: [some("a"), some("a"), some("b")] };
        assert_eq!(restore_plan(&saved, &[some("s"), some("s"), some("s")]), vec![(0, "a".into()), (1, "a".into()), (2, "b".into())]);
        // The user picked another output for one role since: that one stays.
        assert_eq!(restore_plan(&saved, &[some("s"), some("z"), some("s")]), vec![(0, "a".into()), (2, "b".into())]);
        assert!(restore_plan(&saved, &[some("a"), some("a"), some("b")]).is_empty());
    }

    #[test]
    fn the_muted_output_is_remembered() {
        let saved = MuteSaved { endpoint: "{0.0.0.00000000}.{abc}".into() };
        assert_eq!(MuteSaved::from_json(&saved.to_json()), Some(saved));
        assert_eq!(MuteSaved::from_json("{}"), None);
    }

    #[test]
    fn the_saved_state_round_trips() {
        let saved = Saved { steam: "{0.0.0.00000000}.{abc}".into(), previous: [some("a"), None, some("b")] };
        assert_eq!(Saved::from_json(&saved.to_json()), Some(saved));
        assert_eq!(Saved::from_json("not json"), None);
    }

    /// Lists this PC's outputs and says whether Steam's speakers exist; changes nothing.
    #[test]
    fn this_pcs_outputs_can_be_listed() {
        let (names, steam) = enumerate();
        eprintln!("render endpoints: {names:?}; Steam Streaming Speakers present: {steam}");
        assert!(!names.is_empty());
    }
}
