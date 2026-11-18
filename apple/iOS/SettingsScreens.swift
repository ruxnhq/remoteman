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
                            ForEach(Array(ViewerSession.Placement.allCases.enumerated()), id: \.element) { i, p in
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
                            footer: "Automatic attaches to your session when you're logged in at the computer, and shows its login screen when nobody is."
                        ) {
                            Row(title: "Way in") { RowValue(text: "Automatic", chevron: true) }.available(Features.remoteLogin)
                            Row(title: "Require Face ID to connect") { Toggle("", isOn: .constant(false)).labelsHidden() }
                                .available(Features.faceID)
                            Row(title: "Wake it when it's asleep", last: true) { Toggle("", isOn: .constant(false)).labelsHidden() }
                                .available(Features.wake)
                        }
                        InsetGroup(header: "During a session") {
                            Row(title: "Picture quality") { RowValue(text: "Automatic", chevron: true) }.available(Features.pictureQuality)
                            Row(title: "Sound") { RowValue(text: "Plays on this \(device)", chevron: true) }.available(Features.soundChoice)
                            Row(title: "Camera and microphone", last: true) { RowValue(text: "Off until I turn them on", chevron: true) }
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
    @State private var editing: Computer?
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
                        footer: "For a keyboard connected to this \(device). Other keys go by position, so the computer's own layout decides what they type."
                    ) {
                        Row(title: "Command sends") { RowValue(text: "Super", chevron: true) }.available(Features.keyRemapping)
                        Row(title: "Control sends") { RowValue(text: "Ctrl", chevron: true) }.available(Features.keyRemapping)
                        Row(title: "Option sends") { RowValue(text: "Alt", chevron: true) }.available(Features.keyRemapping)
                        Row(title: "Key strip", last: true) { RowValue(text: "Esc, Tab and 19 more", chevron: true) }
                            .available(Features.keyStripEditing)
                    }
                    InsetGroup(
                        header: "Pointer and touch",
                        footer: "Direct: your finger is the pointer where it touches. Trackpad: drag anywhere to move the pointer, like a laptop's trackpad."
                    ) {
                        Row(title: "Touch") { RowValue(text: "Direct") }.available(Features.trackpadMode)
                        Row(title: "Tap to click") { Toggle("", isOn: .constant(true)).labelsHidden() }.available(Features.pointerOptions)
                        Row(title: "Two-finger tap for right click") { Toggle("", isOn: .constant(true)).labelsHidden() }
                            .available(Features.pointerOptions)
                        Row(title: "Natural scrolling") { Toggle("", isOn: .constant(true)).labelsHidden() }.available(Features.pointerOptions)
                        Row(title: "Pinch to zoom the view") { Toggle("", isOn: .constant(true)).labelsHidden() }
                            .available(Features.pointerOptions)
                        Row(title: "Pointer speed", last: true) { RowValue(text: "Medium", chevron: true) }.available(Features.pointerOptions)
                    }
                    InsetGroup(header: "Picture and sound") {
                        Row(title: "Default quality") { RowValue(text: "Automatic", chevron: true) }.available(Features.pictureQuality)
                        Row(title: "On mobile data") { RowValue(text: "Save data", chevron: true) }.available(Features.mobileData)
                        Row(title: "Play the computer's sound") { Toggle("", isOn: .constant(false)).labelsHidden() }.available(Features.soundChoice)
                        Row(title: "Show figures in new sessions", last: true) { Toggle("", isOn: $figures).labelsHidden() }
                    }
                    InsetGroup(
                        header: "Security",
                        footer: "Passwords stay in this \(device)'s keychain and travel only over the encrypted link to a paired computer."
                    ) {
                        Row(title: "Require Face ID to connect") { Toggle("", isOn: .constant(false)).labelsHidden() }.available(Features.faceID)
                        Row(title: "Fill login passwords with Face ID") { Toggle("", isOn: .constant(false)).labelsHidden() }
                            .available(Features.faceID)
                        Row(title: "Ask before sharing the camera", last: true) { Toggle("", isOn: .constant(true)).labelsHidden() }
                            .available(Features.soundChoice)
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
    }

    private func added(_ c: Computer) -> String {
        let f = DateFormatter()
        f.dateFormat = "d MMM"
        return f.string(from: c.added)
    }
}
