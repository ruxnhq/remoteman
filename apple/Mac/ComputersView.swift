// The main window: the computers this Mac knows in a sidebar, and the selected one's details.
import SwiftUI

struct ComputersView: View {
    @EnvironmentObject private var model: AppModel
    @State private var renaming: SavedComputer?
    @State private var forgetting: SavedComputer?

    var body: some View {
        ComputersContent(store: model.store, renaming: $renaming, forgetting: $forgetting)
            .sheet(isPresented: $model.addSheet) {
                AddComputerSheet { model.selection = $0 }
            }
            .sheet(item: pairedBinding) { c in
                PairedSheet(computer: c)
            }
            .renameAlert($renaming, store: model.store)
            .forgetDialog($forgetting) { c in
                model.store.remove(c.id)
                model.errors[c.id] = nil
                if model.selection == c.id { model.selection = model.store.computers.first?.id }
            }
    }

    private var pairedBinding: Binding<SavedComputer?> {
        Binding(get: { model.store[model.pairedSheet] }, set: { if $0 == nil { model.pairedSheet = nil } })
    }
}

private struct ComputersContent: View {
    @EnvironmentObject private var model: AppModel
    @ObservedObject var store: ComputerStore
    @ObservedObject private var nearby = Nearby.shared

    /// Seen on this network and not paired with this Mac.
    private var strangers: [Nearby.Found] {
        nearby.found.filter { f in !store.computers.contains { !f.fp.isEmpty && $0.fingerprint.lowercased().hasPrefix(f.fp) } }
    }
    @Binding var renaming: SavedComputer?
    @Binding var forgetting: SavedComputer?

    var body: some View {
        NavigationSplitView {
            List(selection: $model.selection) {
                Section("Paired") {
                    ForEach(store.computers) { c in
                        SidebarRow(computer: c).tag(c.id)
                    }
                }
                Section("Nearby") {
                    if nearby.refused {
                        Text("macOS isn't letting remoteman look. Allow it under System Settings, Privacy & Security, Local Network.")
                            .font(.system(size: 11)).foregroundStyle(.secondary).selectionDisabled()
                    } else if !nearby.looking && !Nearby.wanted {
                        // Looking is asked for: the first time, macOS asks whether remoteman may.
                        Button("Look on This Network") { nearby.look() }
                            .buttonStyle(.link).font(.system(size: 12)).selectionDisabled()
                    } else if strangers.isEmpty {
                        Text(nearby.found.isEmpty ? "No computers found on this network yet." : "Every computer found here is already paired.")
                            .font(.system(size: 11)).foregroundStyle(.tertiary).selectionDisabled()
                    }
                    ForEach(strangers) { f in
                        HStack(spacing: 8) {
                            Label {
                                Text(f.name).lineLimit(1)
                            } icon: {
                                Image(systemName: f.os == "macos" ? "macmini" : "desktopcomputer").foregroundStyle(.secondary)
                            }
                            Spacer(minLength: 4)
                            Button("Add") {
                                guard let address = f.address else { return }
                                var c = SavedComputer(name: f.name, address: address)
                                c.pairOnConnect = true
                                c.os = f.os
                                c.macs = f.macs
                                store.add(c)
                                model.selection = c.id
                            }
                            .controlSize(.small).disabled(f.address == nil)
                        }
                        .selectionDisabled()
                    }
                }
            }
            .contextMenu(forSelectionType: SavedComputer.ID.self) { ids in
                if let c = store[ids.first] {
                    Button("Connect") { model.connect(c) }
                    Divider()
                    Button("Rename…") { renaming = c }
                    Button("Forget…", role: .destructive) { forgetting = c }
                }
            } primaryAction: { ids in
                if let c = store[ids.first] { model.connect(c) }
            }
            .safeAreaInset(edge: .bottom) { UpdateNotice(updater: model.updater, compact: true) }
            .navigationSplitViewColumnWidth(min: 200, ideal: 248, max: 320)
        } detail: {
            Group {
                if let c = model.selected {
                    ComputerDetail(computer: c)
                } else {
                    EmptyDetail()
                }
            }
            .background(Tokens.win)
        }
        .navigationTitle(model.selected?.name ?? "Computers")
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                Button {
                    model.addSheet = true
                } label: {
                    Label("Pair a computer", systemImage: "plus")
                }
                .help("Pair a computer")
                if let c = model.selected {
                    Menu {
                        Button("Connect") { model.connect(c) }
                        Divider()
                        Button("Rename…") { renaming = c }
                        Button("Forget…", role: .destructive) { forgetting = c }
                    } label: {
                        Label("More actions for \(c.name)", systemImage: "ellipsis")
                    }
                    .menuIndicator(.hidden)
                    .help("More actions for \(c.name)")
                }
            }
        }
    }
}

private struct SidebarRow: View {
    let computer: SavedComputer
    @ObservedObject private var nearby = Nearby.shared
    var body: some View {
        HStack(spacing: 8) {
            Label {
                Text(computer.name).lineLimit(1)
            } icon: {
                Image(systemName: computer.symbol).foregroundStyle(Tokens.tintfg)
            }
            Spacer(minLength: 4)
            // Online / Asleep / Not reachable would sit here; until a computer's state can be
            // told, only "Not paired" is said, of one that has never been connected to.
            if computer.pairOnConnect {
                Text("Not paired").font(.system(size: 11)).foregroundStyle(.secondary)
            } else if nearby.seen(fingerprint: computer.fingerprint) != nil {
                // Seen on this network now. Not seen says nothing: it may be reached another way.
                Circle().fill(Color.green).frame(width: 7, height: 7).help("On this network now").accessibilityLabel("On this network now")
            }
        }
    }
}

/// No computer is selected, because none has been added.
private struct EmptyDetail: View {
    @EnvironmentObject private var model: AppModel
    var body: some View {
        VStack(spacing: 10) {
            Image(systemName: "desktopcomputer").font(.system(size: 52, weight: .light)).foregroundStyle(Tokens.l2)
            Text("No computers yet").font(.system(size: 17, weight: .bold)).foregroundStyle(Tokens.label)
            Text("Add a computer that runs remoteman by its name or address.")
                .font(.system(size: 13)).foregroundStyle(Tokens.l2)
            Button("Pair…") { model.addSheet = true }.padding(.top, 6)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// The detail pane of a saved computer: header, the main button, and its card.
struct ComputerDetail: View {
    @EnvironmentObject private var model: AppModel
    let computer: SavedComputer
    @State private var wakeNote: String?

    private var error: String? { model.errors[computer.id] }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                header
                card
            }
            .padding(.vertical, 28)
            .padding(.horizontal, 32)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 26) {
            thumbnail
            VStack(alignment: .leading, spacing: 6) {
                Text(computer.name).font(.system(size: 26, weight: .bold)).tracking(-0.5).foregroundStyle(Tokens.label)
                Text(computer.osLabel.isEmpty ? "Not connected to yet" : computer.osLabel)
                    .font(.system(size: 14)).foregroundStyle(Tokens.l2)
                statusLine
                HStack(spacing: 10) {
                    Button(buttonTitle) { model.connect(computer) }
                        .buttonStyle(LargeButtonStyle(primary: true))
                        .keyboardShortcut(.defaultAction)
                    if error == nil {
                        Text("or double-click it in the sidebar").font(.system(size: 11)).foregroundStyle(Tokens.l2)
                    } else {
                        Button(wakeNote ?? "Send Wake Signal") {
                            let n = Wake.send(macs: computer.macs, lastAddress: computer.address)
                            wakeNote = n > 0 ? "Wake Signal Sent" : "Couldn't Send It"
                            DispatchQueue.main.asyncAfter(deadline: .now() + 3) { wakeNote = nil }
                        }
                        .buttonStyle(LargeButtonStyle())
                        .available(Features.wake && !computer.macs.isEmpty)
                        .help(computer.macs.isEmpty ? "Its network card's address isn't known yet: it is learned when the computer is seen on this network." : "Wakes it if it is asleep on this network")
                    }
                }
                .padding(.top, 8)
                if computer.pairOnConnect {
                    Text("\(computer.name) has to be in pairing mode for the first connection: on a Mac, choose Pair a Device… in its remoteman menu.")
                        .font(.system(size: 11)).foregroundStyle(Tokens.l2)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }

    private var buttonTitle: String {
        if error != nil { return "Try Again" }
        return computer.pairOnConnect ? "Pair and Connect" : "Connect"
    }

    /// Where the design says "Online, on this network": what is known today is the address,
    /// and how the last attempt went.
    @ViewBuilder private var statusLine: some View {
        if let error {
            VStack(alignment: .leading, spacing: 2) {
                Text("Couldn't connect").font(.system(size: 14, weight: .semibold)).foregroundStyle(Tokens.label)
                Text(error).font(.system(size: 13)).foregroundStyle(Tokens.l2)
                    .fixedSize(horizontal: false, vertical: true).textSelection(.enabled)
            }
        } else {
            Text(computer.address).font(.system(size: 14)).foregroundStyle(Tokens.l2).textSelection(.enabled)
        }
    }

    /// The last picture would be here; there is none to show yet, so the frame is dimmed.
    @ViewBuilder private var thumbnail: some View {
        if let image = model.store.thumbnail(computer.id) {
            Image(nsImage: image).resizable().aspectRatio(contentMode: .fill)
                .frame(width: 336, height: 210)
                .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
                .accessibilityLabel("The last picture of \(computer.name)")
        } else {
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(Tokens.thumbnail)
                .frame(width: 336, height: 210)
                .overlay(Image(systemName: computer.symbol).font(.system(size: 52, weight: .light)).foregroundStyle(.white.opacity(0.7)))
                .accessibilityHidden(true)
        }
    }

    private var card: some View {
        Card {
            Row("Getting in", caption: "remoteman joins the session that is logged in at \(computer.name).") {
                Text("Attach").font(.system(size: 13, weight: .medium)).foregroundStyle(Tokens.label)
            }
            Row("Last used", value: Wording.lastUsed(computer), dim: computer.lastUsed == nil)
            Row("Display") {
                Picker("Display", selection: Binding(get: { computer.screen }, set: { v in model.store.update(computer.id) { $0.screen = v } })) {
                    Text("Wherever the window is").tag("")
                    ForEach(NSScreen.screens.map(\.localizedName), id: \.self) { Text($0).tag($0) }
                    // A display that was chosen and isn't connected now stays in the list.
                    if !computer.screen.isEmpty, !NSScreen.screens.contains(where: { $0.localizedName == computer.screen }) {
                        Text("\(computer.screen) (not connected)").tag(computer.screen)
                    }
                }
                .labelsHidden().fixedSize().available(Features.displayChoice)
            }
            Row("This display joins as", caption: PlacementCopy.caption(computer.place, name: computer.name)) {
                PlacementPicker(computer: computer)
            }
            Row("Picture") {
                Picker("Picture", selection: .constant(0)) {
                    Text("Automatic").tag(0)
                    Text("Sharp text (4:4:4)").tag(1)
                }
                .labelsHidden().fixedSize().available(Features.pictureQuality)
            }
            Row("Play its sound on this Mac") {
                Toggle("Play its sound on this Mac", isOn: .constant(false))
                    .labelsHidden().toggleStyle(.switch).available(Features.defaultsForSharing)
            }
            Row(
                "Paired", caption: "Recognised by its key. No password is stored on this Mac.",
                value: computer.paired.map(Wording.day) ?? "On the first connection", dim: computer.paired == nil)
        }
    }
}

/// The words for where this display sits among the computer's own.
enum PlacementCopy {
    static func caption(_ p: ViewerSession.Placement, name: String) -> String {
        switch p {
        case .extend: return "Beside \(name)'s own screens, which stay on. You can change this during a session."
        case .main: return "As \(name)'s main screen; its own screens stay on. You can change this during a session."
        case .only: return "\(name)'s own screens are switched off while you're connected. You can change this during a session."
        case .mirror: return "\(name)'s main monitor is shown as it is, and stays on there. You can change this during a session."
        }
    }
    static func hint(_ p: ViewerSession.Placement) -> String {
        switch p {
        case .extend: return "Beside its own screens"
        case .main: return "Its own screens stay on"
        case .only: return "Its own screens are off"
        case .mirror: return "Its main monitor, as it is"
        }
    }
}

/// The pop-up for a computer's display placement, saved as soon as it is changed.
struct PlacementPicker: View {
    @EnvironmentObject private var model: AppModel
    let computer: SavedComputer
    var body: some View {
        Picker(
            "This display joins as",
            selection: Binding(get: { computer.place }, set: { p in model.store.update(computer.id) { $0.place = p } })
        ) {
            ForEach(ViewerSession.Placement.own) { Text($0.title).tag($0) }
        }
        .labelsHidden().fixedSize()
    }
}

// MARK: Adding and pairing

/// "Pair a computer": the design pairs by a 6-digit code with a computer found nearby, neither
/// of which exists yet, so this is where a computer is added by its address. Pairing itself
/// happens on the first connection, while the computer is in pairing mode.
struct AddComputerSheet: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    var added: (SavedComputer.ID) -> Void
    @State private var address = ""
    @State private var name = ""
    @State private var pair = true

    private var trimmed: String { address.trimmingCharacters(in: .whitespaces) }

    var body: some View {
        VStack(spacing: 14) {
            Text("Pair a computer").font(.system(size: 17, weight: .bold)).tracking(-0.2)
            Text("Enter the computer's name or IP address. If it listens on a port other than 7791, add it after a colon.")
                .font(.system(size: 13)).foregroundStyle(Tokens.l2).multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            VStack(alignment: .leading, spacing: 8) {
                TextField("Name or address, for example office-pc or 10.0.0.12:7791", text: $address)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit(add)
                TextField("Name to show in the list (optional)", text: $name)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit(add)
                Toggle("Pair with this computer (first connection only)", isOn: $pair)
                Text("Put the computer in pairing mode before you connect for the first time. After that it recognises this Mac by its key.")
                    .font(.system(size: 11)).foregroundStyle(Tokens.l2).fixedSize(horizontal: false, vertical: true)
            }
            // The designed way: compare a code shown on both sides. Not there yet.
            VStack(spacing: 8) {
                HStack(spacing: 6) {
                    ForEach(0..<6, id: \.self) { i in
                        RoundedRectangle(cornerRadius: 11, style: .continuous).fill(Tokens.fill)
                            .frame(width: 46, height: 60)
                            .overlay(Text("–").font(.system(size: 34, weight: .semibold, design: .monospaced)))
                            .padding(.leading, i == 3 ? 10 : 0)
                    }
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("Pairing code, not available yet")
                Text("Pairing by comparing a 6-digit code isn't available yet.")
                    .font(.system(size: 11))
            }
            .foregroundStyle(Tokens.l3)
            .opacity(0.6)
            .padding(.top, 4)
            Card {
                Row("Computer", value: trimmed.isEmpty ? "–" : trimmed, dim: true)
                Row("After pairing", value: "Recognised by its key, no code needed", dim: true)
            }
            HStack(spacing: 10) {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Add", action: add)
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(trimmed.isEmpty)
            }
            .padding(.top, 4)
        }
        .padding(.top, 26).padding(.horizontal, 26).padding(.bottom, 20)
        .frame(width: 460)
    }

    private func add() {
        guard !trimmed.isEmpty else { return }
        if let existing = model.store.computers.first(where: { $0.address.caseInsensitiveCompare(trimmed) == .orderedSame }) {
            added(existing.id)
        } else {
            let shown = name.trimmingCharacters(in: .whitespaces)
            var c = SavedComputer(name: shown.isEmpty ? trimmed : shown, address: trimmed)
            c.pairOnConnect = pair
            model.store.add(c)
            added(c.id)
        }
        dismiss()
    }
}

/// "… is paired": shown once, after the first session with a newly added computer.
struct PairedSheet: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    let computer: SavedComputer

    var body: some View {
        VStack(spacing: 12) {
            Circle().fill(Tokens.green).frame(width: 56, height: 56)
                .overlay(Image(systemName: "checkmark").font(.system(size: 26, weight: .bold)).foregroundStyle(.white))
                .accessibilityHidden(true)
            Text("\(computer.name) is paired").font(.system(size: 17, weight: .bold)).tracking(-0.2)
            Text("This Mac and \(computer.name) now recognise each other by key. You won't be asked to pair again.")
                .font(.system(size: 13)).foregroundStyle(Tokens.l2).multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            Card {
                Row("Require Touch ID to connect", caption: "Asks for Touch ID or your Mac password each time.") {
                    Toggle("Require Touch ID to connect", isOn: .constant(false))
                        .labelsHidden().toggleStyle(.switch).available(Features.touchID)
                }
                Row("Key fingerprint", value: Fingerprint.shown(computer.fingerprint), dim: computer.fingerprint.isEmpty, mono: true)
            }
            .padding(.top, 4)
            HStack(spacing: 10) {
                Spacer()
                Button("Done") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Connect") {
                    dismiss()
                    // Let the sheet go before the window changes.
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) { model.connect(computer) }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
            }
            .padding(.top, 6)
        }
        .padding(.top, 28).padding(.horizontal, 26).padding(.bottom, 20)
        .frame(width: 440)
    }
}

// MARK: Rename and forget

extension View {
    func renameAlert(_ item: Binding<SavedComputer?>, store: ComputerStore) -> some View {
        modifier(RenameAlert(item: item, store: store))
    }

    func forgetDialog(_ item: Binding<SavedComputer?>, forget: @escaping (SavedComputer) -> Void) -> some View {
        alert(
            "Forget \(item.wrappedValue?.name ?? "this computer")?",
            isPresented: Binding(get: { item.wrappedValue != nil }, set: { if !$0 { item.wrappedValue = nil } }),
            presenting: item.wrappedValue
        ) { c in
            Button("Forget", role: .destructive) { forget(c) }
            Button("Cancel", role: .cancel) {}
        } message: { _ in
            Text(
                Features.forgetKey
                    ? "Removes its key from this Mac. Pair again to reconnect."
                    : "It is taken off this Mac's list. To connect again, add it again.")
        }
    }
}

private struct RenameAlert: ViewModifier {
    @Binding var item: SavedComputer?
    let store: ComputerStore
    @State private var name = ""

    func body(content: Content) -> some View {
        content
            .alert(
                "Rename computer",
                isPresented: Binding(get: { item != nil }, set: { if !$0 { item = nil } }),
                presenting: item
            ) { c in
                TextField("Name", text: $name)
                Button("Rename") {
                    let n = name.trimmingCharacters(in: .whitespaces)
                    if !n.isEmpty { store.update(c.id) { $0.name = n } }
                }
                Button("Cancel", role: .cancel) {}
            } message: { c in
                Text("The name is only how this Mac lists \(c.address).")
            }
            .onChange(of: item) { _, c in name = c?.name ?? "" }
    }
}
