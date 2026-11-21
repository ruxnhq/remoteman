// The session: the picture, and its controls in one of three places. In a window they are in
// the title bar, or (Settings > General > Window controls) slide in over the picture when the
// pointer reaches the top edge; in full screen they are a capsule at the top centre.
import AppKit
import SwiftUI

struct SessionScreen: View {
    @EnvironmentObject private var model: AppModel
    @ObservedObject var session: ViewerSession
    let computer: SavedComputer
    @AppStorage(AppModel.WindowControls.key) private var controls = AppModel.WindowControls.titleBar.rawValue

    private var facts: StreamFacts { StreamFacts(session.statsText) }

    var body: some View {
        let bare = model.bare
        GeometryReader { geo in
            ZStack(alignment: .top) {
                Color.black
                if case .streaming = session.state {
                    ViewerRepresentable(session: session)
                        // Files dropped on the picture go to the computer.
                        .onDrop(of: [.fileURL], isTargeted: nil) { providers in
                            guard Features.fileTransfer, session.has("files") else { return false }
                            for p in providers {
                                _ = p.loadObject(ofClass: URL.self) { url, _ in
                                    if let url { DispatchQueue.main.async { session.send(file: url) } }
                                }
                            }
                            return true
                        }
                }
                if model.hasPicture {
                    if session.showStats {
                        FiguresOverlay(
                            facts: facts,
                            bitrate: session.stats.megabitsPerSecond.isNaN ? "–" : String(format: "%.0f Mb/s", session.stats.megabitsPerSecond))
                            .padding(.leading, 14).padding(.top, 42)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .allowsHitTesting(false)
                    }
                    if bare, model.barShown || model.pillStays {
                        HoverBar(session: session, computer: computer, facts: facts).padding(.top, 10)
                            .transition(.move(edge: .top).combined(with: .opacity))
                    }
                } else {
                    ConnectingView(session: session, computer: computer)
                }
                if model.hasPicture { SessionNotes(session: session).frame(maxHeight: .infinity, alignment: .bottom).padding(.bottom, 18) }
            }
            .animation(.easeOut(duration: 0.18), value: model.barShown)
            .onAppear { model.pictureSize = geo.size }
            .onChange(of: geo.size) { _, s in model.pictureSize = s }
        }
        .ignoresSafeArea(.container, edges: bare ? .all : [])
        .navigationTitle(computer.name)
        .navigationSubtitle(model.hasPicture ? caption : "")
        .toolbar {
            if model.hasPicture, !bare {
                SessionToolbar(model: model, session: session, computer: computer)
            }
        }
        // Without the toolbar the title bar goes too, and every click reaches the picture; the
        // bar that slides in draws window buttons of its own.
        .toolbar(bare ? .hidden : .visible, for: .windowToolbar)
        .onChange(of: controls) { _, _ in
            model.barShown = false
            model.applyChrome()
        }
    }

    /// "Attached · 2560 × 1496, 120 Hz": the picture's own size and rate once it is known.
    private var caption: String {
        let f = facts
        if let w = f.width, let h = f.height, let r = f.refresh { return "Attached · \(w) × \(h), \(r) Hz" + (session.streamHDR ? " · HDR" : "") }
        if let d = model.requested { return "Attached · \(d.width) × \(d.height), \(d.refresh) Hz" }
        return "Attached"
    }
}

/// The figures the engine publishes twice a second, read back out of its text.
struct StreamFacts {
    var codec = "–"
    var width: Int?
    var height: Int?
    var refresh: Int?
    var frames = "–"
    var age = "–"
    var lost = "–"
    var roundTrip = "–"

    init(_ text: String) {
        let lines = text.split(separator: "\n").map(String.init)
        guard let first = lines.first else { return }
        // "AV1 4:2:0 10-bit 2752×2064 @120"
        let parts = first.split(separator: " ").map(String.init)
        if parts.count >= 5, parts[3].contains("×") {
            codec = "\(parts[0]) \(parts[2]) \(parts[1])"
            let size = parts[3].split(separator: "×").compactMap { Int($0) }
            if size.count == 2 {
                width = size[0]
                height = size[1]
            }
            refresh = Int(parts[4].dropFirst())
        }
        func number(after word: String) -> String? {
            guard let r = text.range(of: word + " ") else { return nil }
            let rest = text[r.upperBound...]
            let token = rest.prefix { !$0.isWhitespace }
            return token.isEmpty || token == "–" ? nil : String(token)
        }
        if let v = number(after: "shown") { frames = "\(v) fps" }
        if let v = number(after: "age") { age = "\(v) ms" }
        if let v = number(after: "lost") { lost = v }
        if let v = number(after: "round trip") { roundTrip = "\(v) ms" }
    }
}

/// What the user should hear about without asking: a file on its way, or something that didn't work.
struct SessionNotes: View {
    @ObservedObject var session: ViewerSession

    var body: some View {
        VStack(spacing: 6) {
            if let t = session.transfers.last { note(line(t), failed: { if case .failed = t.state { return true } else { return false } }()) }
            if let n = session.notice { note(n, failed: true) }
        }
        .task(id: session.notice) {
            guard session.notice != nil else { return }
            try? await Task.sleep(nanoseconds: 6_000_000_000)
            if !Task.isCancelled { session.notice = nil }
        }
        .task(id: session.transfers) {
            guard let t = session.transfers.last, t.state != .moving else { return }
            try? await Task.sleep(nanoseconds: 5_000_000_000)
            if !Task.isCancelled { session.clearFinishedTransfers() }
        }
        .allowsHitTesting(false)
    }

    private func line(_ t: Transfer) -> String {
        switch t.state {
        case .moving:
            let part = t.size > 0 ? " · \(min(100, t.moved * 100 / t.size))%" : ""
            return (t.incoming ? "Receiving " : "Sending ") + t.name + part
        case .done(let place):
            return t.incoming ? "Saved \((place as NSString).lastPathComponent) in Downloads" : "\(t.name) is on the computer: \(place)"
        case .failed(let why):
            return "\(t.name): \(why)"
        }
    }

    private func note(_ text: String, failed: Bool) -> some View {
        Text(text).font(.system(size: 12, weight: .medium)).foregroundStyle(failed ? Tokens.orange : .white)
            .lineLimit(2).multilineTextAlignment(.center)
            .padding(.vertical, 7).padding(.horizontal, 12).frame(maxWidth: 520)
            .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(Color(red: 18 / 255, green: 18 / 255, blue: 22 / 255, opacity: 0.82)))
            .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).strokeBorder(.white.opacity(0.16), lineWidth: 1))
    }
}

/// "Session figures": six numbers over the top-left of the picture.
struct FiguresOverlay: View {
    let facts: StreamFacts
    var bitrate = "–"

    var body: some View {
        Grid(alignment: .leading, horizontalSpacing: 18, verticalSpacing: 6) {
            GridRow {
                cell("Frames", facts.frames)
                cell("Picture age", facts.age)
                cell("Bitrate", bitrate).opacity(Features.bitrateFigure ? 1 : 0.5)
            }
            GridRow {
                cell("Codec", facts.codec)
                cell("Lost frames", facts.lost)
                cell("Round trip", facts.roundTrip)
            }
        }
        .padding(.vertical, 8).padding(.horizontal, 12)
        .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(Color(red: 18 / 255, green: 18 / 255, blue: 22 / 255, opacity: 0.78)))
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(.white.opacity(0.16), lineWidth: 1))
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Session figures")
    }

    private func cell(_ label: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(label).font(.system(size: 10, weight: .semibold)).foregroundStyle(Color(red: 0.78, green: 0.78, blue: 0.82))
            Text(value).font(.system(size: 13, weight: .semibold)).foregroundStyle(.white).monospacedDigit()
        }
    }
}

// MARK: The controls

/// A place an AppKit menu can be opened from.
final class MenuAnchor {
    weak var view: NSView?
}

private struct AnchorView: NSViewRepresentable {
    let anchor: MenuAnchor
    func makeNSView(context: Context) -> NSView {
        let v = NSView()
        anchor.view = v
        return v
    }
    func updateNSView(_ v: NSView, context: Context) { anchor.view = v }
}

/// Where the controls are drawn, which decides how they look.
enum BarKind {
    case toolbar, slideIn, capsule
}

/// One control: an icon (and in the capsule sometimes a word), with its full name for
/// VoiceOver and the tooltip. `on` draws it pressed; a control that can't do anything yet is
/// passed `enabled: false`.
struct ControlButton: View {
    var symbol: String
    var name: String
    var word: String?
    var kind: BarKind
    var on = false
    var enabled = true
    var action: (NSView?) -> Void
    @State private var anchor = MenuAnchor()

    var body: some View {
        let button = Button {
            action(anchor.view)
        } label: {
            HStack(spacing: 5) {
                Image(systemName: symbol)
                if kind == .capsule, let word { Text(word) }
            }
            .background(AnchorView(anchor: anchor))
        }
        .help(enabled ? name : "\(name): not available yet")
        .accessibilityLabel(name)
        .disabled(!enabled)
        .focusable(false)
        if kind == .toolbar {
            button.background(on ? Tokens.sel : .clear, in: RoundedRectangle(cornerRadius: 6))
        } else {
            button.buttonStyle(BarButtonStyle(on: on, radius: kind == .capsule ? 17 : 14))
        }
    }
}

private struct BarButtonStyle: ButtonStyle {
    var on: Bool
    var radius: CGFloat
    @Environment(\.isEnabled) private var enabled
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 13))
            .padding(.horizontal, 9)
            .frame(minWidth: 34, minHeight: 28)
            .background(
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .fill(on || configuration.isPressed ? Tokens.sel : .clear))
            .opacity(enabled ? 1 : 0.35)
            .contentShape(Rectangle())
    }
}

/// The controls themselves, in the order the design gives them. Each is used by all three bars.
struct SessionControls {
    let model: AppModel
    let session: ViewerSession
    let computer: SavedComputer
    let kind: BarKind

    @ViewBuilder var main: some View {
        ControlButton(
            symbol: "keyboard", name: "Send keys and shortcuts", word: "Keys", kind: kind, enabled: Features.sendKeys
        ) { v in
            SessionMenus.keys(session: session).open(under: v)
        }
        ControlButton(symbol: "display", name: "Display", word: "Display", kind: kind) { v in
            SessionMenus.display(model: model, session: session, computer: computer).open(under: v)
        }
        ControlButton(symbol: "circle.lefthalf.filled", name: "Picture quality", word: "Picture", kind: kind) { v in
            SessionMenus.picture(model: model, session: session).open(under: v)
        }
        ControlButton(
            symbol: "cursorarrow", name: "Pointer", word: "Pointer", kind: kind, enabled: Features.pointerOptions
        ) { v in
            SessionMenus.pointer(session: session).open(under: v)
        }
    }

    @ViewBuilder var media: some View {
        ControlButton(
            symbol: session.soundOn ? "speaker.wave.2" : "speaker.slash", name: session.soundOn ? "Sound, on" : "Sound, off", kind: kind,
            on: session.soundOn, enabled: Features.sound && session.has("sound")
        ) { _ in session.setSound(!session.soundOn) }
        ControlButton(
            symbol: session.micOn ? "mic" : "mic.slash", name: session.micOn ? "Microphone, on" : "Microphone, off", kind: kind,
            on: session.micOn, enabled: Features.microphone && session.has("mic")
        ) { _ in session.setMic(!session.micOn) }
        ControlButton(
            symbol: session.cameraOn ? "video" : "video.slash", name: session.cameraOn ? "Camera, on" : "Camera, off", kind: kind,
            on: session.cameraOn, enabled: Features.camera && session.has("camera")
        ) { _ in session.setCamera(!session.cameraOn) }
    }

    @ViewBuilder var sharing: some View {
        ControlButton(
            symbol: "doc.on.clipboard", name: session.clipboardOn ? "Clipboard, shared" : "Clipboard, not shared", kind: kind,
            on: session.clipboardOn, enabled: Features.clipboard && session.has("clipboard")
        ) { _ in session.setClipboard(!session.clipboardOn) }
        ControlButton(symbol: "folder", name: "Send files", kind: kind, enabled: Features.fileTransfer && session.has("files")) { _ in
            let panel = NSOpenPanel()
            panel.allowsMultipleSelection = true
            panel.canChooseDirectories = false
            panel.prompt = "Send"
            panel.message = "Choose files to send to \(computer.name)"
            if panel.runModal() == .OK { panel.urls.forEach { session.send(file: $0) } }
        }
        ControlButton(
            symbol: "gauge", name: session.showStats ? "Show figures, on" : "Show figures", kind: kind, on: session.showStats
        ) { _ in session.showStats.toggle() }
    }

    var disconnect: some View {
        Button("Disconnect") { model.disconnect() }
            .foregroundStyle(Tokens.red)
            .fontWeight(.medium)
            .focusable(false)
    }
}

/// Windowed (a): the controls as three groups in the window's own toolbar.
struct SessionToolbar: ToolbarContent {
    let model: AppModel
    let session: ViewerSession
    let computer: SavedComputer

    var body: some ToolbarContent {
        let c = SessionControls(model: model, session: session, computer: computer, kind: .toolbar)
        ToolbarItemGroup(placement: .primaryAction) { c.main }
        ToolbarItemGroup(placement: .primaryAction) {
            c.media
            Divider()
            c.sharing
        }
        ToolbarItem(placement: .primaryAction) { c.disconnect }
    }
}

private struct GroupCapsule<Content: View>: View {
    @ViewBuilder var content: Content
    var body: some View {
        HStack(spacing: 2) { content }
            .padding(.horizontal, 3)
            .frame(height: 34)
            .background(Capsule().fill(.white.opacity(0.09)))
            .overlay(Capsule().strokeBorder(.white.opacity(0.085), lineWidth: 1))
    }
}

private struct BarDivider: View {
    var body: some View { Rectangle().fill(Tokens.sep).frame(width: 1, height: 20).padding(.horizontal, 5) }
}

/// Windowed (b): the same bar, as dark glass across the top of the picture, window buttons
/// included, while the pointer is at the top edge.
struct SlideInBar: View {
    @EnvironmentObject private var model: AppModel
    @ObservedObject var session: ViewerSession
    let computer: SavedComputer
    let caption: String

    var body: some View {
        let c = SessionControls(model: model, session: session, computer: computer, kind: .slideIn)
        HStack(spacing: 10) {
            WindowButtons(window: model.window).padding(.leading, 4).padding(.trailing, 8)
            VStack(alignment: .leading, spacing: 0) {
                Text(computer.name).font(.system(size: 13, weight: .bold))
                Text(caption).font(.system(size: 11)).foregroundStyle(.secondary)
            }
            Spacer(minLength: 10)
            GroupCapsule { c.main }
            GroupCapsule {
                c.media
                BarDivider()
                c.sharing
            }
            GroupCapsule { c.disconnect.buttonStyle(.plain).padding(.horizontal, 10) }
        }
        .padding(.horizontal, 14)
        .frame(height: 52)
        .frame(maxWidth: .infinity)
        .background(Color(red: 34 / 255, green: 34 / 255, blue: 40 / 255, opacity: 0.62))
        .background(.ultraThinMaterial)
        .overlay(alignment: .bottom) { Rectangle().fill(.white.opacity(0.12)).frame(height: 1) }
        .shadow(color: .black.opacity(0.35), radius: 12, y: 8)
        .environment(\.colorScheme, .dark)
        // The bar is the window's title bar while it shows: dragging it moves the window.
        .gesture(
            DragGesture(minimumDistance: 3).onChanged { _ in
                if let e = NSApp.currentEvent, e.type == .leftMouseDragged { model.window?.performDrag(with: e) }
            })
    }
}

/// Close, minimise and zoom, drawn as the design draws them, for a window whose title bar is hidden.
private struct WindowButtons: View {
    weak var window: NSWindow?
    @State private var hover = false

    var body: some View {
        HStack(spacing: 8) {
            light(0xFF5F57, "xmark", "Close") { window?.performClose(nil) }
            light(0xFEBC2E, "minus", "Minimise") { window?.miniaturize(nil) }
            light(0x28C840, "arrow.up.left.and.arrow.down.right", "Full screen") { window?.toggleFullScreen(nil) }
        }
        .onHover { hover = $0 }
    }

    private func light(_ hex: UInt32, _ symbol: String, _ name: String, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Circle().fill(Tokens.fixed(hex)).frame(width: 13, height: 13)
                .overlay(Image(systemName: symbol).font(.system(size: 7, weight: .bold)).foregroundStyle(.black.opacity(hover ? 0.55 : 0)))
        }
        .buttonStyle(.plain)
        .focusable(false)
        .help(name)
        .accessibilityLabel(name)
    }
}

/// Full screen: a glass capsule at the top centre while the pointer is at the top edge.
struct HoverBar: View {
    @EnvironmentObject private var model: AppModel
    @ObservedObject var session: ViewerSession
    let computer: SavedComputer
    let facts: StreamFacts

    var body: some View {
        let c = SessionControls(model: model, session: session, computer: computer, kind: .capsule)
        HStack(spacing: 4) {
            // In a window the pill carries the window's own buttons: there is no title bar.
            if !model.fullScreen {
                WindowButtons(window: model.window).padding(.leading, 8).padding(.trailing, 4)
                BarDivider()
            }
            // The computer's name over the picture's size and rate, as on the iPad and the Linux viewer.
            HStack(spacing: 7) {
                StatusDot(color: Tokens.green)
                VStack(alignment: .leading, spacing: 0) {
                    Text(computer.name).font(.system(size: 13, weight: .semibold)).lineLimit(1)
                    if let w = facts.width, let h = facts.height, let r = facts.refresh {
                        Text(verbatim: "\(w) × \(h) at \(r) Hz" + (session.streamHDR ? " · HDR" : "")).font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
                    }
                }
            }
            .padding(.horizontal, 8)
            .frame(height: 48)
            .contentShape(Rectangle())
            // The window has no title bar in a session: it is moved by the pill's name.
            .modifier(MovesWindow())
            .help("Drag to move the window")
            BarDivider()
            c.main
            BarDivider()
            c.media
            BarDivider()
            c.sharing
            BarDivider()
            c.disconnect.buttonStyle(.plain).padding(.horizontal, 10)
        }
        .padding(.horizontal, 8)
        .frame(height: 48)
        .background(.regularMaterial, in: Capsule())
        .overlay(Capsule().strokeBorder(.white.opacity(0.17), lineWidth: 1))
        .shadow(color: .black.opacity(0.16), radius: 11, y: 6)
        .environment(\.colorScheme, .dark)
    }
}

// MARK: The menus

/// A menu item that runs a closure.
final class ActionItem: NSMenuItem {
    private var handler: (() -> Void)?

    /// `hint` is set right-aligned in the secondary colour, as the design's menus have it.
    convenience init(
        _ title: String, hint: String = "", checked: Bool = false, enabled: Bool = true, width: CGFloat,
        handler: (() -> Void)? = nil
    ) {
        self.init(title: title, action: nil, keyEquivalent: "")
        self.handler = handler
        if enabled, handler != nil {
            target = self
            action = #selector(fire)
        }
        isEnabled = enabled
        state = checked ? .on : .off
        if !hint.isEmpty {
            let style = NSMutableParagraphStyle()
            style.tabStops = [NSTextTab(textAlignment: .right, location: width - 44)]
            let text = NSMutableAttributedString(
                string: "\(title)\t\(hint)",
                attributes: [
                    .font: NSFont.menuFont(ofSize: 0), .paragraphStyle: style,
                    .foregroundColor: enabled ? NSColor.labelColor : NSColor.tertiaryLabelColor,
                ])
            text.addAttribute(
                .foregroundColor, value: enabled ? NSColor.secondaryLabelColor : NSColor.tertiaryLabelColor,
                range: NSRange(location: (title as NSString).length + 1, length: (hint as NSString).length))
            attributedTitle = text
        }
    }

    @objc private func fire() { handler?() }
}

extension NSMenu {
    /// Open under a control (or at the pointer when the control's view isn't known).
    func open(under view: NSView?) {
        if let view, view.window != nil {
            popUp(positioning: nil, at: NSPoint(x: 0, y: view.isFlipped ? view.bounds.maxY + 8 : -8), in: view)
        } else {
            popUp(positioning: nil, at: NSEvent.mouseLocation, in: nil)
        }
    }

    fileprivate func header(_ title: String) { addItem(.sectionHeader(title: title)) }

    /// A line of small print inside the menu.
    fileprivate func caption(_ text: String, width: CGFloat) {
        let label = NSTextField(wrappingLabelWithString: text)
        label.font = .systemFont(ofSize: 11)
        label.textColor = .secondaryLabelColor
        label.preferredMaxLayoutWidth = width - 36
        let size = label.sizeThatFits(NSSize(width: width - 36, height: 200))
        label.frame = NSRect(x: 26, y: 3, width: width - 36, height: size.height)
        let box = NSView(frame: NSRect(x: 0, y: 0, width: width, height: size.height + 6))
        box.addSubview(label)
        let item = NSMenuItem()
        item.view = box
        item.isEnabled = false
        addItem(item)
    }
}

enum SessionMenus {
    /// The Display menu: where this display sits among the computer's own (changing it joins
    /// again, since the computer is told at the start), then what isn't there yet.
    static func display(model: AppModel, session: ViewerSession, computer: SavedComputer) -> NSMenu {
        let width: CGFloat = 360
        let menu = NSMenu()
        menu.autoenablesItems = false
        menu.minimumWidth = width
        let name = computer.name
        let facts = StreamFacts(session.statsText)
        if let w = facts.width, let h = facts.height, let r = facts.refresh {
            menu.header("This display, \(w) × \(h) at \(r) Hz, is \(name)'s")
        } else {
            menu.header("This display is \(name)'s")
        }
        let current = model.store[computer.id]?.place ?? computer.place
        for p in ViewerSession.Placement.own {
            menu.addItem(
                ActionItem(p.title, hint: PlacementCopy.hint(p), checked: p == current, width: width) {
                    if p != current { model.reconnect(placement: p) }
                })
        }
        menu.addItem(.separator())
        menu.addItem(
            ActionItem(
                model.secondShown ? "Remove the Second Display" : "Add a Second Display",
                hint: session.has("displays") ? "in a window of its own" : "\(name) can't do this yet", checked: model.secondShown,
                enabled: Features.secondDisplay && session.has("displays"), width: width
            ) { model.toggleSecondDisplay() })
        menu.addItem(.separator())
        menu.header("Or mirror \(name)'s monitor")
        menu.addItem(
            ActionItem(
                "Its Main Monitor", hint: session.has("mirror") ? "at its own size" : "\(name) can't do this yet", checked: current == .mirror,
                enabled: Features.mirrorMonitor && session.has("mirror"), width: width
            ) {
                if current != .mirror { model.reconnect(placement: .mirror) }
            })
        menu.caption("A mirrored monitor stays on at the computer and shows your desktop there.", width: width)
        menu.addItem(.separator())
        let full = ActionItem(model.fullScreen ? "Exit Full Screen" : "Enter Full Screen", width: width) { model.toggleFullScreen() }
        full.keyEquivalent = "f"
        full.keyEquivalentModifierMask = [.control, .command]
        menu.addItem(full)
        return menu
    }

    /// The Picture menu. The stream is what the computer and this Mac agree on, so the choices
    /// show what is in use and can't be changed yet; Show Figures works.
    /// Shortcuts this Mac keeps for itself, or has no key for, sent to the computer.
    static func keys(session: ViewerSession) -> NSMenu {
        let width: CGFloat = 300
        let menu = NSMenu()
        menu.autoenablesItems = false
        menu.minimumWidth = width
        let mac = session.client?.host.os == "macos"
        menu.header("Send to the computer")
        let list: [(String, String, [UInt16])] =
            mac
            ? [
                ("Switch apps", "⌘Tab", [0xE3, 0x2B]), ("Spotlight", "⌘Space", [0xE3, 0x2C]), ("Quit the app in front", "⌘Q", [0xE3, 0x14]),
                ("Force Quit…", "⌥⌘Esc", [0xE2, 0xE3, 0x29]), ("Lock the screen", "⌃⌘Q", [0xE0, 0xE3, 0x14]),
            ]
            : [
                ("Ctrl+Alt+Delete", "", [0xE0, 0xE2, 0x4C]), ("Super", "the overview or Start menu", [0xE3]),
                ("Switch windows", "Alt+Tab", [0xE2, 0x2B]), ("Close the window", "Alt+F4", [0xE2, 0x3D]),
                ("Lock the screen", "Super+L", [0xE3, 0x0F]), ("Print Screen", "", [0x46]),
            ]
        for (title, hint, keys) in list {
            menu.addItem(ActionItem(title, hint: hint, enabled: session.input != nil, width: width) { session.chord(keys) })
        }
        return menu
    }

    /// How the pointer is drawn and how its movement is sent.
    static func pointer(session: ViewerSession) -> NSMenu {
        let width: CGFloat = 340
        let menu = NSMenu()
        menu.autoenablesItems = false
        menu.minimumWidth = width
        let d = UserDefaults.standard
        menu.header("Pointer movement")
        menu.addItem(
            ActionItem("Normal", checked: !InputSettings.captured, width: width) { d.set(false, forKey: InputSettings.capturedKey) })
        menu.addItem(
            ActionItem("Captured", hint: "for 3D apps and games", checked: InputSettings.captured, width: width) {
                d.set(true, forKey: InputSettings.capturedKey)
            })
        menu.caption("Captured: click the picture to hold the pointer there; press ⌃⌥ to let it go.", width: width)
        menu.addItem(.separator())
        menu.addItem(
            ActionItem("Draw the Pointer on This Mac", hint: "from the next connection", checked: InputSettings.localPointer, width: width) {
                d.set(!InputSettings.localPointer, forKey: InputSettings.localPointerKey)
            })
        return menu
    }

    static func picture(model: AppModel, session: ViewerSession) -> NSMenu {
        let width: CGFloat = 340
        let menu = NSMenu()
        menu.autoenablesItems = false
        menu.minimumWidth = width
        let facts = StreamFacts(session.statsText)
        menu.header("Picture quality")
        func quality(_ v: String) {
            guard (ViewerPrefs.sharp ? "sharp" : "auto") != v else { return }
            UserDefaults.standard.set(v, forKey: ViewerPrefs.qualityKey)
            // The computer chooses the stream as a session starts: join again.
            model.reconnect(rejoin: true)
        }
        menu.addItem(
            ActionItem(
                "Automatic", hint: !ViewerPrefs.sharp && facts.codec != "–" ? "\(facts.codec) now" : "", checked: !ViewerPrefs.sharp,
                enabled: Features.pictureQuality, width: width
            ) { quality("auto") })
        menu.addItem(
            ActionItem(
                "Sharp Text (4:4:4)", hint: ViewerPrefs.sharp && facts.codec != "–" ? "\(facts.codec) now" : "HEVC, uses more data",
                checked: ViewerPrefs.sharp, enabled: Features.pictureQuality, width: width
            ) { quality("sharp") })
        // HDR is asked for by itself when this Mac's screen can show it; this turns that off or on
        // (the next connection follows it).
        let canHDR = ViewerSession.screenHeadroom > 1.05
        menu.addItem(
            ActionItem(
                "HDR", hint: session.streamHDR ? "on now" : canHDR ? "next connection" : "this screen can't show it",
                checked: ViewerPrefs.hdr && canHDR, enabled: canHDR, width: width
            ) {
                UserDefaults.standard.set(!ViewerPrefs.hdr, forKey: ViewerPrefs.hdrKey)
            })
        menu.addItem(.separator())
        menu.header("Refresh rate")
        let top = NSScreen.main?.maximumFramesPerSecond ?? 60
        let can60 = Features.refreshRateChoice && top > 60
        func full(_ on: Bool) {
            guard ViewerPrefs.fullRefresh != on else { return }
            UserDefaults.standard.set(on, forKey: ViewerPrefs.fullRefreshKey)
            model.reconnect()
        }
        menu.addItem(
            ActionItem("\(top) Hz", hint: "This display", checked: ViewerPrefs.fullRefresh || top <= 60, enabled: can60, width: width) { full(true) })
        if top > 60 {
            menu.addItem(ActionItem("60 Hz", checked: !ViewerPrefs.fullRefresh, enabled: can60, width: width) { full(false) })
        }
        menu.addItem(.separator())
        menu.header("The most the picture may use")
        let can = Features.bitrateLimit && session.has("configure")
        for bits in [0, 80_000_000, 40_000_000, 20_000_000, 10_000_000, 5_000_000] {
            let now = bits == 0 && session.hostBitrate > 0 ? String(format: "%.0f Mb/s now", Double(session.hostBitrate) / 1_000_000) : ""
            menu.addItem(
                ActionItem(
                    bits == 0 ? "Automatic" : "Up to \(bits / 1_000_000) Mb/s", hint: now, checked: session.bitrateLimit == bits,
                    enabled: can, width: width
                ) { session.setBitrateLimit(bits) })
        }
        menu.addItem(.separator())
        let figures = ActionItem("Show Figures", checked: session.showStats, width: width) { session.showStats.toggle() }
        figures.keyEquivalent = "i"
        figures.keyEquivalentModifierMask = [.option, .command]
        menu.addItem(figures)
        return menu
    }
}

// MARK: Connecting

/// "Connecting to …": the three steps, of which the engine tells two apart (it has answered
/// and its key is right; then the picture arrives).
struct ConnectingView: View {
    @EnvironmentObject private var model: AppModel
    @ObservedObject var session: ViewerSession
    let computer: SavedComputer

    private enum Step { case waiting, busy, done }

    var body: some View {
        let answered: Bool = { if case .streaming = session.state { return true } else { return false } }()
        VStack(spacing: 18) {
            Text("Connecting to \(computer.name)").font(.system(size: 22, weight: .bold))
            Text("remoteman attaches to the session that is logged in at \(computer.name).")
                .font(.system(size: 14)).lineSpacing(4).foregroundStyle(.secondary)
                .multilineTextAlignment(.center).frame(maxWidth: 620)
            HStack(alignment: .top, spacing: 12) {
                let stage = answered ? 4 : session.stage
                step("Found", stage >= 1 ? "At \(computer.address)" : "Looking at \(computer.address)", stage >= 1 ? .done : .busy)
                step(
                    "Recognised",
                    answered ? "Its key matches the pairing" : (computer.pairOnConnect ? "Pairing with its key" : "By its key"),
                    stage >= 2 ? .done : stage == 1 ? .busy : .waiting)
                step("Attach", answered ? "Waiting for the picture" : "To its session", stage >= 2 ? .busy : .waiting)
            }
            .frame(maxWidth: 720)
            if session.loggingIn {
                Label("\(computer.name) was locked or logged out. It is logging in for you; this takes a few seconds.", systemImage: "lock.open")
                    .font(.system(size: 13)).foregroundStyle(.secondary)
            }
            if let code = session.pairingCode {
                VStack(spacing: 6) {
                    Text(code).font(.system(size: 40, weight: .semibold, design: .monospaced)).tracking(2)
                        .accessibilityLabel("Pairing code \(code)")
                    Text("\(computer.name) is waiting for its owner. Check that it shows the same code, then confirm the pairing there.")
                        .font(.system(size: 13)).foregroundStyle(.secondary).multilineTextAlignment(.center)
                }
                .padding(.vertical, 6)
            }
            Label(joinsAs, systemImage: "lock")
                .font(.system(size: 11)).foregroundStyle(.secondary)
                .multilineTextAlignment(.center).frame(maxWidth: 620)
            Button("Cancel") { model.disconnect() }
                .keyboardShortcut(.cancelAction)
                .padding(.top, 4)
        }
        .padding(32)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color(red: 0x17 / 255, green: 0x17 / 255, blue: 0x1A / 255))
        .environment(\.colorScheme, .dark)
    }

    private var joinsAs: String {
        let name = computer.name
        switch model.store[computer.id]?.place ?? computer.place {
        case .extend: return "This Mac joins as an extra display, beside \(name)'s own screens."
        case .main: return "This Mac joins as the main display, and \(name)'s own screens stay on."
        case .only: return "This Mac joins as the only display, so \(name)'s own screens stay off while you're connected."
        case .mirror: return "This Mac shows \(name)'s main monitor as it is."
        }
    }

    private func step(_ title: String, _ caption: String, _ state: Step) -> some View {
        HStack(alignment: .top, spacing: 10) {
            ZStack {
                switch state {
                case .done:
                    Circle().fill(Tokens.green)
                    Image(systemName: "checkmark").font(.system(size: 11, weight: .heavy))
                        .foregroundStyle(Color(red: 0x10 / 255, green: 0x24 / 255, blue: 0x0F / 255))
                case .busy:
                    ProgressView().controlSize(.small)
                case .waiting:
                    Circle().strokeBorder(.white.opacity(0.25), lineWidth: 2)
                }
            }
            .frame(width: 24, height: 24)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.system(size: 13, weight: .semibold)).opacity(state == .waiting ? 0.6 : 1)
                Text(caption).font(.system(size: 11)).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(.white.opacity(0.045)))
        .overlay(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .strokeBorder(state == .busy ? Tokens.tintfg : .white.opacity(0.085), lineWidth: 1))
        .accessibilityElement(children: .combine)
    }
}


/// Dragging here moves the window (macOS 15 and later; before that the window is moved from its edges' title strip).
private struct MovesWindow: ViewModifier {
    func body(content: Content) -> some View {
        if #available(macOS 15.0, *) { content.gesture(WindowDragGesture()) } else { content }
    }
}
