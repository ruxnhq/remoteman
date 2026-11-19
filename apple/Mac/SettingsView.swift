// The Settings window: General, Keyboard and Pointer, Computers. What the engine can't be told
// yet is drawn as designed and disabled (Features.swift); a disabled switch shows what happens
// today (the pointer is drawn on this Mac, sound is not played).
import SwiftUI

struct SettingsView: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        TabView {
            GeneralSettings()
                .tabItem { Label("General", systemImage: "gearshape") }
                .frame(width: 680, height: 680)
            KeyboardPointerSettings()
                .tabItem { Label("Keyboard and Pointer", systemImage: "keyboard") }
                .frame(width: 680, height: 700)
            ComputerSettings(store: model.store)
                .tabItem { Label("Computers", systemImage: "desktopcomputer") }
                .frame(width: 680, height: 620)
        }
        .background(Tokens.win)
    }
}

/// A settings page: groups 18 apart, padded 20 by 24, scrolling if the window is short.
private struct SettingsPage<Content: View>: View {
    @ViewBuilder var content: Content
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) { content }
                .padding(.vertical, 20).padding(.horizontal, 24)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// A switch that can't be changed yet, showing `value`.
private struct FixedSwitch: View {
    var title: String
    var value: Bool
    var flag: Bool
    var body: some View {
        Toggle(title, isOn: .constant(value)).labelsHidden().toggleStyle(.switch).available(flag)
    }
}

/// A pop-up that can't be changed yet, showing `value`.
private struct FixedPopUp: View {
    var title: String
    var value: String
    var flag: Bool
    var body: some View {
        Picker(title, selection: .constant(0)) { Text(value).tag(0) }.labelsHidden().fixedSize().available(flag)
    }
}

struct GeneralSettings: View {
    @AppStorage(AppModel.WindowControls.key) private var controls = AppModel.WindowControls.titleBar.rawValue
    @AppStorage(AppModel.showFiguresKey) private var showFigures = false
    @AppStorage(ViewerPrefs.qualityKey) private var quality = "auto"
    @AppStorage(ViewerPrefs.fullRefreshKey) private var fullRefresh = true
    @AppStorage(ViewerPrefs.soundDefaultKey) private var sound = false
    @AppStorage(ViewerPrefs.clipboardDefaultKey) private var clipboard = false
    @AppStorage(ViewerPrefs.rememberMediaKey) private var rememberMedia = false
    @AppStorage(ViewerPrefs.requireAuthKey) private var requireAuth = false

    var body: some View {
        SettingsPage {
            CardGroup("Session window") {
                Row(
                    "Window controls",
                    caption: "Without a title bar the window shows only the picture. The controls and the window buttons slide in when the pointer reaches the top edge."
                ) {
                    Picker("Window controls", selection: $controls) {
                        Text("In the title bar").tag(AppModel.WindowControls.titleBar.rawValue)
                        Text("Show when the pointer is at the top").tag(AppModel.WindowControls.slideIn.rawValue)
                    }
                    .pickerStyle(.radioGroup).labelsHidden().fixedSize()
                }
            }
            CardGroup("Picture") {
                Row(
                    "Default quality",
                    caption: "Automatic picks the best stream the computer and this Mac both handle. Sharp Text asks for full colour detail (4:4:4) first, which uses more data. Applies from the next connection."
                ) {
                    Picker("Default quality", selection: $quality) {
                        Text("Automatic").tag("auto")
                        Text("Sharp Text (4:4:4)").tag("sharp")
                    }
                    .labelsHidden().fixedSize().available(Features.pictureQuality)
                }
                Row("Use this display's full refresh rate (\(NSScreen.main?.maximumFramesPerSecond ?? 60) Hz)", caption: "Off, the computer is asked for 60 Hz.") {
                    Toggle("Use this display's full refresh rate", isOn: $fullRefresh).labelsHidden().toggleStyle(.switch)
                        .available(Features.refreshRateChoice)
                }
                Row("Show figures when a session starts") {
                    Toggle("Show figures when a session starts", isOn: $showFigures).labelsHidden().toggleStyle(.switch)
                }
            }
            CardGroup("Sound and sharing") {
                Row(
                    "Play the computer's sound on this Mac",
                    caption: "For a computer where you haven't chosen yet. Afterwards each computer keeps what you last chose in its session."
                ) {
                    Toggle("Play the computer's sound on this Mac", isOn: $sound).labelsHidden().toggleStyle(.switch)
                        .available(Features.defaultsForSharing)
                }
                Row("Clipboard", caption: "Also for a computer where you haven't chosen yet, and only if the computer allows it.") {
                    Picker("Clipboard", selection: $clipboard) {
                        Text("Off until I turn it on").tag(false)
                        Text("Shared both ways").tag(true)
                    }
                    .labelsHidden().fixedSize().available(Features.defaultsForSharing)
                }
                Row(
                    "Microphone and camera",
                    caption: "As I left them: a computer you had them on for gets them again when you connect."
                ) {
                    Picker("Microphone and camera", selection: $rememberMedia) {
                        Text("Off until I turn them on").tag(false)
                        Text("As I left them").tag(true)
                    }
                    .labelsHidden().fixedSize().available(Features.defaultsForSharing)
                }
                Row("Received files are saved to", value: "Downloads", dim: true)
            }
            CardGroup("Security") {
                Row(
                    "Require Touch ID to connect",
                    caption: "Touch ID or your Mac password, each time. You can change this for a single computer under Computers."
                ) {
                    Toggle("Require Touch ID to connect", isOn: $requireAuth).labelsHidden().toggleStyle(.switch).available(Features.touchID)
                }
            }
        }
    }
}

struct KeyboardPointerSettings: View {
    @EnvironmentObject private var model: AppModel
    @AppStorage(SystemKeys.settingKey) private var systemShortcuts = false
    @State private var shortcutsNote = ""
    @AppStorage(InputSettings.commandKey) private var command = "super"
    @AppStorage(InputSettings.controlKey) private var control = "ctrl"
    @AppStorage(InputSettings.localPointerKey) private var localPointer = true
    @AppStorage(InputSettings.capturedKey) private var captured = false
    @AppStorage(InputSettings.naturalScrollKey) private var natural = true
    @AppStorage(InputSettings.scrollSpeedKey) private var speed = 0.5
    @AppStorage(InputSettings.pinchZoomsKey) private var pinch = true

    var body: some View {
        SettingsPage {
            CardGroup("Keyboard", footnote: "When connected to a Mac, the keys are sent unchanged.") {
                Row("⌘ Command sends", caption: "Ctrl makes ⌘C and ⌘V copy and paste on a Linux computer.") {
                    Picker("⌘ Command sends", selection: $command) {
                        Text("Ctrl").tag("ctrl")
                        Text("Super").tag("super")
                    }
                    .pickerStyle(.segmented).labelsHidden().fixedSize().available(Features.keyboardMapping)
                }
                Row("⌃ Control sends") {
                    Picker("⌃ Control sends", selection: $control) {
                        Text("Ctrl").tag("ctrl")
                        Text("Super").tag("super")
                    }
                    .pickerStyle(.segmented).labelsHidden().fixedSize().available(Features.keyboardMapping)
                }
                Row("⌥ Option sends", value: "Alt", dim: true)
                Row(
                    "Send system shortcuts to the computer",
                    caption: shortcutsNote.isEmpty ? "In full screen, ⌘Tab, ⌘Space, ⌘Q and ⌘` go to the computer instead of this Mac. macOS asks once for Accessibility." : shortcutsNote
                ) {
                    Toggle("Send system shortcuts to the computer", isOn: Binding(
                        get: { systemShortcuts },
                        set: { on in
                            // macOS lets an app catch these keys only with Accessibility: asked for here.
                            systemShortcuts = on && SystemKeys.askForAccess()
                            if on, !systemShortcuts { shortcutsNote = "Allow remoteman under System Settings, Privacy & Security, Accessibility, then turn this on again." }
                            model.systemKeys.refresh()
                        })
                    ).labelsHidden().toggleStyle(.switch).available(Features.systemShortcuts)
                }
                Row(
                    "Keys are sent", caption: "By position suits a computer set to the same layout as this keyboard.",
                    value: "By position on the keyboard", dim: true)
            }
            CardGroup("Pointer") {
                Row(
                    "Draw the pointer on this Mac",
                    caption: "It moves at once, without waiting for the picture. Off, the computer draws it into the picture. Applies from the next connection."
                ) {
                    Toggle("Draw the pointer on this Mac", isOn: $localPointer).labelsHidden().toggleStyle(.switch).available(Features.pointerOptions)
                }
                Row(
                    "Pointer movement",
                    caption: "Captured sends raw movement, for 3D apps and games: click the picture to hold the pointer there, press ⌃⌥ to release."
                ) {
                    Picker("Pointer movement", selection: $captured) {
                        Text("Normal").tag(false)
                        Text("Captured").tag(true)
                    }
                    .pickerStyle(.segmented).labelsHidden().fixedSize().available(Features.pointerOptions)
                }
                Row("Scroll direction") {
                    Picker("Scroll direction", selection: $natural) {
                        Text("Natural, as on this Mac").tag(true)
                        Text("Reversed").tag(false)
                    }
                    .labelsHidden().fixedSize().available(Features.scrollOptions)
                }
                Row("Scroll speed") {
                    HStack(spacing: 8) {
                        Text("Slow").font(.system(size: 11)).foregroundStyle(Tokens.l2)
                        Slider(value: $speed, in: 0...1).frame(width: 200).available(Features.scrollOptions)
                        Text("Fast").font(.system(size: 11)).foregroundStyle(Tokens.l2)
                    }
                }
                Row("Trackpad pinch") {
                    Picker("Trackpad pinch", selection: $pinch) {
                        Text("Zooms on the computer").tag(true)
                        Text("Does nothing").tag(false)
                    }
                    .labelsHidden().fixedSize().available(Features.scrollOptions)
                }
            }
        }
    }
}

struct ComputerSettings: View {
    @EnvironmentObject private var model: AppModel
    @ObservedObject var store: ComputerStore
    @State private var selection: SavedComputer.ID?
    @State private var adding = false
    @State private var renaming: SavedComputer?
    @State private var forgetting: SavedComputer?

    private var selected: SavedComputer? { store[selection] ?? store.computers.first }

    var body: some View {
        SettingsPage {
            CardGroup("Paired computers") {
                ForEach(store.computers) { c in
                    listRow(c)
                }
                if store.computers.isEmpty {
                    Row("No computers yet", caption: "Add one with the plus button.") { EmptyView() }
                }
                HStack(spacing: 2) {
                    Button { adding = true } label: { Image(systemName: "plus").frame(width: 22, height: 18) }
                        .help("Pair a computer").accessibilityLabel("Pair a computer")
                    Button { forgetting = selected } label: { Image(systemName: "minus").frame(width: 22, height: 18) }
                        .help("Forget \(selected?.name ?? "computer")").accessibilityLabel("Forget \(selected?.name ?? "computer")")
                        .disabled(selected == nil)
                    Spacer()
                }
                .buttonStyle(.borderless)
                .padding(.horizontal, 8)
                .frame(height: 30)
            }
            if let c = selected {
                CardGroup(c.name) {
                    Row("Name") {
                        Text(c.name).foregroundStyle(Tokens.l2)
                        Button("Rename…") { renaming = c }
                    }
                    Row("Address", value: c.address, dim: true)
                    Row(
                        "This Mac's display joins as",
                        caption: "Extra display, main display or only display. You can change it during a session."
                    ) {
                        PlacementPicker(computer: c)
                    }
                    Row(
                        "Way in",
                        caption: "For a computer that can show its login screen to a device when nobody is logged in. Other computers take no notice."
                    ) {
                        Picker("Way in", selection: Binding(get: { c.wayIn }, set: { v in store.update(c.id) { $0.wayIn = v } })) {
                            Text("Automatic").tag("")
                            Text("Only a session that is already running").tag("attach")
                        }
                        .labelsHidden().fixedSize()
                    }
                    Row("Require Touch ID to connect") {
                        Picker("Require Touch ID to connect", selection: Binding(get: { c.auth }, set: { v in store.update(c.id) { $0.auth = v } })) {
                            Text("Use the default (\(ViewerPrefs.requireAuth ? "on" : "off"))").tag("")
                            Text("Always").tag("always")
                            Text("Never").tag("never")
                        }
                        .labelsHidden().fixedSize().available(Features.touchID)
                    }
                    Row(
                        "Wake it before connecting",
                        caption: c.macs.isEmpty
                            ? "Its network card's address isn't known yet. It is learned when the computer is seen on this network (Nearby, in the main window)."
                            : "Sends the wake signal on this network first, to \(c.macs.joined(separator: ", "))."
                    ) {
                        Toggle("Wake it before connecting", isOn: Binding(get: { c.wake }, set: { on in store.update(c.id) { $0.wake = on } }))
                            .labelsHidden().toggleStyle(.switch).available(Features.wake && !c.macs.isEmpty)
                    }
                    Row("Key fingerprint", value: Fingerprint.shown(c.fingerprint), dim: c.fingerprint.isEmpty, mono: true)
                    Row(
                        "Forget this computer",
                        caption: Features.forgetKey
                            ? "Removes its key from this Mac. Pair again to reconnect."
                            : "Takes it off this Mac's list."
                    ) {
                        Button("Forget…") { forgetting = c }.foregroundStyle(Tokens.red)
                    }
                }
            }
        }
        .sheet(isPresented: $adding) {
            AddComputerSheet { selection = $0 }
        }
        .renameAlert($renaming, store: store)
        .forgetDialog($forgetting) { c in
            // A computer in a session is left alone.
            guard model.active != c.id else { return }
            store.remove(c.id)
            model.errors[c.id] = nil
            if selection == c.id { selection = nil }
            if model.selection == c.id { model.selection = store.computers.first?.id }
        }
    }

    private func listRow(_ c: SavedComputer) -> some View {
        let on = selected?.id == c.id
        let detail = [c.osLabel, c.paired.map { "paired \(Wording.day($0))" } ?? "not paired yet"]
            .filter { !$0.isEmpty }.joined(separator: " · ")
        return HStack(spacing: 8) {
            Image(systemName: c.symbol).foregroundStyle(on ? Color.white : Tokens.tintfg).frame(width: 20)
            Text(c.name).font(.system(size: 13, weight: on ? .medium : .regular))
            Spacer()
            Text(detail).font(.system(size: 11)).foregroundStyle(on ? Color.white.opacity(0.85) : Tokens.l2)
        }
        .foregroundStyle(on ? Color.white : Tokens.label)
        .padding(.horizontal, 12)
        .frame(height: 32)
        .background(on ? Tokens.tint : Color.clear)
        .contentShape(Rectangle())
        .onTapGesture { selection = c.id }
        .accessibilityAddTraits(on ? [.isButton, .isSelected] : .isButton)
    }
}
