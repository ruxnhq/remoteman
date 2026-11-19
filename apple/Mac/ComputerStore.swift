// The computers this Mac knows: a small list kept as JSON in Application Support/remoteman.
// The keys themselves are the core's business (it keeps them beside this file); this is only
// what the interface shows and remembers about each computer.
import Foundation
import SwiftUI

struct SavedComputer: Codable, Identifiable, Equatable {
    var id = UUID()
    var name: String
    /// A name or an IP address, with ":port" when it isn't 7791.
    var address: String
    /// ViewerSession.Placement's raw value.
    var placement = ViewerSession.Placement.extend.rawValue
    var added = Date()
    /// Ask the computer to accept this Mac as a new device on the next connection (it has to be
    /// in pairing mode). Cleared by the first connection that works.
    var pairOnConnect = false
    /// When the first connection worked.
    var paired: Date?
    var lastUsed: Date?
    var lastDuration: TimeInterval?
    /// As the computer reported them on the last connection.
    var os = ""
    var fingerprint = ""
    /// Its network cards' addresses, as it said them on this network (for waking it).
    var macs: [String] = []
    /// Send the wake signal before connecting.
    var wake = false
    /// Touch ID before connecting: "" follows the setting in General, "always", "never".
    var auth = ""
    /// "" any way in (a running session, or the computer's login screen); "attach" only a running session.
    var wayIn = ""
    /// The name of the display of this Mac a session opens on; "" wherever the window is.
    var screen = ""

    init(name: String, address: String) {
        self.name = name
        self.address = address
    }

    /// Fields a file doesn't have keep their defaults, so an older list still loads.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decode(String.self, forKey: .name)
        address = try c.decode(String.self, forKey: .address)
        id = try c.decodeIfPresent(UUID.self, forKey: .id) ?? UUID()
        placement = try c.decodeIfPresent(String.self, forKey: .placement) ?? ViewerSession.Placement.extend.rawValue
        added = try c.decodeIfPresent(Date.self, forKey: .added) ?? Date()
        pairOnConnect = try c.decodeIfPresent(Bool.self, forKey: .pairOnConnect) ?? false
        paired = try c.decodeIfPresent(Date.self, forKey: .paired)
        lastUsed = try c.decodeIfPresent(Date.self, forKey: .lastUsed)
        lastDuration = try c.decodeIfPresent(TimeInterval.self, forKey: .lastDuration)
        os = try c.decodeIfPresent(String.self, forKey: .os) ?? ""
        fingerprint = try c.decodeIfPresent(String.self, forKey: .fingerprint) ?? ""
        macs = try c.decodeIfPresent([String].self, forKey: .macs) ?? []
        wake = try c.decodeIfPresent(Bool.self, forKey: .wake) ?? false
        auth = try c.decodeIfPresent(String.self, forKey: .auth) ?? ""
        wayIn = try c.decodeIfPresent(String.self, forKey: .wayIn) ?? ""
        screen = try c.decodeIfPresent(String.self, forKey: .screen) ?? ""
    }

    var place: ViewerSession.Placement {
        get { ViewerSession.Placement(rawValue: placement) ?? .extend }
        set { placement = newValue.rawValue }
    }

    /// The symbol for the kind of computer, as far as its system tells.
    var symbol: String { os.lowercased().contains("mac") ? "macmini" : "desktopcomputer" }

    var osLabel: String {
        switch os.lowercased() {
        case "": return ""
        case "macos": return "macOS"
        case "linux": return "Linux"
        case "windows": return "Windows"
        default: return os
        }
    }
}

extension ViewerSession.Placement {
    /// As the design's pop-ups and menus spell it.
    var title: String {
        switch self {
        case .extend: return "Extra Display"
        case .main: return "Main Display"
        case .only: return "Only Display"
        case .mirror: return "Mirror Its Main Monitor"
        }
    }
}

final class ComputerStore: ObservableObject {
    @Published private(set) var computers: [SavedComputer] = []
    private let url: URL?

    /// The list on disk. A first run takes over the address the old connect form remembered.
    init() {
        let file = URL(fileURLWithPath: ViewerSession.stateDir).appendingPathComponent("computers.json")
        url = file
        if let data = try? Data(contentsOf: file) {
            let decoder = JSONDecoder()
            decoder.dateDecodingStrategy = .iso8601
            computers = (try? decoder.decode([SavedComputer].self, from: data)) ?? []
        } else if let old = UserDefaults.standard.string(forKey: "host")?.trimmingCharacters(in: .whitespaces), !old.isEmpty {
            var c = SavedComputer(name: old.split(separator: ":").first.map(String.init) ?? old, address: old)
            c.place = .saved(for: old)
            computers = [c]
            save()
        }
    }

    /// A list that lives only in memory, for previews.
    init(preview: [SavedComputer]) {
        url = nil
        computers = preview
    }

    subscript(id: SavedComputer.ID?) -> SavedComputer? { computers.first { $0.id == id } }

    func add(_ c: SavedComputer) {
        computers.append(c)
        save()
    }

    func update(_ id: SavedComputer.ID, _ change: (inout SavedComputer) -> Void) {
        guard let i = computers.firstIndex(where: { $0.id == id }) else { return }
        change(&computers[i])
        save()
    }

    // MARK: The last picture of each computer

    private static func thumbnailURL(_ id: SavedComputer.ID) -> URL? {
        guard let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first else { return nil }
        let dir = base.appendingPathComponent("remoteman/thumbnails", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent(id.uuidString + ".jpg")
    }

    func thumbnail(_ id: SavedComputer.ID) -> NSImage? { Self.thumbnailURL(id).flatMap { NSImage(contentsOf: $0) } }

    func saveThumbnail(_ image: CGImage, for id: SavedComputer.ID) {
        guard let url = Self.thumbnailURL(id) else { return }
        // Small: it is shown at 336 points across.
        let k = min(1, 1008 / CGFloat(image.width))
        let w = Int(CGFloat(image.width) * k), h = Int(CGFloat(image.height) * k)
        guard let ctx = CGContext(data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0, space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)
        else { return }
        ctx.interpolationQuality = .high
        ctx.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
        guard let small = ctx.makeImage() else { return }
        let data = NSBitmapImageRep(cgImage: small).representation(using: .jpeg, properties: [.compressionFactor: 0.8])
        try? data?.write(to: url)
        objectWillChange.send()
    }

    func remove(_ id: SavedComputer.ID) {
        if let url = Self.thumbnailURL(id) { try? FileManager.default.removeItem(at: url) }
        let address = self[id]?.address
        computers.removeAll { $0.id == id }
        if let address, !computers.contains(where: { $0.address == address }) { ViewerSession.forget(host: address) }
        save()
    }

    private func save() {
        guard let url else { return }
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        if let data = try? encoder.encode(computers) { try? data.write(to: url, options: .atomic) }
    }
}

/// Dates and durations as the design words them.
enum Wording {
    /// "Today at 09:12", "Friday at 18:40", "2 November at 10:05".
    static func moment(_ date: Date, now: Date = Date()) -> String {
        let cal = Calendar.current
        let time = date.formatted(date: .omitted, time: .shortened)
        if cal.isDate(date, inSameDayAs: now) { return "Today at \(time)" }
        if let y = cal.date(byAdding: .day, value: -1, to: now), cal.isDate(date, inSameDayAs: y) { return "Yesterday at \(time)" }
        if let days = cal.dateComponents([.day], from: date, to: now).day, days < 7 {
            return "\(date.formatted(.dateTime.weekday(.wide))) at \(time)"
        }
        return "\(day(date)) at \(time)"
    }

    /// "12 October 2026".
    static func day(_ date: Date) -> String { date.formatted(.dateTime.day().month(.wide).year()) }

    /// "1 h 40 min", "25 min", "under a minute".
    static func length(_ seconds: TimeInterval) -> String {
        let minutes = Int(seconds / 60)
        if minutes < 1 { return "under a minute" }
        if minutes < 60 { return "\(minutes) min" }
        return minutes % 60 == 0 ? "\(minutes / 60) h" : "\(minutes / 60) h \(minutes % 60) min"
    }

    static func lastUsed(_ c: SavedComputer) -> String {
        guard let used = c.lastUsed else { return "Never" }
        guard let d = c.lastDuration else { return moment(used) }
        return "\(moment(used)), for \(length(d))"
    }
}
