// A way to look at the interface without anyone looking at the screen, in the manner of the
// other unattended runs:
//   remoteman --ui-shots DIR [--ui-recording file.rmrec]
// draws the designed screens with made-up computers (the saved list is not read or written),
// saves each as a PNG in DIR, and quits. With a recording it also puts the session's chrome
// around it. The names and dates here are the design's invented examples, used only for this.
import AppKit
import SwiftUI

enum UIShots {
    private static func value(after flag: String) -> String? {
        let args = CommandLine.arguments
        guard let i = args.firstIndex(of: flag), i + 1 < args.count else { return nil }
        return args[i + 1]
    }

    static let directory = value(after: "--ui-shots")

    static var sampleStore: ComputerStore {
        func day(_ y: Int, _ m: Int, _ d: Int) -> Date { Calendar.current.date(from: DateComponents(year: y, month: m, day: d, hour: 9, minute: 12))! }
        var a = SavedComputer(name: "Studio PC", address: "studio-pc")
        a.os = "linux"
        a.place = .only
        a.paired = day(2026, 10, 12)
        a.lastUsed = Date().addingTimeInterval(-3 * 3600)
        a.lastDuration = 100 * 60
        a.fingerprint = "7c4a19e2b05d63f8a1b2c3d4e5f60718"
        var b = SavedComputer(name: "Office Mac mini", address: "office-mini.local:7800")
        b.os = "macos"
        b.paired = day(2026, 9, 30)
        var c = SavedComputer(name: "Lab NUC", address: "10.0.0.12")
        c.pairOnConnect = true
        return ComputerStore(preview: [a, b, c])
    }

    static func run(model: AppModel) {
        guard let directory else { return }
        let dir = URL(fileURLWithPath: directory, isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        func settle(_ seconds: Double) { RunLoop.current.run(until: Date().addingTimeInterval(seconds)) }
        func write(_ view: NSView, _ name: String) {
            guard let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { return print("ui-shots: no bitmap for \(name)") }
            view.cacheDisplay(in: view.bounds, to: rep)
            let ok = (try? rep.representation(using: .png, properties: [:])?.write(to: dir.appendingPathComponent(name + ".png"))) != nil
            print("ui-shots: \(name) \(Int(view.bounds.width))x\(Int(view.bounds.height)) \(ok ? "saved" : "NOT saved")")
        }
        /// The app's own window, title bar and toolbar included.
        func window(_ name: String) {
            guard let frame = model.window?.contentView?.superview else { return print("ui-shots: no window for \(name)") }
            write(frame, name)
        }
        /// A view by itself, in a window that is never shown.
        func view<V: View>(_ name: String, _ size: CGSize, dark: Bool = false, _ content: V) {
            let host = NSHostingView(
                rootView: content.environmentObject(model).tint(Tokens.tint)
                    .frame(width: size.width, height: size.height).background(Tokens.win))
            let w = NSWindow(contentRect: NSRect(origin: .zero, size: size), styleMask: [.borderless], backing: .buffered, defer: false)
            w.appearance = NSAppearance(named: dark ? .darkAqua : .aqua)
            w.contentView = host
            host.layoutSubtreeIfNeeded()
            settle(0.4)
            write(host, name)
        }

        let computers = model.store.computers
        settle(1.0)
        window("main-light")
        model.window?.appearance = NSAppearance(named: .darkAqua)
        model.selection = computers[1].id
        settle(0.6)
        window("main-dark")
        model.window?.appearance = NSAppearance(named: .aqua)
        model.selection = computers[2].id
        model.errors[computers[2].id] = "connection refused (os error 61)"
        settle(0.6)
        window("main-error")
        model.errors = [:]
        model.selection = computers[0].id

        view("add-sheet", CGSize(width: 460, height: 560), AddComputerSheet { _ in })
        view("paired-sheet", CGSize(width: 440, height: 400), PairedSheet(computer: computers[0]))
        view("connecting", CGSize(width: 1120, height: 668), dark: true, ConnectingView(session: model.session, computer: computers[0]))
        view("settings-general", CGSize(width: 680, height: 680), GeneralSettings())
        view("settings-keyboard", CGSize(width: 680, height: 700), dark: true, KeyboardPointerSettings())
        view("settings-computers", CGSize(width: 680, height: 620), ComputerSettings(store: model.store))
        let text = "AV1 4:2:0 10-bit 2560×1496 @120\nshown 120 fps · received 120 fps · replaced unseen 0\ndecode 2.10 ms (max 3.40) · age 11.0 ms\nlost 0 · loss reports 0 · round trip 1.80 ms"
        view("figures", CGSize(width: 420, height: 120), dark: true, FiguresOverlay(facts: StreamFacts(text)).padding(10))
        view(
            "hover-bar", CGSize(width: 1100, height: 80), dark: true,
            ZStack {
                Color.gray
                HoverBar(session: model.session, computer: computers[0], facts: StreamFacts(text))
            })
        view(
            "slide-in-bar", CGSize(width: 1280, height: 80), dark: true,
            ZStack(alignment: .top) {
                Color.gray
                SlideInBar(session: model.session, computer: computers[0], caption: "Attached · 2560 × 1600, 120 Hz")
            })

        // The session's chrome around a recording.
        if let recording = value(after: "--ui-recording") {
            let before = UserDefaults.standard.string(forKey: AppModel.WindowControls.key)
            defer { UserDefaults.standard.set(before, forKey: AppModel.WindowControls.key) }
            UserDefaults.standard.set(AppModel.WindowControls.titleBar.rawValue, forKey: AppModel.WindowControls.key)
            model.session.showStats = true
            model.preview(computers[0], recording: URL(fileURLWithPath: recording))
            settle(2.5)
            print("ui-shots: session has picture \(model.hasPicture), picture area \(Int(model.pictureSize.width))x\(Int(model.pictureSize.height))")
            window("session-title-bar")
            UserDefaults.standard.set(AppModel.WindowControls.slideIn.rawValue, forKey: AppModel.WindowControls.key)
            settle(0.8)
            window("session-no-bar")
            print("ui-shots: without a bar the picture area is \(Int(model.pictureSize.width))x\(Int(model.pictureSize.height))")
            // Who gets a click in the top strip: it has to be the picture, not the title bar.
            if let w = model.window, let frame = w.contentView?.superview {
                let p = NSPoint(x: frame.bounds.midX, y: frame.bounds.maxY - 10)
                let hit = frame.hitTest(p)
                print("ui-shots: a click 10 pt under the top edge goes to \(hit.map { String(describing: type(of: $0)) } ?? "nothing"), movable \(w.isMovable)")
            }
            model.barShown = true
            settle(0.8)
            window("session-bar-slid-in")
            model.disconnect()
            settle(0.6)
        }
        print("ui-shots: done")
        exit(0)
    }
}

extension AppModel {
    /// A session with a recording in place of a computer, for looking at the session's chrome.
    func preview(_ c: SavedComputer, recording: URL) {
        beginPreview(c)
        session.play(recording: recording)
    }
}
