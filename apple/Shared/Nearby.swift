// Computers running remoteman on this network (docs/PROTOCOL.md, "Finding a host"): a DNS-SD
// service `_remoteman._udp` with the computer's name, its port, and text records `v`, `fp`
// (the first 16 hex digits of its key's fingerprint), `os` and `mac`.
//
// Looking is only ever started by the person (the first look makes the system ask whether the
// app may find devices on the local network); once they have asked for it, it is remembered.
// Seeing nothing says nothing: a paired computer reached over a VPN is never seen here.
import Foundation
import Network
import dnssd

final class Nearby: ObservableObject {
    struct Found: Identifiable, Equatable {
        var id: String { name }
        var name: String
        var fp = ""
        var os = ""
        var macs: [String] = []
        /// "address:port" once the service has been resolved.
        var address: String?
    }

    static let shared = Nearby()
    static let wantedKey = "nearby.looking"
    static let type = "_remoteman._udp"

    @Published private(set) var found: [Found] = []
    @Published private(set) var looking = false
    /// Set when the system refused (Local Network switched off for the app in Settings).
    @Published private(set) var refused = false

    private var browser: NWBrowser?
    private var resolving: [String: Resolving] = [:]
    private var addresses: [String: String] = [:]

    /// Whether the person has asked for this before (so it may start by itself).
    static var wanted: Bool {
        get { UserDefaults.standard.bool(forKey: wantedKey) }
        set { UserDefaults.standard.set(newValue, forKey: wantedKey) }
    }

    /// Start looking. Call only because the person asked, or when `wanted` is already true.
    func look() {
        guard browser == nil else { return }
        Self.wanted = true
        let params = NWParameters()
        params.includePeerToPeer = false
        let b = NWBrowser(for: .bonjourWithTXTRecord(type: Self.type, domain: nil), using: params)
        b.stateUpdateHandler = { [weak self] state in
            DispatchQueue.main.async {
                guard let self else { return }
                switch state {
                case .ready:
                    self.looking = true
                    self.refused = false
                case .waiting(let e), .failed(let e):
                    // -65570: the app isn't allowed on the local network.
                    if case .dns(let code) = e, code == -65570 { self.refused = true }
                    self.looking = false
                default: break
                }
            }
        }
        b.browseResultsChangedHandler = { [weak self] results, _ in
            DispatchQueue.main.async { self?.took(results) }
        }
        browser = b
        b.start(queue: .main)
    }

    func stop() {
        browser?.cancel()
        browser = nil
        resolving.values.forEach { $0.cancel() }
        resolving = [:]
        addresses = [:]
        found = []
        looking = false
    }

    /// The computer with this key, if it is seen now. `fingerprint` is the whole one.
    func seen(fingerprint: String?) -> Found? {
        guard let f = fingerprint?.lowercased(), f.count >= 16 else { return nil }
        return found.first { !$0.fp.isEmpty && f.hasPrefix($0.fp) }
    }

    private func took(_ results: Set<NWBrowser.Result>) {
        var list: [Found] = []
        for r in results {
            guard case .service(let name, _, _, _) = r.endpoint else { continue }
            var f = Found(name: name)
            if case .bonjour(let txt) = r.metadata {
                f.fp = (txt["fp"] ?? "").lowercased()
                f.os = txt["os"] ?? ""
                f.macs = (txt["mac"] ?? "").split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces).lowercased() }.filter { !$0.isEmpty }
            }
            // The same computer on two interfaces is one computer.
            if let i = list.firstIndex(where: { $0.name == name }) {
                if list[i].fp.isEmpty { list[i] = f }
                continue
            }
            f.address = addresses[name]
            list.append(f)
            if addresses[name] == nil, resolving[name] == nil { resolve(name, r.endpoint) }
        }
        for name in resolving.keys where !list.contains(where: { $0.name == name }) {
            resolving[name]?.cancel()
            resolving[name] = nil
        }
        addresses = addresses.filter { a in list.contains { $0.name == a.key } }
        found = list.sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    /// The service's host name and port, as DNS-SD resolves it ("blackbox.local:7791").
    private func resolve(_ name: String, _ endpoint: NWEndpoint) {
        guard case .service(let service, let type, let domain, _) = endpoint else { return }
        let box = ResolveBox(name: name, owner: self)
        var ref: DNSServiceRef?
        let context = Unmanaged.passRetained(box)
        let err = DNSServiceResolve(
            &ref, 0, 0, service, type, domain,
            { _, _, _, error, _, target, port, _, _, context in
                guard let context else { return }
                let box = Unmanaged<ResolveBox>.fromOpaque(context).takeUnretainedValue()
                guard error == kDNSServiceErr_NoError, let target else { return }
                var host = String(cString: target)
                if host.hasSuffix(".") { host.removeLast() }
                let address = "\(host):\(UInt16(bigEndian: port))"
                DispatchQueue.main.async { box.owner?.resolved(box.name, address) }
            }, context.toOpaque())
        guard err == kDNSServiceErr_NoError, let ref else {
            context.release()
            return
        }
        DNSServiceSetDispatchQueue(ref, .main)
        resolving[name] = Resolving(ref: ref, box: context)
    }

    private func resolved(_ name: String, _ address: String) {
        addresses[name] = address
        if let i = found.firstIndex(where: { $0.name == name }) { found[i].address = address }
        resolving[name]?.cancel()
        resolving[name] = nil
    }
}

private final class ResolveBox {
    let name: String
    weak var owner: Nearby?
    init(name: String, owner: Nearby) {
        self.name = name
        self.owner = owner
    }
}

private struct Resolving {
    let ref: DNSServiceRef
    let box: Unmanaged<ResolveBox>
    func cancel() {
        DNSServiceRefDeallocate(ref)
        box.release()
    }
}

/// Waking a sleeping computer: the usual magic packet (six bytes of 0xFF, then the card's
/// address sixteen times) to UDP ports 9 and 7, broadcast on every network this device is on,
/// and to the computer's last address as well.
enum Wake {
    /// How many packets left. 0: none could be sent (no network, or the system refused).
    @discardableResult
    static func send(macs: [String], lastAddress: String? = nil) -> Int {
        var sent = 0
        var targets: [in_addr] = [in_addr(s_addr: INADDR_BROADCAST.bigEndian)]
        // Each network's own broadcast address.
        var list: UnsafeMutablePointer<ifaddrs>?
        if getifaddrs(&list) == 0 {
            var p = list
            while let a = p {
                let flags = Int32(a.pointee.ifa_flags)
                if flags & IFF_UP != 0, flags & IFF_BROADCAST != 0, flags & IFF_LOOPBACK == 0, let b = a.pointee.ifa_dstaddr,
                    b.pointee.sa_family == sa_family_t(AF_INET)
                {
                    let addr = b.withMemoryRebound(to: sockaddr_in.self, capacity: 1) { $0.pointee.sin_addr }
                    if !targets.contains(where: { $0.s_addr == addr.s_addr }) { targets.append(addr) }
                }
                p = a.pointee.ifa_next
            }
            freeifaddrs(list)
        }
        if var host = lastAddress {
            if let colon = host.lastIndex(of: ":"), !host.hasPrefix("[") { host = String(host[..<colon]) }
            var a = in_addr()
            if inet_pton(AF_INET, host, &a) == 1 { targets.append(a) }
        }
        let s = socket(AF_INET, SOCK_DGRAM, 0)
        guard s >= 0 else { return 0 }
        defer { close(s) }
        var yes: Int32 = 1
        setsockopt(s, SOL_SOCKET, SO_BROADCAST, &yes, socklen_t(MemoryLayout<Int32>.size))
        for mac in macs {
            let bytes = mac.split(separator: ":").compactMap { UInt8($0, radix: 16) }
            guard bytes.count == 6 else { continue }
            let packet = [UInt8](repeating: 0xFF, count: 6) + (0..<16).flatMap { _ in bytes }
            for t in targets {
                for port: UInt16 in [9, 7] {
                    var to = sockaddr_in()
                    to.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
                    to.sin_family = sa_family_t(AF_INET)
                    to.sin_port = port.bigEndian
                    to.sin_addr = t
                    let n = withUnsafePointer(to: &to) {
                        $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                            sendto(s, packet, packet.count, 0, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
                        }
                    }
                    if n == packet.count { sent += 1 }
                }
            }
        }
        return sent
    }
}
