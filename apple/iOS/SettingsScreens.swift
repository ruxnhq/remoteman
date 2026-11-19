// Settings for one computer, and the app's settings. Most of what the design lays out here
// belongs to features that aren't built yet; those rows are present and dimmed (Features.swift).
import SwiftUI

struct ComputerSettingsScreen: View {
    @EnvironmentObject private var model: AppModel
    @EnvironmentObject private var store: ComputerStore
    @Environment(\.dismiss) private var dismiss
    let computerID: UUID
    @State private var forgetting = false

    private var device: String { UIDevice.current.userInterfaceIdiom == .pad ? "iPad" : "iPhone" }
    private static let notes: [ViewerSession.Placement: String] = [
        .extend: "Beside the computer's own screens",
        .main: "The computer's screens stay on as extras",
        .only: "The computer's own screens are off while you're connected",
    ]

    var body: some View {
        if let c = store.computer(computerID) {
            VStack(spacing: 0) {
                ZStack {
                    Text(c.name).font(Theme.headline)
                    HStack {
                        CircleButton(icon: "xmark", label: "Close") { dismiss() }
                        Spacer()
                    }
                }
                .padding(.horizontal, 14).padding(.top, 14).padding(.bottom, 6)
                ScrollView {
                    VStack(spacing: 22) {
                        InsetGroup {
                            HStack(spacing: 14) {
                                Thumbnail(computer: c).frame(width: 96, height: 72)
                                    .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(c.name).font(Theme.headline)
                                    Text(c.osLine).font(Theme.subhead).foregroundStyle(Theme.secondary)
                                    StatusLine(computer: c, short: false)
                                }
                                Spacer(minLength: 4)
                                Button("Connect") {
                                    dismiss()
                                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.35) { model.connect(c) }
                                }
                                .buttonStyle(WideButtonStyle(small: true))
                                .fixedSize()
                            }
                            .padding(14)
                        }
                        InsetGroup(
                            header: "This \(device)'s display on the computer",
                            footer: "You can switch during a session from \(device == "iPad" ? "Display" : "the controls")."
                        ) {
                            ForEach(Array(ViewerSession.Placement.own.enumerated()), id: \.element) { i, p in
                                Button {
                                    var u = c
                                    u.placementChoice = p
                                    store.update(u)
                                } label: {
                                    Row(title: p.label, subtitle: Self.notes[p], last: i == 2) {
                                        Image(systemName: "checkmark").font(.system(size: 17, weight: .bold)).foregroundStyle(Theme.tint)
                                            .opacity(c.placementChoice == p ? 1 : 0)
                                    }
                                }
                                .buttonStyle(.plain)
                            }
                        }
                        InsetGroup(
                            header: "Getting in",
                            footer: "Automatic attaches to your session when you're logged in at the computer, and shows its login screen when nobody is (on a computer that can). Only a running session never shows a login screen."
                        ) {
                            ChoiceRow(
                                title: "Way in",
                                value: Binding(get: { c.wayIn ?? "any" }, set: { v in
                                    var u = c
                                    u.wayIn = v == "attach" ? "attach" : nil
                                    store.update(u)
                                }),
                                choices: [("any", "Automatic"), ("attach", "Only a running session")])
                            Row(title: "Require \(Gate.name) to connect", subtitle: ViewerPrefs.requireAuth ? "On for every computer, in Settings" : nil) {
                                Toggle("", isOn: Binding(get: { c.auth == "always" || (c.auth == nil && ViewerPrefs.requireAuth) }, set: { on in
                                    var u = c
                                    u.auth = on ? "always" : (ViewerPrefs.requireAuth ? "never" : nil)
                                    store.update(u)
                                })).labelsHidden()
                            }
                            .available(Features.faceID)
                            Row(
                                title: "Wake it before connecting",
                                subtitle: (c.macs ?? []).isEmpty ? "Its network card isn't known yet: it is learned when the computer is seen on this network" : nil,
                                last: true
                            ) {
                                Toggle("", isOn: Binding(get: { c.wake == true }, set: { on in
                                    var u = c
                                    u.wake = on
                                    store.update(u)
                                })).labelsHidden()
                            }
                            .available(Features.wake && !(c.macs ?? []).isEmpty)
                        }
                        InsetGroup(
                            header: "During a session",
                            footer: "What happens when a session with this computer starts. Switching the sound in a session changes this too."
                        ) {
                            Row(title: "Play its sound on this \(device)", last: true) {
                                Toggle("", isOn: Binding(
                                    get: { UserDefaults.standard.object(forKey: "sound." + target(c)) as? Bool ?? ViewerPrefs.soundDefault },
                                    set: { UserDefaults.standard.set($0, forKey: "sound." + target(c)) })
                                ).labelsHidden()
                            }
                            .available(Features.soundChoice)
                        }
                        InsetGroup(header: "Address", footer: c.fingerprint.map { "Key fingerprint \(short($0))" }) {
                            Row(title: "Address", last: true) {
                                TextField("Address", text: Binding(
                                    get: { c.address },
                                    set: { new in
                                        var u = c
                                        u.address = new.trimmingCharacters(in: .whitespaces)
                                        store.update(u)
                                    })
                                )
                                .multilineTextAlignment(.trailing).textInputAutocapitalization(.never).autocorrectionDisabled()
                                .keyboardType(.URL)
                            }
                        }
                        InsetGroup {
                            Button { forgetting = true } label: {
                                Text("Forget This Computer…").font(Theme.body).foregroundStyle(Theme.red)
                                    .frame(maxWidth: .infinity, minHeight: 54)
                            }
                            .buttonStyle(.plain)
                        }
                    }
                    .padding(.horizontal, 20).padding(.vertical, 16)
                }
            }
            .background(Theme.bg.ignoresSafeArea())
            .presentationCornerRadius(40)
            .alert("Forget \(c.name)?", isPresented: $forgetting) {
                Button("Forget", role: .destructive) {
                    store.forget(c)
                    Haptics.warning()
                    dismiss()
                }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("Its key is removed from this \(device). Revoke this \(device) on the computer too.")
            }
        } else {
            Color.clear.onAppear { dismiss() }
        }
    }

    /// The computer's address as the session knows it.
    private func target(_ c: Computer) -> String { c.address.contains(":") ? c.address : c.address + ":7791" }

    private func short(_ f: String) -> String {
        let u = f.uppercased()
        guard u.count >= 12 else { return u }
        return "\(u.prefix(4)) \(u.dropFirst(4).prefix(4)) · · · \(u.suffix(4))"
    }
}

struct SettingsScreen: View {
    @EnvironmentObject private var store: ComputerStore
    @Environment(\.dismiss) private var dismiss
    @AppStorage("figuresInNewSessions") private var figures = false
    @AppStorage(SessionHandle.shownKey) private var showHandle = true
    @State private var editing: Computer?
    @AppStorage(InputSettings.commandKey) private var command = "super"
    @AppStorage(InputSettings.controlKey) private var control = "ctrl"
    @AppStorage(InputSettings.touchModeKey) private var touch = "direct"
    @AppStorage(InputSettings.tapToClickKey) private var tapToClick = true
    @AppStorage(InputSettings.twoFingerRightKey) private var twoFinger = true
    @AppStorage(InputSettings.naturalScrollKey) private var natural = true
    @AppStorage(InputSettings.pointerSpeedKey) private var pointerSpeed = "medium"
    @AppStorage(ViewerPrefs.qualityKey) private var quality = "auto"
    @AppStorage(ViewerPrefs.saveDataKey) private var saveData = true
    @AppStorage(ViewerPrefs.soundDefaultKey) private var soundDefault = false
    @AppStorage(ViewerPrefs.rememberMediaKey) private var rememberMedia = false
    @AppStorage(ViewerPrefs.requireAuthKey) private var requireAuth = false
    @AppStorage(ViewerPrefs.askCameraKey) private var askCamera = false
    @State private var editingStrip = false
    @AppStorage(InputSettings.viewZoomKey) private var viewZoom = true
    private var device: String { UIDevice.current.userInterfaceIdiom == .pad ? "iPad" : "iPhone" }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text("Settings").font(Theme.largeTitle)
                Spacer()
                CircleButton(icon: "checkmark", label: "Done", tinted: true) { dismiss() }
            }
            .padding(.horizontal, 20).padding(.top, 20).padding(.bottom, 8)
            ScrollView {
                VStack(spacing: 22) {
                    InsetGroup(
                        header: "Keyboard",
                        footer: "For a keyboard connected to this \(device). A Mac gets the keys unchanged. Other keys go by position, so the computer's own layout decides what they type."
                    ) {
                        ChoiceRow(title: "Command sends", value: $command, choices: [("super", "Super"), ("ctrl", "Ctrl")])
                            .available(Features.keyRemapping)
                        ChoiceRow(title: "Control sends", value: $control, choices: [("ctrl", "Ctrl"), ("super", "Super")])
                            .available(Features.keyRemapping)
                        Row(title: "Option sends") { RowValue(text: "Alt") }
                        Button { editingStrip = true } label: {
                            Row(title: "Key strip", last: true) { RowValue(text: "\(KeyStrip.groups.flatMap { $0 }.count) keys", chevron: true) }
                        }
                        .buttonStyle(.plain).available(Features.keyStripEditing)
                    }
                    InsetGroup(
                        header: "Pointer and touch",
                        footer: "Direct: your finger touches the computer's screen where it lands. Trackpad: drag anywhere to move the pointer, tap to click, tap then hold to drag, two fingers to scroll."
                    ) {
                        ChoiceRow(title: "Touch", value: $touch, choices: [("direct", "Direct"), ("trackpad", "Trackpad")])
                            .available(Features.trackpadMode)
                        Row(title: "Tap to click") { Toggle("", isOn: $tapToClick).labelsHidden() }
                            .available(Features.pointerOptions && touch == "trackpad")
                        Row(title: "Two-finger tap for right click") { Toggle("", isOn: $twoFinger).labelsHidden() }
                            .available(Features.pointerOptions && touch == "trackpad")
                        Row(title: "Natural scrolling") { Toggle("", isOn: $natural).labelsHidden() }.available(Features.pointerOptions)
                        Row(title: "Pinch to zoom the view") { Toggle("", isOn: $viewZoom).labelsHidden() }
                            .available(Features.viewZoom)
                        ChoiceRow(
                            title: "Pointer speed", last: true, value: $pointerSpeed,
                            choices: [("slow", "Slow"), ("medium", "Medium"), ("fast", "Fast")]
                        )
                        .available(Features.pointerOptions && touch == "trackpad")
                    }
                    InsetGroup(
                        header: "During a session",
                        footer: "The handle is the small bar at the edge of the picture; tap it for the controls, drag it to move it. With it off, tap with three fingers, or press Control-Option-Command-M on a keyboard."
                    ) {
                        Row(title: "Show the session handle", last: true) { Toggle("", isOn: $showHandle).labelsHidden() }
                    }
                    InsetGroup(header: "Picture and sound") {
                        ChoiceRow(title: "Default quality", value: $quality, choices: [("auto", "Automatic"), ("sharp", "Sharp text (4:4:4)")])
                            .available(Features.pictureQuality)
                        ChoiceRow(
                            title: "On mobile data",
                            value: Binding(get: { saveData ? "save" : "same" }, set: { saveData = $0 == "save" }),
                            choices: [("save", "Save data (5 Mb/s)"), ("same", "As on Wi-Fi")]
                        )
                        .available(Features.mobileData)
                        Row(title: "Play the computer's sound", subtitle: "For a computer where you haven't chosen yet") {
                            Toggle("", isOn: $soundDefault).labelsHidden()
                        }
                        .available(Features.soundChoice)
                        ChoiceRow(
                            title: "Microphone and camera",
                            value: Binding(get: { rememberMedia ? "left" : "off" }, set: { rememberMedia = $0 == "left" }),
                            choices: [("off", "Off until I turn them on"), ("left", "As I left them")]
                        )
                        .available(Features.soundChoice)
                        Row(title: "Show figures in new sessions", last: true) { Toggle("", isOn: $figures).labelsHidden() }
                    }
                    InsetGroup(
                        header: "Security",
                        footer: "Passwords stay in this \(device)'s keychain and travel only over the encrypted link to a paired computer."
                    ) {
                        Row(title: "Require \(Gate.name) to connect") { Toggle("", isOn: $requireAuth).labelsHidden() }.available(Features.faceID)
                        Row(title: "Fill login passwords with Face ID") { Toggle("", isOn: .constant(false)).labelsHidden() }
                            .available(Features.remoteLogin)
                        Row(title: "Ask before sharing the camera", last: true) { Toggle("", isOn: $askCamera).labelsHidden() }
                            .available(Features.cameraAsk)
                    }
                    if !store.computers.isEmpty {
                        InsetGroup(
                            header: "Paired computers",
                            footer: "Forgetting a computer here removes its key from this \(device). Revoke this \(device) on the computer too."
                        ) {
                            ForEach(Array(store.computers.enumerated()), id: \.element.id) { i, c in
                                Button { editing = c } label: {
                                    Row(title: c.name, subtitle: "\(c.osLine) · added \(added(c))", last: i == store.computers.count - 1) {
                                        RowValue(text: "", chevron: true)
                                    }
                                }
                                .buttonStyle(.plain)
                            }
                        }
                    }
                }
                .padding(.horizontal, 20).padding(.vertical, 12)
            }
        }
        .background(Theme.bg.ignoresSafeArea())
        .presentationCornerRadius(40)
        .sheet(item: $editing) { ComputerSettingsScreen(computerID: $0.id) }
        .sheet(isPresented: $editingStrip) { KeyStripEditor() }
    }

    private func added(_ c: Computer) -> String {
        let f = DateFormatter()
        f.dateFormat = "d MMM"
        return f.string(from: c.added)
    }
}

/// A row whose value is one of a few choices, picked from a menu.
struct ChoiceRow: View {
    let title: String
    var last = false
    @Binding var value: String
    let choices: [(String, String)]
    var body: some View {
        Row(title: title, last: last) {
            Menu {
                Picker(title, selection: $value) {
                    ForEach(choices, id: \.0) { Text($0.1).tag($0.0) }
                }
            } label: {
                RowValue(text: choices.first { $0.0 == value }?.1 ?? "", chevron: true)
            }
        }
    }
}

/// Choosing the keys of the strip above the keyboard.
struct KeyStripEditor: View {
    @Environment(\.dismiss) private var dismiss
    @State private var chosen = KeyStrip.chosen
    private let sections: [(String, [KeyStrip.Key])] = [
        ("Keys", KeyStrip.all[0]), ("Modifiers", KeyStrip.all[1]), ("Arrows", KeyStrip.all[2]), ("Function keys", KeyStrip.all[3]),
        ("More keys", KeyStrip.extras),
    ]

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text("Key Strip").font(Theme.largeTitle)
                Spacer()
                CircleButton(icon: "checkmark", label: "Done", tinted: true) { dismiss() }
            }
            .padding(.horizontal, 20).padding(.top, 20).padding(.bottom, 8)
            ScrollView {
                VStack(spacing: 22) {
                    ForEach(sections, id: \.0) { title, keys in
                        InsetGroup(header: title) {
                            ForEach(Array(keys.enumerated()), id: \.element.id) { i, k in
                                Row(title: k.label, last: i == keys.count - 1) {
                                    Toggle("", isOn: Binding(
                                        get: { chosen.contains(k.usage) },
                                        set: { on in
                                            if on { chosen.insert(k.usage) } else { chosen.remove(k.usage) }
                                            KeyStrip.chosen = chosen
                                        })
                                    ).labelsHidden()
                                }
                            }
                        }
                    }
                    Button("Back to the Usual Keys") {
                        UserDefaults.standard.removeObject(forKey: KeyStrip.shownKey)
                        chosen = KeyStrip.chosen
                    }
                    .font(Theme.body).foregroundStyle(Theme.tint)
                }
                .padding(.horizontal, 20).padding(.vertical, 12)
            }
        }
        .background(Theme.bg.ignoresSafeArea())
        .presentationCornerRadius(40)
    }
}
