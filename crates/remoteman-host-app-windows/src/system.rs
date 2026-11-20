//! What the app asks of Windows itself: this PC's names and addresses, the file dialog, and the
//! icon files the notification area and the window take by path.

use std::net::Ipv4Addr;
use std::path::PathBuf;

use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, HWND};
use windows::Win32::NetworkManagement::IpHelper::{GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::Win32::System::SystemInformation::{ComputerNameDnsHostname, GetComputerNameExW};
use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
use windows::core::{HSTRING, PWSTR};

/// What this PC is called, as the host calls it unless told otherwise.
pub fn computer_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "this PC".into())
}

/// This PC's name on the network, in lower case (`den-pc`).
pub fn host_name() -> String {
    let mut size = 0u32;
    // SAFETY: the first call asks for the size; the second fills a buffer of that size.
    unsafe {
        let _ = GetComputerNameExW(ComputerNameDnsHostname, None, &mut size);
        let mut buffer = vec![0u16; size as usize + 1];
        if GetComputerNameExW(ComputerNameDnsHostname, Some(PWSTR(buffer.as_mut_ptr())), &mut size).is_ok() {
            return String::from_utf16_lossy(&buffer[..size as usize]).to_lowercase();
        }
    }
    computer_name().to_lowercase()
}

/// This PC's IPv4 addresses on the interfaces that are up, not the loopback.
pub fn ipv4_addresses() -> Vec<Ipv4Addr> {
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size = 16 * 1024u32;
    let mut buffer: Vec<u64>;
    loop {
        buffer = vec![0u64; size as usize / 8 + 1];
        // SAFETY: the buffer is `size` bytes long and 8-aligned, as the structures need.
        let result = unsafe { GetAdaptersAddresses(AF_INET.0 as u32, flags, None, Some(buffer.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH), &mut size) };
        if result == ERROR_BUFFER_OVERFLOW.0 {
            continue;
        }
        if result != 0 {
            return Vec::new();
        }
        break;
    }
    let mut found = Vec::new();
    let mut adapter = buffer.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    // SAFETY: a list Windows wrote into the buffer, which lives until the end of this function.
    unsafe {
        while let Some(a) = adapter.as_ref() {
            // IF_TYPE_SOFTWARE_LOOPBACK
            if a.OperStatus == IfOperStatusUp && a.IfType != 24 {
                let mut unicast = a.FirstUnicastAddress;
                while let Some(u) = unicast.as_ref() {
                    let sockaddr = u.Address.lpSockaddr;
                    if !sockaddr.is_null() && (*sockaddr).sa_family == AF_INET {
                        let v4 = &*(sockaddr as *const SOCKADDR_IN);
                        let ip = Ipv4Addr::from(u32::from_be(v4.sin_addr.S_un.S_addr));
                        if !ip.is_loopback() && !ip.is_link_local() {
                            found.push(ip);
                        }
                    }
                    unicast = u.Next;
                }
            }
            adapter = a.Next;
        }
    }
    found
}

/// What to enter on a device: this PC's name on the network, and its addresses. The host's
/// own port only when it isn't the usual one.
pub fn reach(address: &str) -> String {
    let port = address.rsplit(':').next().filter(|p| !p.is_empty() && *p != crate::host::DEFAULT_PORT.to_string()).map(|p| format!(":{p}")).unwrap_or_default();
    let also = ipv4_addresses().iter().map(|ip| format!("{ip}{port}")).collect::<Vec<_>>().join(", ");
    let mut text = format!("{}.local{port}", host_name());
    if !also.is_empty() {
        text.push_str(&format!("\nAlso: {also}"));
    }
    text.push_str("\nWhat to enter on a device to reach this PC");
    text
}

/// The icons the notification area and the window take by path: written once into a folder of
/// this user's (they are part of the exe, so nothing has to ship beside it).
pub struct Icons {
    pub sharing: String,
    pub connected: String,
    pub paused: String,
}

pub fn icons() -> Icons {
    let folder = std::env::temp_dir().join("remoteman-host-app");
    let _ = std::fs::create_dir_all(&folder);
    let write = |name: &str, bytes: &[u8]| -> String {
        let path: PathBuf = folder.join(name);
        if std::fs::read(&path).ok().as_deref() != Some(bytes)
            && let Err(e) = std::fs::write(&path, bytes)
        {
            crate::log::say(&format!("couldn't write {}: {e}", path.display()));
        }
        path.to_string_lossy().into_owned()
    };
    Icons {
        sharing: write("sharing.ico", include_bytes!("../assets/sharing.ico")),
        connected: write("connected.ico", include_bytes!("../assets/connected.ico")),
        paused: write("paused.ico", include_bytes!("../assets/paused.ico")),
    }
}

/// Windows' file dialog, on a thread of its own (it runs its own message loop, which must not
/// run inside the app's update), owned by the window when it is open. `chosen` gets the path.
pub fn pick_file(owner: isize, chosen: impl FnOnce(String) + Send + 'static) {
    let spawned = std::thread::Builder::new().name("file dialog".into()).spawn(move || {
        // SAFETY: COM on this thread for the dialog, undone at the end; the item's path is
        // freed with the allocator Windows made it with.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let picked = (|| -> windows::core::Result<Option<String>> {
                let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
                dialog.SetTitle(&HSTRING::from("Send a file to the device"))?;
                let owner = (owner != 0).then_some(HWND(owner as *mut _));
                if dialog.Show(owner).is_err() {
                    // Cancelled.
                    return Ok(None);
                }
                let path = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
                let text = path.to_string().unwrap_or_default();
                CoTaskMemFree(Some(path.0 as *const _));
                Ok(Some(text))
            })();
            CoUninitialize();
            match picked {
                Ok(Some(path)) => chosen(path),
                Ok(None) => crate::log::say("file dialog: cancelled"),
                Err(e) => crate::log::say(&format!("file dialog: {e}")),
            }
        }
    });
    if let Err(e) = spawned {
        crate::log::say(&format!("couldn't open the file dialog: {e}"));
    }
}
