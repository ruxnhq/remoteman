// A live session: the picture, and floating over it the controls of the design. On iPad a bar
// at the top (tucked away while a hardware keyboard is attached), the performance figures, and
// a strip of keys above the on-screen keyboard; on iPhone one capsule that opens a sheet.
import GameController
import SwiftUI

struct SessionScreen: View {
    @EnvironmentObject private var model: AppModel
    @EnvironmentObject private var session: ViewerSession
    @Environment(\.horizontalSizeClass) private var sizeClass
    @State private var keyboardShown = false
    @State private var barShown = GCKeyboard.coalesced == nil
    @State private var displayMenu = false
    @State private var phoneControls = false
    @StateObject private var keys = KeyStrip()

    private var pad: Bool { sizeClass == .regular }
    private var name: String { model.current?.name ?? session.client?.host.name ?? "Computer" }

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            ViewerRepresentable(session: session, keyboardShown: keyboardShown).ignoresSafeArea()
            // Unattended runs show the picture alone.
            if model.current != nil { chrome }
        }
        .environment(\.colorScheme, .dark)
        .onAppear { keys.session = session }
        .onDisappear { keys.releaseAll() }
        .onReceive(NotificationCenter.default.publisher(for: .GCKeyboardDidConnect)) { _ in barShown = false }
        .onReceive(NotificationCenter.default.publisher(for: .GCKeyboardDidDisconnect)) { _ in barShown = true }
        .sheet(isPresented: $phoneControls) { phoneSheet }
    }

    @ViewBuilder
    private var chrome: some View {
        if pad {
            VStack(spacing: 0) {
                if barShown {
                    padBar.padding(.top, 14).transition(.move(edge: .top).combined(with: .opacity))
                } else {
                    Button { withAnimation(.snappy) { barShown = true } } label: {
                        Capsule().fill(.white.opacity(0.45)).frame(width: 40, height: 5).padding(.vertical, 14).padding(.horizontal, 30)
                    }
                    .accessibilityLabel("Show session controls")
                }
                Spacer()
                if keyboardShown { keyStrip.padding(.bottom, 8) }
            }
            .overlay(alignment: .topTrailing) { if session.showStats { HUD().padding(.top, barShown ? 88 : 20).padding(.trailing, 20) } }
        } else {
            VStack {
                Button { phoneControls = true } label: {
                    HStack(spacing: 8) {
                        Circle().fill(Color(hex: 0x45D166)).frame(width: 9, height: 9)
                        Text(name).font(.system(size: 15, weight: .semibold)).lineLimit(1)
                        Image(systemName: "chevron.down").font(.system(size: 12, weight: .bold)).foregroundStyle(.white.opacity(0.7))
                    }
                    .foregroundStyle(.white).padding(.horizontal, 16).frame(height: 44).sessionGlass(radius: 22)
                }
                .accessibilityLabel("Show session controls")
                .padding(.top, 54)
                Spacer()
                if session.showStats { HUD().padding(.bottom, keyboardShown ? 8 : 104) }
            }
        }
    }

    // MARK: iPad bar

    private var padBar: some View {
        HStack(spacing: 6) {
            VStack(alignment: .leading, spacing: 0) {
                Text(name).font(.system(size: 15, weight: .semibold)).lineLimit(1)
                Text(subtitle).font(.system(size: 12, weight: .medium)).foregroundStyle(.white.opacity(0.78)).lineLimit(1)
            }
            .padding(.horizontal, 10)
            separator
            icon("keyboard", "Keyboard", active: keyboardShown) { keyboardShown.toggle() }
            text("Display", "display", active: displayMenu) { displayMenu = true }
                .popover(isPresented: $displayMenu) { DisplayMenu().presentationCompactAdaptation(.popover) }
            text("Picture", "sparkles.tv", active: false) {}.available(Features.pictureQuality)
            text("Touch", "hand.point.up.left", active: false) {}.available(Features.trackpadMode)
            separator
            icon("speaker.wave.2", "Sound", active: false) {}.available(Features.sound)
            icon("mic", "Microphone", active: false) {}.available(Features.microphone)
            icon("camera", "Camera", active: false) {}.available(Features.camera)
            separator
            icon("doc.on.clipboard", "Clipboard", active: false) {}.available(Features.clipboard)
            icon("arrow.up.arrow.down", "Send or fetch files", active: false) {}.available(Features.files)
            icon("gauge.with.dots.needle.33percent", "Show performance figures", active: session.showStats) { session.showStats.toggle() }
            separator
            Button("Disconnect") { model.disconnect() }
                .font(.system(size: 15, weight: .medium)).foregroundStyle(Theme.sessionRed).padding(.horizontal, 12).frame(height: 44)
            if GCKeyboard.coalesced != nil {
                icon("chevron.up", "Hide session controls", active: false) { withAnimation(.snappy) { barShown = false } }
            }
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
                Text(title).font(.system(size: 15, weight: .medium))
            }
            .padding(.horizontal, 12).frame(height: 44)
            .background(active ? .white.opacity(0.20) : .clear, in: RoundedRectangle(cornerRadius: 22, style: .continuous))
        }
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
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text(name).font(Theme.headline)
                    Text(subtitle).font(Theme.footnote).foregroundStyle(Theme.secondary)
                }
                Spacer()
                CircleButton(icon: "xmark", label: "Close controls") { phoneControls = false }
            }
            LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 10), count: 3), spacing: 10) {
                tile("keyboard", "Keyboard", on: keyboardShown) {
                    keyboardShown.toggle()
                    phoneControls = false
                }
                tile("speaker.wave.2", "Sound", on: false) {}.available(Features.sound)
                tile("mic", "Microphone", on: false) {}.available(Features.microphone)
                tile("camera", "Camera", on: false) {}.available(Features.camera)
                tile("doc.on.clipboard", "Clipboard", on: false) {}.available(Features.clipboard)
                tile("arrow.up.arrow.down", "Files", on: false) {}.available(Features.files)
            }
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 6) { ForEach(KeyStrip.groups.flatMap { $0 }) { k in keyButton(k) } }
            }
            VStack(spacing: 0) {
                Menu {
                    Picker("This iPhone on \(name)", selection: placement) {
                        ForEach(ViewerSession.Placement.allCases) { Text($0.label).tag($0) }
                    }
                } label: {
                    Row(title: "Display") { RowValue(text: placement.wrappedValue.label, chevron: true) }
                }
                Row(title: "Picture") { RowValue(text: "Automatic", chevron: true) }.available(Features.pictureQuality)
                Row(title: "Touch") { RowValue(text: "Direct", chevron: false) }.available(Features.trackpadMode)
                Row(title: "Show figures", last: true) { Toggle("", isOn: $session.showStats).labelsHidden() }
            }
            .background(Theme.fill, in: RoundedRectangle(cornerRadius: Theme.cardRadius, style: .continuous))
            Button("Disconnect") {
                phoneControls = false
                model.disconnect()
            }
            .buttonStyle(WideButtonStyle(kind: .destructive))
        }
        .padding(20)
        .presentationDetents([.medium, .large])
        .presentationCornerRadius(38)
        .presentationDragIndicator(.visible)
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

    /// The placement is part of the hello, so choosing another one reconnects.
    private var placement: Binding<ViewerSession.Placement> {
        Binding(
            get: { model.current?.placementChoice ?? .extend },
            set: { new in
                guard var c = model.current, c.placementChoice != new else { return }
                c.placementChoice = new
                model.store.update(c)
                phoneControls = false
                model.disconnect()
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) { model.connect(c) }
            })
    }
}

/// "Display" on the iPad's bar: where this iPad sits among the computer's own screens.
struct DisplayMenu: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss

    private static let notes: [ViewerSession.Placement: String] = [
        .extend: "Beside the computer's own screens",
        .main: "The computer's screens stay on as extras",
        .only: "The computer's own screens are off while you're connected",
    ]

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            header("This iPad on \(model.current?.name ?? "the computer")")
            ForEach(ViewerSession.Placement.allCases) { p in
                item(p.label, Self.notes[p] ?? "", checked: model.current?.placementChoice == p) { choose(p) }
            }
            Divider().padding(.vertical, 6)
            header("Size")
            item("Match this iPad", "This iPad's own pixels and refresh rate", checked: true) {}.available(Features.displaySize)
            item("More space", "Smaller text, same pixels", checked: false) {}.available(Features.displaySize)
            Divider().padding(.vertical, 6)
            header("More displays")
            item("Add a display on the external monitor", "A monitor connected to this iPad", checked: false) {}
                .available(Features.moreDisplays)
            item("Mirror one of the computer's monitors", "Show a monitor the computer already has", checked: false) {}
                .available(Features.moreDisplays)
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
        guard var c = model.current, c.placementChoice != p else { return dismiss() }
        c.placementChoice = p
        model.store.update(c)
        dismiss()
        model.disconnect()
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) { model.connect(c) }
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
                    cell(number(s.megabitsPerSecond, 0, " Mb/s"), "bitrate")
                }
                GridRow {
                    cell(s.codec.isEmpty ? "–" : s.codec, "codec, \(s.chroma.isEmpty ? "–" : s.chroma)")
                    cell("\(s.lost)", "lost frames")
                    cell(number(s.roundTripMs, 0, " ms"), "round trip")
                }
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

    static let groups: [[Key]] = [
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
            } else {
                held.insert(k.usage)
                input.key(usage: k.usage, down: true)
            }
        } else {
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
