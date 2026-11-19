// A live session: the picture, and floating over it the controls of the design. On iPad a bar
// at the top (tucked away while a hardware keyboard is attached), the performance figures, and
// a strip of keys above the on-screen keyboard; on iPhone one capsule that opens a sheet.
import GameController
import SwiftUI

struct SessionScreen: View {
    @EnvironmentObject private var model: AppModel
    @EnvironmentObject private var session: ViewerSession
    @State private var keyboardShown = false
    /// The controls are folded into the handle until asked for.
    @State private var expanded = false
    @State private var displayMenu = false
    @State private var pictureMenu = false
    @State private var pickingFiles = false
    @State private var leaving = false
    @AppStorage(InputSettings.touchModeKey) private var touchMode = "direct"
    @State private var confirmingCamera = false

    /// The camera goes on at once, or after a question for those who asked for one.
    private func askCamera() {
        if !session.cameraOn, UserDefaults.standard.bool(forKey: ViewerPrefs.askCameraKey) {
            confirmingCamera = true
        } else {
            Haptics.toggle(!session.cameraOn)
            session.setCamera(!session.cameraOn)
        }
    }
    @State private var hint = false
    @StateObject private var keys = KeyStrip()
    @AppStorage(SessionHandle.shownKey) private var showHandle = true
    @AppStorage(SessionHandle.edgeKey) private var handleEdge = ""

    // By the kind of device, not the width: a large iPhone on its side is still an iPhone.
    private var pad: Bool { UIDevice.current.userInterfaceIdiom == .pad }
    private var name: String { model.current?.name ?? session.client?.host.name ?? "Computer" }

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            ViewerRepresentable(session: session, keyboardShown: keyboardShown).ignoresSafeArea()
            // Unattended runs show the picture alone.
            if model.current != nil { chrome }
        }
        .environment(\.colorScheme, .dark)
        .onAppear {
            keys.session = session
            // A three-finger tap, or the keyboard, asks for the controls from the picture itself.
            session.toggleControls = { withAnimation(.snappy) { expanded.toggle() } }
            session.leaveRequested = { leave() }
            if model.current != nil, !UserDefaults.standard.bool(forKey: SessionHandle.hintKey) {
                UserDefaults.standard.set(true, forKey: SessionHandle.hintKey)
                hint = true
                DispatchQueue.main.asyncAfter(deadline: .now() + 6) { withAnimation { hint = false } }
            }
        }
        .onDisappear {
            keys.releaseAll()
            session.toggleControls = nil
            session.leaveRequested = nil
        }
        .sheet(isPresented: Binding(get: { expanded && !pad }, set: { if !$0 { expanded = false } })) { phoneSheet }
        .overlay(alignment: .bottom) { if model.current != nil { SessionNotes().padding(.bottom, keyboardShown ? 76 : 30) } }
        // Above the sheet too, so that the iPhone's Files tile can open it.
        .background(
            Color.clear.fileImporter(isPresented: $pickingFiles, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
                if case .success(let urls) = result { urls.forEach { session.send(file: $0) } }
            })
        .alert("Share this camera with \(session.client?.host.name ?? "the computer")?", isPresented: $confirmingCamera) {
            Button("Share") {
                Haptics.toggle(true)
                session.setCamera(true)
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Programs on the computer can then see through this camera until you turn it off.")
        }
        .alert("A file is still on its way", isPresented: $leaving) {
            Button("Disconnect", role: .destructive) { model.disconnect() }
            Button("Stay", role: .cancel) {}
        } message: {
            Text("Disconnecting now stops it.")
        }
    }

    /// Back to the list of computers. Asks only when something would be lost.
    private func leave() {
        if session.transfers.contains(where: { $0.state == .moving }) {
            expanded = false
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.45) { leaving = true }
        } else {
            expanded = false
            model.disconnect()
        }
    }

    private func pickFiles() {
        if expanded, !pad {
            expanded = false
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { pickingFiles = true }
        } else {
            pickingFiles = true
        }
    }

    @ViewBuilder
    private var chrome: some View {
        let low = handleEdge == SessionHandle.Edge.bottom.rawValue
        ZStack {
            // A touch outside the open bar folds it away (and goes no further).
            if expanded, pad {
                Color.black.opacity(0.001).ignoresSafeArea().onTapGesture { withAnimation(.snappy) { expanded = false } }
            }
            VStack(spacing: 0) {
                if pad, expanded, !low { padBar.padding(.top, 14).transition(.move(edge: .top).combined(with: .opacity)) }
                Spacer()
                if session.showStats, !pad { HUD().padding(.bottom, keyboardShown ? 8 : 104) }
                if pad, keyboardShown { keyStrip.padding(.bottom, 8) }
                if pad, expanded, low { padBar.padding(.bottom, 14).transition(.move(edge: .bottom).combined(with: .opacity)) }
            }
            .overlay(alignment: .topTrailing) {
                if session.showStats, pad { HUD().padding(.top, expanded && !low ? 88 : 20).padding(.trailing, 20) }
            }
            if showHandle { SessionHandle(expanded: $expanded, pad: pad, hint: hint) }
        }
    }

    // MARK: iPad bar

    private var padBar: some View {
        HStack(spacing: 6) {
            Button("Disconnect") { leave() }
                .font(.system(size: 15, weight: .medium)).foregroundStyle(Theme.sessionRed).padding(.horizontal, 12).frame(height: 44)
            separator
            VStack(alignment: .leading, spacing: 0) {
                Text(name).font(.system(size: 15, weight: .semibold)).lineLimit(1)
                Text(subtitle).font(.system(size: 12, weight: .medium)).foregroundStyle(.white.opacity(0.78)).lineLimit(1)
            }
            .padding(.horizontal, 10)
            separator
            icon("keyboard", "Keyboard", active: keyboardShown) { keyboardShown.toggle() }
            text("Display", "display", active: displayMenu) { displayMenu = true }
                .popover(isPresented: $displayMenu) { DisplayMenu().presentationCompactAdaptation(.popover) }
            text("Picture", "sparkles.tv", active: pictureMenu) { pictureMenu = true }
                .popover(isPresented: $pictureMenu) { PictureMenu().presentationCompactAdaptation(.popover) }
                .available(Features.bitrateLimit && session.has("configure"))
            text(touchMode == "trackpad" ? "Trackpad" : "Touch", touchMode == "trackpad" ? "rectangle.and.hand.point.up.left" : "hand.point.up.left", active: touchMode == "trackpad") {
                touchMode = touchMode == "trackpad" ? "direct" : "trackpad"
                Haptics.toggle(touchMode == "trackpad")
            }
            .available(Features.trackpadMode)
            separator
            icon(session.soundOn ? "speaker.wave.2.fill" : "speaker.wave.2", session.soundOn ? "Sound, on" : "Sound, off", active: session.soundOn) {
                Haptics.toggle(!session.soundOn)
                session.setSound(!session.soundOn)
            }
            .available(Features.sound && session.has("sound"))
            icon(session.micOn ? "mic.fill" : "mic", session.micOn ? "Microphone, on" : "Microphone, off", active: session.micOn) {
                session.setMic(!session.micOn)
            }
            .available(Features.microphone && session.has("mic"))
            icon(session.cameraOn ? "camera.fill" : "camera", session.cameraOn ? "Camera, on" : "Camera, off", active: session.cameraOn) {
                askCamera()
            }
            .available(Features.camera && session.has("camera"))
            separator
            icon(
                session.clipboardOn ? "doc.on.clipboard.fill" : "doc.on.clipboard",
                session.clipboardOn ? "Clipboard, shared" : "Clipboard, not shared", active: session.clipboardOn
            ) { session.setClipboard(!session.clipboardOn) }
            .available(Features.clipboard && session.has("clipboard"))
            icon("arrow.up.arrow.down", "Send files to the computer", active: false) { pickFiles() }
                .available(Features.files && session.has("files"))
            icon("gauge.with.dots.needle.33percent", "Show performance figures", active: session.showStats) { session.showStats.toggle() }
            icon("chevron.up", "Hide session controls", active: false) { withAnimation(.snappy) { expanded = false } }
        }
        .foregroundStyle(.white)
        .padding(.horizontal, 8).frame(height: 60)
        .sessionGlass()
    }

    private var subtitle: String {
        let hz = session.stats.label.split(separator: "@").last.map { " · \($0) Hz" } ?? ""
        return "Attached" + hz
    }

    private var separator: some View { Rectangle().fill(.white.opacity(0.22)).frame(width: 1, height: 24).padding(.horizontal, 6) }

    private func icon(_ symbol: String, _ label: String, active: Bool, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol).font(.system(size: 18)).frame(width: 44, height: 44)
                .background(active ? .white.opacity(0.20) : .clear, in: RoundedRectangle(cornerRadius: 22, style: .continuous))
        }
        .accessibilityLabel(label)
    }

    private func text(_ title: String, _ symbol: String, active: Bool, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 7) {
                Image(systemName: symbol).font(.system(size: 17))
                // The words go when the iPad is upright and the bar would not fit with them.
                if UIScreen.main.bounds.width >= 1150 { Text(title).font(.system(size: 15, weight: .medium)).lineLimit(1).fixedSize() }
            }
            .padding(.horizontal, 12).frame(height: 44)
            .background(active ? .white.opacity(0.20) : .clear, in: RoundedRectangle(cornerRadius: 22, style: .continuous))
        }
        .accessibilityLabel(title)
    }

    // MARK: Key strip

    private var keyStrip: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 6) {
                ForEach(KeyStrip.groups.indices, id: \.self) { g in
                    ForEach(KeyStrip.groups[g]) { k in keyButton(k) }
                    if g < KeyStrip.groups.count - 1 { Spacer().frame(width: 6) }
                }
            }
            .padding(.horizontal, 8)
        }
        .frame(height: 60).frame(maxWidth: 900)
        .sessionGlass()
        .padding(.horizontal, 12)
        .accessibilityLabel("Keys the iPad keyboard lacks or keeps for itself")
    }

    private func keyButton(_ k: KeyStrip.Key) -> some View {
        let held = keys.held.contains(k.usage)
        return Button { keys.press(k) } label: {
            Text(k.label).font(.system(size: 15, weight: .medium))
                .foregroundStyle(held ? .black : .white)
                .frame(minWidth: 50, minHeight: 44).padding(.horizontal, 4)
                .background(held ? Color.white : .white.opacity(0.16), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        }
        .buttonStyle(.plain)
    }

    // MARK: iPhone controls sheet

    private var phoneSheet: some View {
        ScrollView {
        VStack(alignment: .leading, spacing: 16) {
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(name).font(Theme.headline).lineLimit(1)
                    Text(subtitle).font(Theme.footnote).foregroundStyle(Theme.secondary)
                }
                Spacer(minLength: 4)
                // First among the controls: back to the list of computers in two taps.
                Button("Disconnect") { leave() }
                    .font(.system(size: 15, weight: .semibold)).foregroundStyle(Theme.sessionRed)
                    .padding(.horizontal, 14).frame(height: 44)
                    .background(Theme.fill, in: Capsule())
                CircleButton(icon: "xmark", label: "Close controls") { expanded = false }
            }
            LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 10), count: 3), spacing: 10) {
                tile("keyboard", "Keyboard", on: keyboardShown) {
                    keyboardShown.toggle()
                    expanded = false
                }
                tile("speaker.wave.2", "Sound", on: session.soundOn) { Haptics.toggle(!session.soundOn); session.setSound(!session.soundOn) }
                    .available(Features.sound && session.has("sound"))
                tile("mic", "Microphone", on: session.micOn) { Haptics.toggle(!session.micOn); session.setMic(!session.micOn) }
                    .available(Features.microphone && session.has("mic"))
                tile("camera", "Camera", on: session.cameraOn) { askCamera() }
                    .available(Features.camera && session.has("camera"))
                tile("doc.on.clipboard", "Clipboard", on: session.clipboardOn) { Haptics.toggle(!session.clipboardOn); session.setClipboard(!session.clipboardOn) }
                    .available(Features.clipboard && session.has("clipboard"))
                tile("arrow.up.arrow.down", "Files", on: false) { pickFiles() }.available(Features.files && session.has("files"))
            }
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 6) { ForEach(KeyStrip.groups.flatMap { $0 }) { k in keyButton(k) } }
            }
            VStack(spacing: 0) {
                Menu {
                    Picker("This iPhone on \(name)", selection: placement) {
                        ForEach(ViewerSession.Placement.own) { Text($0.label).tag($0) }
                        if session.has("mirror") { Text(ViewerSession.Placement.mirror.label).tag(ViewerSession.Placement.mirror) }
                    }
                } label: {
                    Row(title: "Display") { RowValue(text: placement.wrappedValue.label, chevron: true) }
                }
                Menu {
                    Picker("The most the picture may use", selection: Binding(get: { session.bitrateLimit }, set: { session.setBitrateLimit($0) })) {
                        ForEach(PictureMenu.limits, id: \.self) { Text(PictureMenu.label($0)).tag($0) }
                    }
                } label: {
                    Row(title: "Picture") { RowValue(text: PictureMenu.label(session.bitrateLimit), chevron: true) }
                }
                .available(Features.bitrateLimit && session.has("configure"))
                ChoiceRow(title: "Touch", value: $touchMode, choices: [("direct", "Direct"), ("trackpad", "Trackpad")])
                    .available(Features.trackpadMode)
                Row(title: "Show figures", last: true) { Toggle("", isOn: $session.showStats).labelsHidden() }
            }
            .background(Theme.fill, in: RoundedRectangle(cornerRadius: Theme.cardRadius, style: .continuous))
        }
        .padding(20)
        }
        .presentationDetents([.medium, .large])
        .presentationCornerRadius(38)
        .presentationDragIndicator(.visible)
        // A dark, solid sheet whichever way the phone is held (on its side the sheet fills the
        // screen, and the system's own light background made the white labels unreadable).
        .presentationBackground(Color(hex: 0x1C1C1E))
        .preferredColorScheme(.dark)
        .environment(\.colorScheme, .dark)
    }

    private func tile(_ symbol: String, _ label: String, on: Bool, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            VStack(spacing: 6) {
                Image(systemName: symbol).font(.system(size: 20))
                Text(label).font(.system(size: 13, weight: .medium))
            }
            .foregroundStyle(on ? .black : .white)
            .frame(maxWidth: .infinity).frame(height: 70)
            .background(on ? Color.white : Theme.fill, in: RoundedRectangle(cornerRadius: 22, style: .continuous))
        }
        .buttonStyle(.plain)
    }

    private var placement: Binding<ViewerSession.Placement> {
        Binding(
            get: { model.current?.placementChoice ?? .extend },
            set: { new in if model.setPlacement(new) { expanded = false } })
    }
}

/// The session's handle: a small pill at an edge of the picture, always there during a
/// session (unless switched off in Settings). A tap opens the controls; a drag moves it along
/// the edges, and it stays where it was put on this device. It fades when left alone, and its
/// touches never reach the computer.
struct SessionHandle: View {
    enum Edge: String { case top, bottom, leading, trailing }
    static let shownKey = "showSessionHandle", edgeKey = "sessionHandleEdge", alongKey = "sessionHandleAlong", hintKey = "sessionHandleHintShown"

    @Binding var expanded: Bool
    let pad: Bool
    let hint: Bool
    @AppStorage(SessionHandle.edgeKey) private var edgeRaw = ""
    @AppStorage(SessionHandle.alongKey) private var along = -1.0
    @State private var finger: CGPoint?
    @State private var moved = false
    @State private var touched = Date()
    @State private var faded = false

    // The top edge's middle on iPad; the trailing edge on iPhone, a third of the way down.
    private var edge: Edge { Edge(rawValue: edgeRaw) ?? (pad ? .top : .trailing) }
    private var fraction: Double { along >= 0 ? along : (pad ? 0.5 : 0.33) }

    /// The screen's own margins (the notch, the home indicator): the session's picture ignores
    /// them, the handle keeps clear of them.
    private var insets: UIEdgeInsets {
        let scene = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first
        return scene?.windows.first(where: { $0.isKeyWindow })?.safeAreaInsets ?? scene?.windows.first?.safeAreaInsets ?? .zero
    }

    /// The rectangle the handle's centre runs along.
    private func track(_ size: CGSize) -> CGRect {
        let i = insets
        let m: CGFloat = 16
        return CGRect(
            x: i.left + m, y: i.top + m, width: max(1, size.width - i.left - i.right - 2 * m), height: max(1, size.height - i.top - i.bottom - 2 * m))
    }

    private func point(_ size: CGSize) -> CGPoint {
        let r = track(size)
        switch edge {
        case .top: return CGPoint(x: r.minX + r.width * fraction, y: r.minY)
        case .bottom: return CGPoint(x: r.minX + r.width * fraction, y: r.maxY)
        case .leading: return CGPoint(x: r.minX, y: r.minY + r.height * fraction)
        case .trailing: return CGPoint(x: r.maxX, y: r.minY + r.height * fraction)
        }
    }

    /// Let go at `p`: the nearest edge takes the handle, at the place along it nearest `p`.
    private func drop(_ p: CGPoint, _ size: CGSize) {
        let r = track(size)
        let d: [(Edge, CGFloat)] = [(.top, abs(p.y - r.minY)), (.bottom, abs(p.y - r.maxY)), (.leading, abs(p.x - r.minX)), (.trailing, abs(p.x - r.maxX))]
        let nearest = d.min { $0.1 < $1.1 }!.0
        let f = nearest == .top || nearest == .bottom ? (p.x - r.minX) / r.width : (p.y - r.minY) / r.height
        edgeRaw = nearest.rawValue
        along = Double(min(max(f, 0.06), 0.94))
    }

    var body: some View {
        GeometryReader { geo in
            let at = finger ?? point(geo.size)
            let upright = finger == nil && (edge == .leading || edge == .trailing)
            ZStack {
                Capsule()
                    .fill(.white.opacity(0.92))
                    .overlay(Capsule().strokeBorder(.black.opacity(0.35), lineWidth: 1))
                    .shadow(color: .black.opacity(0.5), radius: 3)
                    .frame(width: upright ? 7 : 40, height: upright ? 40 : 7)
            }
            // What the finger has to hit is larger than what is drawn.
            .frame(width: upright ? 44 : 60, height: upright ? 60 : 44)
            .contentShape(Rectangle())
            .opacity(faded && finger == nil && !expanded ? 0.3 : 1)
            .position(at)
            .gesture(
                DragGesture(minimumDistance: 0, coordinateSpace: .named("handle"))
                    .onChanged { g in
                        touched = Date()
                        faded = false
                        if hypot(g.translation.width, g.translation.height) > 10 { moved = true }
                        if moved { finger = g.location }
                    }
                    .onEnded { g in
                        if moved {
                            withAnimation(.snappy) {
                                drop(g.location, geo.size)
                                finger = nil
                            }
                            Haptics.choice()
                        } else {
                            Haptics.key()
                            withAnimation(.snappy) { expanded.toggle() }
                        }
                        moved = false
                        touched = Date()
                    })
            .accessibilityLabel(expanded ? "Hide session controls" : "Show session controls")
            .accessibilityAddTraits(.isButton)
            .overlay {
                if hint {
                    Text("Tap for controls · a three-finger tap also works")
                        .font(.system(size: 13, weight: .medium)).foregroundStyle(.white)
                        .padding(.horizontal, 12).padding(.vertical, 8)
                        .sessionGlass(radius: 16)
                        .fixedSize()
                        .position(hintPoint(at, geo.size))
                        .allowsHitTesting(false)
                        .transition(.opacity)
                }
            }
        }
        .coordinateSpace(name: "handle")
        .ignoresSafeArea()
        .task(id: touched) {
            try? await Task.sleep(nanoseconds: 3_500_000_000)
            if !Task.isCancelled { withAnimation(.easeOut(duration: 0.6)) { faded = true } }
        }
    }

    /// Beside the handle, towards the middle of the screen, and inside it.
    private func hintPoint(_ at: CGPoint, _ size: CGSize) -> CGPoint {
        var p = at
        switch edge {
        case .top: p.y += 44
        case .bottom: p.y -= 44
        case .leading: p.x += 190
        case .trailing: p.x -= 190
        }
        p.x = min(max(p.x, 180), size.width - 180)
        return p
    }
}

/// "Picture" on the iPad's bar: the most the picture may use.
struct PictureMenu: View {
    @EnvironmentObject private var session: ViewerSession
    @Environment(\.dismiss) private var dismiss

    static let limits = [0, 80_000_000, 40_000_000, 20_000_000, 10_000_000, 5_000_000]
    static func label(_ bits: Int) -> String { bits == 0 ? "Automatic" : "Up to \(bits / 1_000_000) Mb/s" }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text("The most the picture may use").font(.system(size: 13, weight: .semibold)).foregroundStyle(Theme.secondary)
                .padding(.horizontal, 12).padding(.vertical, 6)
            ForEach(Self.limits, id: \.self) { bits in
                Button {
                    Haptics.choice()
                    session.setBitrateLimit(bits)
                    dismiss()
                } label: {
                    HStack(spacing: 10) {
                        Image(systemName: "checkmark").font(.system(size: 15, weight: .bold)).foregroundStyle(Theme.tint)
                            .opacity(session.bitrateLimit == bits ? 1 : 0).frame(width: 20)
                        Text(Self.label(bits)).font(Theme.body).foregroundStyle(Theme.label)
                        Spacer(minLength: 0)
                    }
                    .padding(.horizontal, 12).frame(minHeight: 46)
                }
                .buttonStyle(.plain)
            }
            if session.hostBitrate > 0 {
                Text(String(format: "The computer is encoding at %.0f Mb/s now.", Double(session.hostBitrate) / 1_000_000))
                    .font(.system(size: 13)).foregroundStyle(Theme.secondary).padding(.horizontal, 12).padding(.vertical, 8)
            }
        }
        .padding(8).frame(width: 320)
        .environment(\.colorScheme, .dark)
    }
}

/// What the user should hear about without asking: a file on its way, or something that didn't work.
struct SessionNotes: View {
    @EnvironmentObject private var session: ViewerSession

    var body: some View {
        VStack(spacing: 6) {
            if let t = session.transfers.last { note(line(t), failed: { if case .failed = t.state { return true } else { return false } }()) }
            if let n = session.notice { note(n, failed: true) }
        }
        .animation(.snappy, value: session.transfers)
        .animation(.snappy, value: session.notice)
        .task(id: session.notice) {
            guard session.notice != nil else { return }
            try? await Task.sleep(nanoseconds: 6_000_000_000)
            if !Task.isCancelled { session.notice = nil }
        }
        .task(id: session.transfers) {
            guard let t = session.transfers.last, t.state != .moving else { return }
            if case .failed = t.state { Haptics.failure() } else { Haptics.success() }
            try? await Task.sleep(nanoseconds: 5_000_000_000)
            if !Task.isCancelled { session.clearFinishedTransfers() }
        }
    }

    private func line(_ t: Transfer) -> String {
        switch t.state {
        case .moving:
            let part = t.size > 0 ? " · \(min(100, t.moved * 100 / t.size))%" : ""
            return (t.incoming ? "Receiving " : "Sending ") + t.name + part
        case .done(let where_):
            return t.incoming ? "Saved \(where_) in Files, under remoteman" : "\(t.name) is on the computer: \(where_)"
        case .failed(let why):
            return "\(t.name): \(why)"
        }
    }

    private func note(_ text: String, failed: Bool) -> some View {
        Text(text).font(.system(size: 14, weight: .medium)).foregroundStyle(failed ? Color(hex: 0xFFA53D) : .white)
            .lineLimit(2).multilineTextAlignment(.center)
            .padding(.horizontal, 16).padding(.vertical, 10).frame(maxWidth: 520)
            .sessionGlass(radius: 20)
            .environment(\.colorScheme, .dark)
    }
}

/// "Display" on the iPad's bar: where this iPad sits among the computer's own screens.
struct DisplayMenu: View {
    @EnvironmentObject private var model: AppModel
    @EnvironmentObject private var session: ViewerSession
    @Environment(\.dismiss) private var dismiss

    private static let notes: [ViewerSession.Placement: String] = [
        .extend: "Beside the computer's own screens",
        .main: "The computer's screens stay on as extras",
        .only: "The computer's own screens are off while you're connected",
    ]

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            header("This iPad on \(model.current?.name ?? "the computer")")
            ForEach(ViewerSession.Placement.own) { p in
                item(p.label, Self.notes[p] ?? "", checked: model.current?.placementChoice == p) { choose(p) }
            }
            Divider().padding(.vertical, 6)
            header("Size")
            item("Match this iPad", "This iPad's own pixels and refresh rate", checked: !model.moreSpace) { size(false) }
                .available(Features.displaySize && session.has("configure"))
            item("More space", "Smaller text, same pixels", checked: model.moreSpace) { size(true) }
                .available(Features.displaySize && session.has("configure"))
            Divider().padding(.vertical, 6)
            header("More displays")
            item("Add a display on the external monitor", "A monitor connected to this iPad", checked: false) {}
                .available(Features.moreDisplays)
            item(
                "Mirror the computer's main monitor", session.has("mirror") ? "As it is, at its own size; it stays on there" : "This computer can't do this yet",
                checked: model.current?.placementChoice == .mirror
            ) { choose(.mirror) }
            .available(Features.mirror && session.has("mirror"))
        }
        .padding(8).frame(width: 440)
        .environment(\.colorScheme, .dark)
    }

    private func header(_ s: String) -> some View {
        Text(s).font(.system(size: 13, weight: .semibold)).foregroundStyle(Theme.secondary).padding(.horizontal, 12).padding(.vertical, 6)
    }

    private func item(_ title: String, _ sub: String, checked: Bool, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 10) {
                Image(systemName: "checkmark").font(.system(size: 15, weight: .bold)).foregroundStyle(Theme.tint).opacity(checked ? 1 : 0)
                    .frame(width: 20)
                VStack(alignment: .leading, spacing: 1) {
                    Text(title).font(Theme.body).foregroundStyle(Theme.label)
                    Text(sub).font(.system(size: 13)).foregroundStyle(Theme.secondary)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 12).frame(minHeight: 46)
        }
        .buttonStyle(.plain)
    }

    private func choose(_ p: ViewerSession.Placement) {
        Haptics.choice()
        dismiss()
        model.setPlacement(p)
    }

    private func size(_ more: Bool) {
        dismiss()
        model.setMoreSpace(more)
    }
}

/// The performance figures: three across, two down, each a value over what it is.
struct HUD: View {
    @EnvironmentObject private var session: ViewerSession

    var body: some View {
        let s = session.stats
        VStack(alignment: .leading, spacing: 6) {
            Grid(horizontalSpacing: 18, verticalSpacing: 6) {
                GridRow {
                    cell(number(s.shownFps, 0), "frames/s")
                    cell(number(s.ageMs, 0, " ms"), "picture age")
                    cell(number(s.megabitsPerSecond, 0, " Mb/s"), session.bitrateLimit > 0 ? "bitrate, up to \(session.bitrateLimit / 1_000_000)" : "bitrate")
                }
                GridRow {
                    cell(s.codec.isEmpty ? "–" : s.codec, "codec, \(s.chroma.isEmpty ? "–" : s.chroma)")
                    cell("\(s.lost)", "lost frames")
                    cell(number(s.roundTripMs, 0, " ms"), "round trip")
                }
            }
            if s.soundOn {
                Text("Sound: \(Int(s.soundBufferMs)) ms waiting" + (s.soundConcealed + s.soundDropped > 0 ? " · \(s.soundConcealed) patched · \(s.soundDropped) left out" : ""))
                    .font(.system(size: 11)).foregroundStyle(.white.opacity(0.8))
            }
            if s.wifiPausing {
                Text("Wi-Fi is pausing about twice a second").font(.system(size: 11)).foregroundStyle(Color(hex: 0xFFA53D))
            }
        }
        .padding(.horizontal, 14).padding(.vertical, 9)
        .background(Theme.hudGlass, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
        .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 18, style: .continuous).strokeBorder(.white.opacity(0.16), lineWidth: 1))
        .allowsHitTesting(false)
        .environment(\.colorScheme, .dark)
    }

    private func number(_ x: Double, _ digits: Int, _ unit: String = "") -> String {
        x.isNaN ? "–" : String(format: "%.\(digits)f", x) + unit
    }

    private func cell(_ value: String, _ caption: String) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(value).font(.system(size: 15, weight: .semibold)).monospacedDigit().foregroundStyle(.white)
            Text(caption).font(.system(size: 11)).foregroundStyle(.white.opacity(0.8))
        }
        .frame(minWidth: 78, alignment: .leading)
    }
}

/// The keys an iPad's on-screen keyboard lacks. Modifiers latch: a tap holds one down until it
/// is tapped again, so Ctrl then C on the keyboard below is Ctrl-C.
final class KeyStrip: ObservableObject {
    struct Key: Identifiable {
        let label: String
        let usage: UInt16
        var modifier = false
        var id: UInt16 { usage }
    }

    /// Which keys the strip shows, kept as their codes; nothing kept means the usual ones.
    static let shownKey = "keyStrip.shown"
    /// Keys that can be added to the strip besides the usual ones.
    static let extras: [Key] = [
        Key(label: "Home", usage: 0x4A), Key(label: "End", usage: 0x4D), Key(label: "PgUp", usage: 0x4B), Key(label: "PgDn", usage: 0x4E),
        Key(label: "Ins", usage: 0x49), Key(label: "PrtSc", usage: 0x46), Key(label: "Menu", usage: 0x65), Key(label: "⌫", usage: 0x2A),
        Key(label: "↵", usage: 0x28), Key(label: "Space", usage: 0x2C),
    ]
    static var chosen: Set<UInt16> {
        get {
            if let list = UserDefaults.standard.array(forKey: shownKey) as? [Int] { return Set(list.map { UInt16($0) }) }
            return Set(all.flatMap { $0 }.map(\.usage))
        }
        set { UserDefaults.standard.set(newValue.map { Int($0) }.sorted(), forKey: shownKey) }
    }
    /// The strip as chosen: the usual groups, then whatever was added, without what was taken out.
    static var groups: [[Key]] {
        let on = chosen
        return (all + [extras]).map { $0.filter { on.contains($0.usage) } }.filter { !$0.isEmpty }
    }

    static let all: [[Key]] = [
        [Key(label: "Esc", usage: 0x29), Key(label: "Tab", usage: 0x2B)],
        [
            Key(label: "Super", usage: 0xE3, modifier: true), Key(label: "Ctrl", usage: 0xE0, modifier: true),
            Key(label: "Alt", usage: 0xE2, modifier: true), Key(label: "Shift", usage: 0xE1, modifier: true),
        ],
        [Key(label: "←", usage: 0x50), Key(label: "↓", usage: 0x51), Key(label: "↑", usage: 0x52), Key(label: "→", usage: 0x4F)],
        (1...12).map { Key(label: "F\($0)", usage: UInt16(0x3A + $0 - 1)) } + [Key(label: "Del", usage: 0x4C)],
    ]

    @Published private(set) var held: Set<UInt16> = []
    weak var session: ViewerSession?

    func press(_ k: Key) {
        guard let input = session?.input else { return }
        if k.modifier {
            if held.contains(k.usage) {
                held.remove(k.usage)
                input.key(usage: k.usage, down: false)
                Haptics.toggle(false)
            } else {
                held.insert(k.usage)
                input.key(usage: k.usage, down: true)
                Haptics.toggle(true)
            }
        } else {
            Haptics.key()
            input.key(usage: k.usage, down: true)
            input.key(usage: k.usage, down: false)
        }
        session?.modifiersLatched = !held.isEmpty
    }

    func releaseAll() {
        for u in held { session?.input?.key(usage: u, down: false) }
        held = []
        session?.modifiersLatched = false
    }
}
