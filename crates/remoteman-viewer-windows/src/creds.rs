//! A locked computer's password, kept on this PC when its user asks for that: in Windows'
//! Credential Manager, which the user's own sign-in protects. One entry a computer, named by
//! the computer's key, so a password is only ever offered to the computer it was given for.

use windows::Win32::Security::Credentials::{CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW, CredWriteW};
use windows::core::{HSTRING, PWSTR};

fn target(computer: &str) -> HSTRING {
    HSTRING::from(format!("remoteman:{computer}"))
}

/// The password kept for the computer with this key fingerprint, if there is one.
pub fn find(computer: &str) -> Option<String> {
    let mut found = std::ptr::null_mut();
    // SAFETY: the credential Windows hands out is read within its size and freed.
    unsafe {
        CredReadW(&target(computer), CRED_TYPE_GENERIC, None, &mut found).ok()?;
        let c = &*found;
        let password = String::from_utf8(std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize).to_vec()).ok();
        CredFree(found as *const _);
        password.filter(|p| !p.is_empty())
    }
}

/// Keep `password` for the computer; `name` is what Credential Manager shows beside it.
pub fn keep(computer: &str, name: &str, password: &str) -> windows::core::Result<()> {
    let (target, user) = (target(computer), HSTRING::from(name));
    let mut blob = password.as_bytes().to_vec();
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target.as_ptr() as *mut _),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: PWSTR(user.as_ptr() as *mut _),
        ..Default::default()
    };
    // SAFETY: everything the structure points at lives through the call.
    unsafe { CredWriteW(&credential, 0) }
}

pub fn forget(computer: &str) {
    // SAFETY: a call with a string that lives through it.
    unsafe {
        let _ = CredDeleteW(&target(computer), CRED_TYPE_GENERIC, None);
    }
}
