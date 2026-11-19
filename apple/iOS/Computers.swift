// The computers this device knows, kept on the device: name, address, how its display sits on
// that computer, when it was added and last used, and a thumbnail of the last picture.
import SwiftUI
import UIKit

struct Computer: Codable, Identifiable, Equatable {
    var id = UUID()
    var name: String
    /// "name:port" or an address; the port may be left out.
    var address: String
    /// "linux", "macos", once the computer has said.
    var os: String?
    var placement: String = "extend"
    var added = Date()
    var lastUsed: Date?
    /// False until the first connection has gone through (which is when the pairing happens).
    var paired = false
    var fingerprint: String?
    /// Its network cards' addresses, as it said them on this network (for waking it).
    var macs: [String]?
    /// Send the wake signal before connecting.
    var wake: Bool?

    var osLine: String {
        switch os {
        case "linux": return "Linux"
        case "macos": return "macOS"
        case .some(let o) where !o.isEmpty: return o
        default: return address
        }
    }

    var placementChoice: ViewerSession.Placement {
        get { ViewerSession.Placement(rawValue: placement) ?? .extend }
        set { placement = newValue.rawValue }
    }

    /// "Used today, 09:12", "Used yesterday", "Used Friday", "Used 2 Nov"; nil if never used.
    func usedLine(short: Bool) -> String? {
        guard let lastUsed else { return nil }
        let cal = Calendar.current
        let f = DateFormatter()
        var text: String
        if cal.isDateInToday(lastUsed) {
            f.dateFormat = "HH:mm"
            text = "today, " + f.string(from: lastUsed)
        } else if cal.isDateInYesterday(lastUsed) {
            text = "yesterday"
        } else if let days = cal.dateComponents([.day], from: lastUsed, to: Date()).day, days < 7 {
            f.dateFormat = "EEEE"
            text = f.string(from: lastUsed)
        } else {
            f.dateFormat = "d MMM"
            text = f.string(from: lastUsed)
        }
        if short { return text.prefix(1).uppercased() + text.dropFirst() }
        return "Used " + text
    }
}

final class ComputerStore: ObservableObject {
    @Published private(set) var computers: [Computer] = []
    private let key = "computers"

    init() {
        if let data = UserDefaults.standard.data(forKey: key),
            let list = try? JSONDecoder().decode([Computer].self, from: data)
        {
            computers = list
        }
    }

    private func save() {
        if let data = try? JSONEncoder().encode(computers) { UserDefaults.standard.set(data, forKey: key) }
    }

    @discardableResult
    func add(name: String, address: String) -> Computer {
        let c = Computer(name: name.isEmpty ? address : name, address: address)
        computers.append(c)
        save()
        return c
    }

    func update(_ c: Computer) {
        guard let i = computers.firstIndex(where: { $0.id == c.id }) else { return }
        computers[i] = c
        save()
    }

    func forget(_ c: Computer) {
        computers.removeAll { $0.id == c.id }
        // Its key goes too (unless another entry is the same computer), so it must be paired again.
        if !computers.contains(where: { $0.address == c.address }) { ViewerSession.forget(host: c.address) }
        try? FileManager.default.removeItem(at: thumbnailURL(c))
        save()
    }

    func computer(_ id: UUID?) -> Computer? { computers.first { $0.id == id } }

    // MARK: Thumbnails (the last picture of each computer)

    private func thumbnailURL(_ c: Computer) -> URL {
        let dir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("remoteman/thumbnails", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent(c.id.uuidString + ".jpg")
    }

    func thumbnail(_ c: Computer) -> UIImage? { UIImage(contentsOfFile: thumbnailURL(c).path) }

    func saveThumbnail(_ image: CGImage, for c: Computer) {
        let target = CGSize(width: 640, height: 640 * CGFloat(image.height) / CGFloat(max(image.width, 1)))
        let small = UIGraphicsImageRenderer(size: target).image { _ in
            UIImage(cgImage: image).draw(in: CGRect(origin: .zero, size: target))
        }
        try? small.jpegData(compressionQuality: 0.8)?.write(to: thumbnailURL(c))
        objectWillChange.send()
    }
}
