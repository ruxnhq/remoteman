// The clipboard shared both ways, and files sent to and taken from the computer. Both travel
// as blobs (docs/PROTOCOL.md, "Features").
import Foundation

#if os(macOS)
    import AppKit
#else
    import UIKit
#endif

/// What is on this device's clipboard, in the two forms the protocol carries.
private enum Clip {
    static let text = "text/plain;charset=utf-8"
    static let png = "image/png"
    static let most = 8 << 20

    static var changeCount: Int {
        #if os(macOS)
            NSPasteboard.general.changeCount
        #else
            UIPasteboard.general.changeCount
        #endif
    }

    /// Text if there is any, else a picture as PNG. On iPadOS and iOS reading what another app
    /// copied makes the system ask the user first, each time, unless they allowed it in Settings.
    static func read() -> (type: String, data: Data)? {
        #if os(macOS)
            let pb = NSPasteboard.general
            if let s = pb.string(forType: .string), !s.isEmpty { return (text, Data(s.utf8)) }
            if let d = pb.data(forType: .png) { return (png, d) }
            if let d = pb.data(forType: .tiff), let rep = NSBitmapImageRep(data: d), let p = rep.representation(using: .png, properties: [:]) {
                return (png, p)
            }
            return nil
        #else
            let pb = UIPasteboard.general
            if pb.hasStrings, let s = pb.string, !s.isEmpty { return (text, Data(s.utf8)) }
            if pb.hasImages, let p = pb.image?.pngData() { return (png, p) }
            return nil
        #endif
    }

    static func write(type: String, data: Data) -> Bool {
        #if os(macOS)
            let pb = NSPasteboard.general
            if type.hasPrefix("text/plain"), let s = String(data: data, encoding: .utf8) {
                pb.clearContents()
                return pb.setString(s, forType: .string)
            }
            if type == png {
                pb.clearContents()
                return pb.setData(data, forType: .png)
            }
            return false
        #else
            if type.hasPrefix("text/plain"), let s = String(data: data, encoding: .utf8) {
                UIPasteboard.general.string = s
                return true
            }
            if type == png, let image = UIImage(data: data) {
                UIPasteboard.general.image = image
                return true
            }
            return false
        #endif
    }
}

/// A file on its way to or from the computer, for the app to show.
struct Transfer: Identifiable, Equatable {
    enum State: Equatable {
        case moving
        case done(String)
        case failed(String)
    }
    let id: Int
    let name: String
    let size: Int
    let incoming: Bool
    var moved = 0
    var state = State.moving
}

/// Clipboard and files for one connection. Made on the main thread; `blob` and `fileResult`
/// are called on the core's thread.
final class Sharing {
    private weak var client: CoreClient?
    private let work = DispatchQueue(label: "remoteman.sharing")
    /// Called on the main thread whenever a transfer changes.
    var transferChanged: ((Transfer) -> Void)?
    /// Where files from the computer are put.
    static var received: URL {
        #if os(macOS)
            FileManager.default.urls(for: .downloadsDirectory, in: .userDomainMask)[0]
        #else
            FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        #endif
    }

    init(client: CoreClient) { self.client = client }

    /// For tests: what clipboard sharing did and when (kinds and sizes, never content).
    private(set) var log: [String] = []
    private let logLock = NSLock()
    private let began = Date()
    private func note(_ s: String) {
        logLock.lock()
        log.append(String(format: "+%.2f s %@", Date().timeIntervalSince(began), s))
        logLock.unlock()
    }

    /// For tests: put text on this device's clipboard, and say what is on it without showing it.
    static var testText: Data?
    static func copyForTest(_ text: String) {
        testText = Data(text.utf8)
        _ = Clip.write(type: Clip.text, data: Data(text.utf8))
    }
    private var ticks = 0, idleTicks = 0
    var tickReport: String { "looked at the clipboard \(ticks) times, \(idleTicks) of them while the app wasn't active; change count \(Clip.changeCount), last seen \(seenCount)" }
    static func clipboardSummary(expecting: String?) -> String {
        guard let c = Clip.read() else { return "nothing readable" }
        let same = expecting.map { c.data == Data($0.utf8) ? ", equal to the test text" : ", not the test text" } ?? ""
        return "\(c.type), \(c.data.count) bytes\(same)"
    }

    // MARK: Clipboard

    private var timer: Timer?
    private var seenCount = 0
    private var sentSeq = 0
    private var takenSeq = -1
    /// What last went to the computer and what last came from it: neither is sent (back) again.
    private var lastSent: Data?
    private var lastTaken: Data?
    private(set) var clipboardOn = false

    /// On the main thread. `sendCurrent`: also send what is on the clipboard now.
    func setClipboard(_ on: Bool, sendCurrent: Bool) {
        guard on != clipboardOn else { return }
        clipboardOn = on
        note("sharing \(on ? "on" : "off")")
        client?.send(["t": "clipboard", "on": on])
        timer?.invalidate()
        timer = nil
        guard on else { return }
        seenCount = sendCurrent ? -1 : Clip.changeCount
        timer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in self?.lookAtClipboard() }
    }

    /// For tests: look now instead of at the next half second.
    func lookNow() { if clipboardOn { lookAtClipboard() } }

    private func lookAtClipboard() {
        ticks += 1
        #if os(iOS)
            guard UIApplication.shared.applicationState == .active else {
                idleTicks += 1
                return
            }
        #endif
        let count = Clip.changeCount
        guard count != seenCount else { return }
        seenCount = count
        guard let clip = Clip.read() else { return note("the clipboard changed here; nothing readable on it") }
        guard clip.data.count <= Clip.most, clip.data != lastSent, clip.data != lastTaken else {
            return note("the clipboard changed here, \(clip.data.count) bytes; not sent (too large, or what was last sent or taken)")
        }
        lastSent = clip.data
        sentSeq += 1
        let header: [String: Any] = ["seq": sentSeq, "type": clip.type, "size": clip.data.count]
        let client = self.client
        note("sending seq \(sentSeq), \(clip.type), \(clip.data.count) bytes")
        work.async { [weak self] in
            var rest: Data? = clip.data
            let ok = client?.sendBlob(kind: CoreClient.clipboardKind, header: header) {
                defer { rest = nil }
                return rest
            }
            self?.note("seq \(header["seq"] ?? 0) \(ok == true ? "sent whole" : ok == false ? "NOT sent (open, write or finish failed)" : "not sent, no connection")")
        }
    }

    // MARK: Blobs from the computer (core thread)

    private struct Incoming {
        var kind: UInt8
        var header: [String: Any]
        var data = Data()  // a clipboard's content
        var file: FileHandle?
        var part: URL?
        var transfer: Transfer?
        var failed: String?
    }
    private var incoming: [UInt32: Incoming] = [:]

    func blob(what: UInt8, id: UInt32, kind: UInt8, header: [String: Any], data: UnsafeRawBufferPointer, complete: Bool) {
        switch what {
        case 0:
            var b = Incoming(kind: kind, header: header)
            if kind == CoreClient.fileKind { openFile(&b, id: id) }
            incoming[id] = b
        case 1:
            guard var b = incoming[id] else { return }
            if b.kind == CoreClient.clipboardKind {
                if b.data.count + data.count <= Clip.most { b.data.append(contentsOf: data) } else { b.failed = "too large" }
            } else if let f = b.file, b.failed == nil {
                do {
                    try f.write(contentsOf: Data(data))
                    b.transfer?.moved += data.count
                    if let t = b.transfer, t.moved % (1 << 20) < data.count { report(t) }
                } catch {
                    b.failed = "This device couldn't save it: \(error.localizedDescription)"
                }
            }
            incoming[id] = b
        default:
            guard let b = incoming.removeValue(forKey: id) else { return }
            if b.kind == CoreClient.clipboardKind {
                if complete, b.failed == nil { takeClipboard(b) }
            } else {
                closeFile(b, complete: complete)
            }
        }
    }

    private func takeClipboard(_ b: Incoming) {
        let seq = (b.header["seq"] as? NSNumber)?.intValue ?? 0
        let type = b.header["type"] as? String ?? Clip.text
        let data = b.data
        DispatchQueue.main.async { [self] in
            // Stale, or this device's own copy coming back after sharing was switched on again.
            guard clipboardOn, seq > takenSeq, data != lastSent else {
                return note("from the computer: seq \(seq), \(data.count) bytes; ignored (\(!clipboardOn ? "sharing is off" : seq <= takenSeq ? "stale" : "what this device sent"))")
            }
            note("from the computer: seq \(seq), \(type), \(data.count) bytes\(data == Sharing.testText ? " (equal to the last test text)" : ""); put on the clipboard")
            takenSeq = seq
            lastTaken = data
            if Clip.write(type: type, data: data) { seenCount = Clip.changeCount }
        }
    }

    // MARK: Files

    private var nextFile = 1
    private var outgoing: [Int: Transfer] = [:]  // main thread

    /// A bare file name that is safe to use: no folders, no leading dots, no control characters.
    static func clean(_ name: String) -> String {
        var n = name.components(separatedBy: CharacterSet(charactersIn: "/\\:")).last ?? ""
        n = String(n.unicodeScalars.filter { !CharacterSet.controlCharacters.contains($0) })
        while n.hasPrefix(".") { n.removeFirst() }
        n = n.trimmingCharacters(in: .whitespaces)
        return n.isEmpty ? "file" : String(n.prefix(200))
    }

    /// A path in `dir` for `name` that nothing is at yet ("report 2.pdf").
    static func free(_ name: String, in dir: URL) -> URL {
        let base = (name as NSString).deletingPathExtension, ext = (name as NSString).pathExtension
        var url = dir.appendingPathComponent(name)
        var n = 2
        while FileManager.default.fileExists(atPath: url.path) {
            url = dir.appendingPathComponent(ext.isEmpty ? "\(base) \(n)" : "\(base) \(n).\(ext)")
            n += 1
        }
        return url
    }

    private func openFile(_ b: inout Incoming, id: UInt32) {
        let name = Self.clean(b.header["name"] as? String ?? "")
        let size = (b.header["size"] as? NSNumber)?.intValue ?? 0
        b.transfer = Transfer(id: -Int(id) - 1, name: name, size: size, incoming: true)
        let part = Self.received.appendingPathComponent(".\(name).\(id).part")
        do {
            try FileManager.default.createDirectory(at: Self.received, withIntermediateDirectories: true)
            guard FileManager.default.createFile(atPath: part.path, contents: nil) else { throw CocoaError(.fileWriteNoPermission) }
            b.file = try FileHandle(forWritingTo: part)
            b.part = part
        } catch {
            b.failed = "This device couldn't save it: \(error.localizedDescription)"
        }
        if let t = b.transfer { report(t) }
    }

    private func closeFile(_ b: Incoming, complete: Bool) {
        try? b.file?.close()
        var t = b.transfer
        var ok = false
        var message = b.failed ?? "The computer gave the file up."
        if complete, b.failed == nil, let part = b.part, let name = t?.name {
            let final = Self.free(name, in: Self.received)
            do {
                try FileManager.default.moveItem(at: part, to: final)
                ok = true
                message = final.lastPathComponent
            } catch {
                message = "This device couldn't save it: \(error.localizedDescription)"
            }
        }
        if !ok, let part = b.part { try? FileManager.default.removeItem(at: part) }
        // The computer hears how it went, unless it gave the file up itself.
        if complete || b.failed != nil, let id = b.header["id"] {
            client?.send(["t": "file_result", "id": id, "ok": ok, "message": message])
        }
        t?.state = ok ? .done(message) : .failed(message)
        if let t { report(t) }
    }

    /// Send a file to the computer. On the main thread.
    func send(file url: URL) {
        let scoped = url.startAccessingSecurityScopedResource()
        let id = nextFile
        nextFile += 1
        let size = ((try? url.resourceValues(forKeys: [.fileSizeKey]))?.fileSize) ?? 0
        var t = Transfer(id: id, name: url.lastPathComponent, size: size, incoming: false)
        outgoing[id] = t
        transferChanged?(t)
        let client = self.client
        work.async { [weak self] in
            defer { if scoped { url.stopAccessingSecurityScopedResource() } }
            var why = "The connection ended before it was all sent."
            var sent = false
            if let handle = try? FileHandle(forReadingFrom: url) {
                var last = 0
                sent =
                    client?.sendBlob(kind: CoreClient.fileKind, header: ["id": id, "name": t.name, "size": size]) {
                        guard let piece = try? handle.read(upToCount: 256 << 10), !piece.isEmpty else { return nil }
                        t.moved += piece.count
                        if t.moved - last >= 1 << 20 {
                            last = t.moved
                            let now = t
                            DispatchQueue.main.async { self?.progress(now) }
                        }
                        return piece
                    } ?? false
                try? handle.close()
            } else {
                why = "The file couldn't be read."
            }
            DispatchQueue.main.async {
                guard let self else { return }
                if sent {
                    // Wait for the computer's word on it (file_result).
                    self.progress(t)
                } else {
                    t.state = .failed(why)
                    self.outgoing[id] = nil
                    self.transferChanged?(t)
                }
            }
        }
    }

    private func progress(_ t: Transfer) {
        guard outgoing[t.id] != nil else { return }
        outgoing[t.id] = t
        transferChanged?(t)
    }

    /// The computer's answer about a file this device sent (core thread).
    func fileResult(_ m: [String: Any]) {
        guard let id = (m["id"] as? NSNumber)?.intValue else { return }
        let ok = m["ok"] as? Bool ?? false
        let message = m["message"] as? String ?? ""
        DispatchQueue.main.async { [self] in
            guard var t = outgoing.removeValue(forKey: id) else { return }
            t.moved = t.size
            t.state = ok ? .done(message) : .failed(message.isEmpty ? "The computer didn't take it." : message)
            transferChanged?(t)
        }
    }

    private func report(_ t: Transfer) {
        DispatchQueue.main.async { [self] in transferChanged?(t) }
    }

    func stop() {
        timer?.invalidate()
        timer = nil
        clipboardOn = false
    }
}
