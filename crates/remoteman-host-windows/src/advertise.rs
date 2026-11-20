//! This host on the network, for viewers to find (docs/PROTOCOL.md, "Finding a host"): a DNS-SD
//! service `_remoteman._udp` through Windows' own responder (`DnsServiceRegister`), named after
//! the computer, on the host's port, with the text records `v`, `fp`, `os` and `mac`, as the
//! Mac and Linux hosts give. Withdrawn when the host stops.

use std::ffi::c_void;

use windows::Win32::NetworkManagement::Dns::{
    DNS_SERVICE_INSTANCE, DNS_SERVICE_REGISTER_REQUEST, DnsServiceConstructInstance, DnsServiceDeRegister, DnsServiceFreeInstance, DnsServiceRegister,
};
use windows::Win32::NetworkManagement::IpHelper::{GAA_FLAG_INCLUDE_GATEWAYS, GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH};
use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};
use windows::core::PCWSTR;

/// What `DnsServiceRegister` says when the registration has been taken and goes on in the background.
const DNS_REQUEST_PENDING: i32 = 9506;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// This PC's own network cards that are up: the IPv4 address of the one with a gateway (the
/// LAN, not a VPN's tunnel or a virtual switch's), and every physical card's hardware address
/// (not the made-up, locally administered ones of virtual adapters).
fn cards() -> (Option<[u8; 4]>, Vec<String>) {
    const ETHERNET: u32 = 6;
    const WIFI: u32 = 71;
    let mut size = 32 * 1024u32;
    let mut buffer = vec![0u8; size as usize];
    // SAFETY: the buffer is sized as said (grown once if Windows asks for more).
    let mut got = unsafe { GetAdaptersAddresses(AF_INET.0 as u32, GAA_FLAG_INCLUDE_GATEWAYS, None, Some(buffer.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH), &mut size) };
    if got == 111 {
        // ERROR_BUFFER_OVERFLOW
        buffer = vec![0u8; size as usize];
        got = unsafe { GetAdaptersAddresses(AF_INET.0 as u32, GAA_FLAG_INCLUDE_GATEWAYS, None, Some(buffer.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH), &mut size) };
    }
    let (mut lan, mut macs) = (None, Vec::new());
    if got != 0 {
        return (lan, macs);
    }
    let mut at = buffer.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    // SAFETY: a list Windows wrote into the buffer, read while the buffer lives.
    unsafe {
        while let Some(a) = at.as_ref() {
            at = a.Next;
            let up = a.OperStatus.0 == 1;
            let description = a.Description.to_string().unwrap_or_default().to_lowercase();
            let virtual_card = ["hyper-v", "virtual", "vmware", "virtualbox", "tap-", "wintun", "tailscale", "loopback"].iter().any(|v| description.contains(v));
            if !up || !(a.IfType == ETHERNET || a.IfType == WIFI) || virtual_card {
                continue;
            }
            if a.PhysicalAddressLength == 6 && a.PhysicalAddress[0] & 0x02 == 0 {
                let mac = a.PhysicalAddress[..6].iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":");
                if !macs.contains(&mac) {
                    macs.push(mac);
                }
            }
            if lan.is_none() && !a.FirstGatewayAddress.is_null() {
                if let Some(u) = a.FirstUnicastAddress.as_ref() {
                    let sa = u.Address.lpSockaddr;
                    if !sa.is_null() && (*sa).sa_family == AF_INET {
                        lan = Some((*(sa as *const SOCKADDR_IN)).sin_addr.S_un.S_un_b).map(|b| [b.s_b1, b.s_b2, b.s_b3, b.s_b4]);
                    }
                }
            }
        }
    }
    (lan, macs)
}

/// The service, registered until dropped.
pub struct Advert {
    request: DNS_SERVICE_REGISTER_REQUEST,
    instance: *mut DNS_SERVICE_INSTANCE,
}

// SAFETY: Windows' handles to the registration, used only to withdraw it.
unsafe impl Send for Advert {}
unsafe impl Sync for Advert {}

impl Drop for Advert {
    fn drop(&mut self) {
        // SAFETY: the request and instance registered in `start`.
        unsafe {
            DnsServiceDeRegister(&self.request, None);
            std::thread::sleep(std::time::Duration::from_millis(300));
            DnsServiceFreeInstance(self.instance);
        }
        tracing::info!("withdrawn from the network's list of computers (DNS-SD)");
    }
}

unsafe extern "system" fn registered(status: u32, _context: *const c_void, instance: *const DNS_SERVICE_INSTANCE) {
    if status != 0 {
        tracing::warn!(status, "Windows couldn't announce this host on the network (DNS-SD)");
    }
    if !instance.is_null() {
        // SAFETY: Windows hands its copy over to be freed.
        unsafe { DnsServiceFreeInstance(instance) };
    }
}

/// Announce this host. `None` (and said why) if Windows wouldn't.
pub fn start(name: &str, port: u16, fingerprint: &str) -> Option<Advert> {
    let (lan, macs) = cards();
    let Some(ip) = lan else {
        tracing::info!("no network card with a gateway: not announced on the network (DNS-SD)");
        return None;
    };
    let fp: String = fingerprint.to_lowercase().chars().take(16).collect();
    let mut records = vec![("v", "0".to_string()), ("fp", fp), ("os", "windows".to_string())];
    if !macs.is_empty() {
        records.push(("mac", macs.join(",")));
    }
    // An instance name is one DNS label: no dots in it.
    let label: String = name.replace('.', "-");
    let computer = std::env::var("COMPUTERNAME").unwrap_or_else(|_| label.clone());
    let full = wide(&format!("{label}._remoteman._udp.local"));
    let host = wide(&format!("{computer}.local"));
    let keys: Vec<Vec<u16>> = records.iter().map(|r| wide(r.0)).collect();
    let values: Vec<Vec<u16>> = records.iter().map(|r| wide(&r.1)).collect();
    let key_ptrs: Vec<PCWSTR> = keys.iter().map(|k| PCWSTR(k.as_ptr())).collect();
    let value_ptrs: Vec<PCWSTR> = values.iter().map(|v| PCWSTR(v.as_ptr())).collect();
    let ip4 = u32::from_ne_bytes(ip);
    // SAFETY: every string and array lives until the calls return; Windows copies them.
    unsafe {
        let instance = DnsServiceConstructInstance(PCWSTR(full.as_ptr()), PCWSTR(host.as_ptr()), Some(&ip4), None, port, 0, 0, records.len() as u32, key_ptrs.as_ptr(), value_ptrs.as_ptr());
        if instance.is_null() {
            tracing::warn!("couldn't make the network announcement (DNS-SD)");
            return None;
        }
        let request = DNS_SERVICE_REGISTER_REQUEST { Version: 1, InterfaceIndex: 0, pServiceInstance: instance, pRegisterCompletionCallback: Some(registered), pQueryContext: std::ptr::null_mut(), ..Default::default() };
        let status = DnsServiceRegister(&request, None);
        if status as i32 != DNS_REQUEST_PENDING {
            tracing::warn!(status, "Windows wouldn't announce this host on the network (DNS-SD)");
            DnsServiceFreeInstance(instance);
            return None;
        }
        tracing::info!(name = label, port, address = format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]), cards = macs.len(), "announced on the network for viewers to find (DNS-SD _remoteman._udp)");
        Some(Advert { request, instance })
    }
}
