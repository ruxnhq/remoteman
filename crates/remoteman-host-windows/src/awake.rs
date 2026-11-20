//! Keeps the PC awake while a device is connected: no sleep and no screens off for want of
//! input. A power request (`PowerCreateRequest`) rather than `SetThreadExecutionState`: a request
//! belongs to the handle, not to a thread, so it works from the service's host (SYSTEM, in the
//! console's session) and ends when the handle is closed, whichever thread does it. `powercfg
//! /requests` lists it under its reason text.

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Power::{PowerClearRequest, PowerCreateRequest, PowerRequestDisplayRequired, PowerRequestSystemRequired, PowerSetRequest};
use windows::Win32::System::Threading::{POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0};
use windows::core::PWSTR;

/// `POWER_REQUEST_CONTEXT_VERSION`.
const VERSION: u32 = 0;
/// The reason Windows shows beside the request.
const REASON: &str = "remoteman: a device is connected";

/// While this lives, the PC stays awake. Dropping it lets the PC sleep again.
pub struct Awake(HANDLE);

// SAFETY: a power request handle may be used from any thread.
unsafe impl Send for Awake {}

impl Awake {
    pub fn new() -> Option<Self> {
        let mut reason: Vec<u16> = REASON.encode_utf16().chain(Some(0)).collect();
        let context = REASON_CONTEXT { Version: VERSION, Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING, Reason: REASON_CONTEXT_0 { SimpleReasonString: PWSTR(reason.as_mut_ptr()) } };
        // SAFETY: the context and its text are ours and outlive the call, which copies them.
        let handle = match unsafe { PowerCreateRequest(&context) } {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("couldn't ask Windows to keep this PC awake: {e}");
                return None;
            }
        };
        let mut kept = Vec::new();
        for (what, name) in [(PowerRequestSystemRequired, "the PC"), (PowerRequestDisplayRequired, "the screens")] {
            // SAFETY: the handle is ours.
            match unsafe { PowerSetRequest(handle, what) } {
                Ok(()) => kept.push(name),
                Err(e) => tracing::warn!("couldn't keep {name} awake: {e}"),
            }
        }
        if kept.is_empty() {
            // SAFETY: ours.
            let _ = unsafe { CloseHandle(handle) };
            return None;
        }
        tracing::info!("a device is connected: {} kept awake until it leaves", kept.join(" and "));
        Some(Self(handle))
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        // SAFETY: the handle is ours; clearing is optional (closing ends the requests) but
        // makes the end exact.
        unsafe {
            let _ = PowerClearRequest(self.0, PowerRequestSystemRequired);
            let _ = PowerClearRequest(self.0, PowerRequestDisplayRequired);
            let _ = CloseHandle(self.0);
        }
        tracing::info!("no device is connected: the PC may sleep again");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A normal-user process may hold the request; it is released on drop.
    #[test]
    fn a_request_can_be_made_and_released() {
        let awake = Awake::new();
        assert!(awake.is_some());
        drop(awake);
    }
}
