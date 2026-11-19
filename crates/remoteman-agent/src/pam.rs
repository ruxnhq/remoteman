//! Checking a password against this computer's own rules for logging in (PAM), the way a
//! screen locker does: as the user the host runs as, for that user's own account, with no
//! special rights. The system's limits on wrong tries apply as they do at its own lock screen.

use std::ffi::{CStr, CString, c_char, c_int, c_void};

#[repr(C)]
struct Message {
    style: c_int,
    text: *const c_char,
}

#[repr(C)]
struct Response {
    text: *mut c_char,
    code: c_int,
}

#[repr(C)]
struct Conversation {
    talk: extern "C" fn(count: c_int, messages: *mut *const Message, responses: *mut *mut Response, data: *mut c_void) -> c_int,
    data: *mut c_void,
}

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_start(service: *const c_char, user: *const c_char, conversation: *const Conversation, handle: *mut *mut c_void) -> c_int;
    fn pam_authenticate(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_acct_mgmt(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_end(handle: *mut c_void, status: c_int) -> c_int;
    fn pam_strerror(handle: *mut c_void, code: c_int) -> *const c_char;
}

const SUCCESS: c_int = 0;
const PROMPT_ECHO_OFF: c_int = 1;
const PROMPT_ECHO_ON: c_int = 2;
const BUF_ERR: c_int = 5;
const CONV_ERR: c_int = 19;
/// What PAM answers when the password (or the user) is simply not right.
const WRONG: [c_int; 4] = [6 /* permission denied */, 7 /* authentication error */, 10 /* user unknown */, 11 /* too many tries */];

/// PAM asks its questions through this: every question is answered with the password.
extern "C" fn talk(count: c_int, messages: *mut *const Message, responses: *mut *mut Response, data: *mut c_void) -> c_int {
    if count <= 0 || messages.is_null() || responses.is_null() || data.is_null() {
        return CONV_ERR;
    }
    // SAFETY: PAM's contract: `count` messages, and an array of answers that PAM frees with
    // free(), each answer's text included; `data` is the password this module passed in.
    unsafe {
        let answers = libc::calloc(count as usize, std::mem::size_of::<Response>()) as *mut Response;
        if answers.is_null() {
            return BUF_ERR;
        }
        let password = data as *const c_char;
        for i in 0..count as usize {
            let message = *messages.add(i);
            if !message.is_null() && matches!((*message).style, PROMPT_ECHO_OFF | PROMPT_ECHO_ON) {
                (*answers.add(i)).text = libc::strdup(password);
            }
        }
        *responses = answers;
    }
    SUCCESS
}

/// The name of the user this program runs as.
pub fn user() -> Option<String> {
    // SAFETY: getpwuid's result is read at once, on this thread.
    unsafe {
        let entry = libc::getpwuid(libc::getuid());
        if entry.is_null() || (*entry).pw_name.is_null() {
            return None;
        }
        Some(CStr::from_ptr((*entry).pw_name).to_string_lossy().into_owned())
    }
}

/// Whether `password` is `user`'s. `Ok(false)`: it is not (or the system won't take more
/// tries just now). `Err`: it couldn't be checked at all.
pub fn check(user: &str, password: &str) -> Result<bool, String> {
    // The rules for logging in at this computer; a system may name others for remoteman.
    let service = std::env::var("REMOTEMAN_PAM_SERVICE").unwrap_or_else(|_| if std::path::Path::new("/etc/pam.d/remoteman").exists() { "remoteman".into() } else { "login".into() });
    let (Ok(service), Ok(user), Ok(password)) = (CString::new(service), CString::new(user), CString::new(password)) else { return Ok(false) };
    let conversation = Conversation { talk, data: password.as_ptr() as *mut c_void };
    let mut handle = std::ptr::null_mut();
    // SAFETY: PAM's calls in their order; `conversation` and `password` outlive the handle.
    unsafe {
        let started = pam_start(service.as_ptr(), user.as_ptr(), &conversation, &mut handle);
        if started != SUCCESS {
            return Err(format!("PAM wouldn't start ({started})"));
        }
        let mut result = pam_authenticate(handle, 0);
        if result == SUCCESS {
            // The account itself: not expired, not shut out.
            result = pam_acct_mgmt(handle, 0);
        }
        let said = CStr::from_ptr(pam_strerror(handle, result)).to_string_lossy().into_owned();
        pam_end(handle, result);
        match result {
            SUCCESS => Ok(true),
            code if WRONG.contains(&code) => Ok(false),
            _ => Err(said),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_that_does_not_exist_is_turned_down() {
        // (Never a real account: a wrong try there would count against its owner.)
        assert_eq!(check("remoteman-no-such-user", "not a password"), Ok(false));
        assert!(user().is_some_and(|u| !u.is_empty()));
    }
}
