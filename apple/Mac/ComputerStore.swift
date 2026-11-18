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

    func remove(_ id: SavedComputer.ID) {
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
