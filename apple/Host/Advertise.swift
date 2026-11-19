// Telling the network this Mac can be reached (docs/PROTOCOL.md, "Finding a host"): a DNS-SD
// service `_remoteman._udp` under the Mac's name, on the port the host listens on, with the
// text records `v`, `fp`, `os` and `mac`. Off until the owner turns it on: the first time,
// macOS asks whether the app may be found on the local network.
import Darwin
import Foundation
import IOKit
import dnssd

final class Advertise {
    private var service: DNSServiceRef?

    /// The hardware addresses of this Mac's own network cards. From the I/O registry: the
    /// socket interfaces give every app the same made-up address in their place.
    static var macs: [String] {
        var out: [String] = []
        var it: io_iterator_t = 0
        guard IOServiceGetMatchingServices(kIOMainPortDefault, IOServiceMatching("IOEthernetInterface"), &it) == KERN_SUCCESS else { return out }
        defer { IOObjectRelease(it) }
        while case let entry = IOIteratorNext(it), entry != 0 {
            defer { IOObjectRelease(entry) }
            var parent: io_registry_entry_t = 0
            guard IORegistryEntryGetParentEntry(entry, kIOServicePlane, &parent) == KERN_SUCCESS else { continue }
            defer { IOObjectRelease(parent) }
            guard let data = IORegistryEntryCreateCFProperty(parent, "IOMACAddress" as CFString, kCFAllocatorDefault, 0)?.takeRetainedValue() as? Data,
                data.count == 6
            else { continue }
            let text = data.map { String(format: "%02x", $0) }.joined(separator: ":")
            // Not the made-up addresses of bridges and tunnels (the locally administered bit).
            if data[0] & 0x02 == 0, !out.contains(text) { out.append(text) }
        }
        return out
    }

    /// Returns why not, if the service couldn't be registered.
    func start(name: String, port: UInt16, fingerprint: String) -> String? {
        stop()
        var records = ["v=0", "fp=\(fingerprint.lowercased().prefix(16))", "os=macos"]
        let macs = Self.macs
        if !macs.isEmpty { records.append("mac=\(macs.joined(separator: ","))") }
        var txt = [UInt8]()
        for r in records {
            let b = Array(r.utf8.prefix(255))
            txt.append(UInt8(b.count))
            txt.append(contentsOf: b)
        }
        var ref: DNSServiceRef?
        let err = DNSServiceRegister(
            &ref, 0, 0, name, "_remoteman._udp", nil, nil, port.bigEndian, UInt16(txt.count), txt,
            { _, _, error, name, _, _, _ in
                // -65570: macOS isn't letting the app use the local network.
                if error == kDNSServiceErr_NoError {
                    hostLog("host: the network has it as \"\(name.map { String(cString: $0) } ?? "?")\"")
                } else {
                    hostLog("host: the network refused the name (\(error)\(error == -65570 ? ": allow remoteman host under System Settings, Privacy & Security, Local Network" : ""))")
                }
            }, nil)
        guard err == kDNSServiceErr_NoError, let ref else { return "DNS-SD refused (\(err))" }
        DNSServiceSetDispatchQueue(ref, .main)
        service = ref
        return nil
    }

    func stop() {
        if let s = service { DNSServiceRefDeallocate(s) }
        service = nil
    }

    var on: Bool { service != nil }
}
