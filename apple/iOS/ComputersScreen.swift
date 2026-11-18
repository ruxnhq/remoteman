// The root screen: the paired computers (a grid of cards on iPad, a list on iPhone), and the
// computers nearby that aren't paired yet.
import SwiftUI

struct ComputersScreen: View {
    @EnvironmentObject private var model: AppModel
    @EnvironmentObject private var store: ComputerStore
    @Environment(\.horizontalSizeClass) private var sizeClass
    @State private var pairing = false
    @State private var settings = false
    @State private var editing: Computer?
    @State private var renaming: Computer?
    @State private var newName = ""
    @State private var forgetting: Computer?

    private var pad: Bool { sizeClass == .regular }
    private var device: String { UIDevice.current.userInterfaceIdiom == .pad ? "iPad" : "iPhone" }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: pad ? 22 : 14) {
                header
                if store.computers.isEmpty { empty } else if pad { grid } else { list }
                nearby
            }
            .padding(.horizontal, pad ? 40 : 16)
            .padding(.top, pad ? 36 : 12)
            .padding(.bottom, 34)
        }
        .background(Theme.bg.ignoresSafeArea())
        .onAppear(perform: showFromArguments)
        .sheet(isPresented: $pairing) { PairSheet() }
        .sheet(isPresented: $settings) { SettingsScreen() }
        .sheet(item: $editing) { ComputerSettingsScreen(computerID: $0.id) }
        // The computer is handed to the buttons (`presenting`): by the time a button's action
        // runs, the dialog has already cleared `renaming` / `forgetting`, so reading them there
        // found nothing and the button did nothing.
        .alert("Rename", isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } }), presenting: renaming) { c in
            TextField("Name", text: $newName)
            Button("Cancel", role: .cancel) {}
            Button("Rename") {
                let name = newName.trimmingCharacters(in: .whitespaces)
                guard !name.isEmpty else { return }
                var renamed = c
                renamed.name = name
                store.update(renamed)
            }
        }
        .confirmationDialog(
            "Forget \(forgetting?.name ?? "this computer")?", isPresented: Binding(get: { forgetting != nil }, set: { if !$0 { forgetting = nil } }),
            titleVisibility: .visible, presenting: forgetting
        ) { c in
            Button("Forget This Computer", role: .destructive) {
                store.forget(c)
                Haptics.warning()
            }
        } message: { _ in
            Text("Its key is removed from this \(device). Revoke this \(device) on the computer too.")
        }
    }

    /// For checking a screen without tapping: `--show pair`, `--show settings` or `--show computer`.
    private func showFromArguments() {
        let args = CommandLine.arguments
        guard let i = args.firstIndex(of: "--show"), i + 1 < args.count else { return }
        switch args[i + 1] {
        case "pair": pairing = true
        case "settings": settings = true
        case "computer": editing = store.computers.first
        default: break
        }
    }

    // MARK: Header

    private var header: some View {
        HStack(alignment: .center, spacing: 12) {
            if pad { Text("Computers").font(Theme.largeTitle) }
            Spacer()
            if pad {
                Button("Select") {}
                    .font(Theme.body).foregroundStyle(Theme.label).padding(.horizontal, 16).frame(height: 48)
                    .glass().available(Features.multiSelect)
            }
            HStack(spacing: 0) {
                barButton("plus", "Pair a computer") { pairing = true }
                barButton("gearshape", "Settings") { settings = true }
            }
            .padding(.horizontal, 4).frame(height: 48).glass()
        }
        .frame(minHeight: pad ? 56 : 48)
        .overlay(alignment: .bottomLeading) {
            if !pad { Text("Computers").font(Theme.largeTitle).offset(y: 46) }
        }
        .padding(.bottom, pad ? 0 : 46)
    }

    private func barButton(_ icon: String, _ label: String, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: icon).font(.system(size: 20, weight: .regular)).foregroundStyle(Theme.label)
                .frame(width: 44, height: 44)
        }
        .accessibilityLabel(label)
    }

    private var empty: some View {
        VStack(spacing: 14) {
            StateBlock(
                icon: "display", headline: "No computers yet",
                text: "Pair a computer running remoteman to see and use its desktop from this \(device).")
            Button("Pair a Computer") { pairing = true }.buttonStyle(WideButtonStyle()).frame(maxWidth: 320)
        }
        .frame(maxWidth: .infinity).padding(.vertical, 60)
    }

    // MARK: iPad grid

    private var grid: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Paired").font(Theme.title2)
            LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 22), count: 4), spacing: 22) {
                ForEach(store.computers) { c in card(c) }
            }
        }
    }

    private func card(_ c: Computer) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Button { model.connect(c) } label: {
                Thumbnail(computer: c).aspectRatio(4 / 3, contentMode: .fit)
                    .clipShape(RoundedRectangle(cornerRadius: 17, style: .continuous))
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Connect to \(c.name)")
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(c.name).font(Theme.headline).lineLimit(1)
                    Text(c.osLine).font(Theme.subhead).foregroundStyle(Theme.secondary).lineLimit(1)
                    StatusLine(computer: c, short: false)
                }
                Spacer(minLength: 0)
                Menu {
                    menuItems(c)
                } label: {
                    Image(systemName: "ellipsis").font(.system(size: 19, weight: .medium)).foregroundStyle(Theme.label)
                        .frame(width: 44, height: 44)
                }
                .accessibilityLabel("More for \(c.name)")
            }
            .padding(.leading, 10).padding(.trailing, 8)
        }
        .padding(EdgeInsets(top: 10, leading: 10, bottom: 14, trailing: 10))
        .background(Theme.card, in: RoundedRectangle(cornerRadius: Theme.cardRadius, style: .continuous))
    }

    @ViewBuilder
    private func menuItems(_ c: Computer) -> some View {
        Button { model.connect(c) } label: { Label("Connect", systemImage: "display") }
        Button {} label: { Label("Connect in New Window", systemImage: "macwindow.on.rectangle") }
            .disabled(!Features.multiWindow)
        Divider()
        Button { editing = c } label: { Label("Settings for This Computer…", systemImage: "slider.horizontal.3") }
        Button {
            newName = c.name
            renaming = c
        } label: {
            Label("Rename…", systemImage: "pencil")
        }
        Divider()
        Button(role: .destructive) { forgetting = c } label: { Label("Forget This Computer…", systemImage: "trash") }
    }

    // MARK: iPhone list

    private var list: some View {
        InsetGroup {
            ForEach(Array(store.computers.enumerated()), id: \.element.id) { i, c in
                VStack(spacing: 0) {
                    Button { model.connect(c) } label: {
                        HStack(spacing: 12) {
                            Thumbnail(computer: c).frame(width: 76, height: 57)
                                .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
                            VStack(alignment: .leading, spacing: 2) {
                                Text(c.name).font(Theme.headline).foregroundStyle(Theme.label).lineLimit(1)
                                Text(c.osLine).font(Theme.subhead).foregroundStyle(Theme.secondary).lineLimit(1)
                                StatusLine(computer: c, short: true)
                            }
                            Spacer(minLength: 4)
                            Image(systemName: "chevron.right").font(.system(size: 13, weight: .bold)).foregroundStyle(Theme.tertiary)
                        }
                        .padding(.horizontal, 14).padding(.vertical, 10)
                    }
                    .buttonStyle(.plain)
                    .contextMenu { menuItems(c) }
                    if i < store.computers.count - 1 {
                        Rectangle().fill(Theme.separator).frame(height: 0.5).padding(.leading, 102)
                    }
                }
            }
        }
    }

    // MARK: Nearby

    @ViewBuilder
    private var nearby: some View {
        VStack(alignment: .leading, spacing: 12) {
            if pad {
                Text("Nearby").font(Theme.title2).padding(.top, 8)
                Text("Computers running remoteman on this network that aren't paired with this iPad yet.")
                    .font(Theme.subhead).foregroundStyle(Theme.secondary)
            }
            InsetGroup(
                header: pad ? nil : "Nearby, not paired",
                footer: pad
                    ? "To pair, open remoteman on the computer, choose Pair a Device, and scan the code it shows."
                    : "To pair, open remoteman on the computer and scan the code it shows."
            ) {
                Row(title: "Looking for computers isn't available yet", subtitle: "Add one by its address with the + button", last: true) {
                    EmptyView()
                }
            }
        }
        .available(Features.discovery)
    }
}

/// The last picture of a computer, or a plain tile with its kind when there is none yet.
struct Thumbnail: View {
    @EnvironmentObject private var store: ComputerStore
    let computer: Computer
    var body: some View {
        if let image = store.thumbnail(computer) {
            Color.black.overlay(Image(uiImage: image).resizable().scaledToFill()).clipped()
        } else {
            (computer.os == "macos" ? Color(hex: 0x7A4A2B) : Color(hex: 0x27425E))
                .overlay(
                    Image(systemName: computer.os == "macos" ? "laptopcomputer" : "display")
                        .font(.system(size: 28, weight: .light)).foregroundStyle(.white.opacity(0.7)))
        }
    }
}

/// "Online · Used today, 09:12" in the design. Whether a computer is online isn't known before
/// connecting yet, so only when it was last used is shown.
struct StatusLine: View {
    let computer: Computer
    let short: Bool
    var body: some View {
        HStack(spacing: 6) {
            if Features.presence { Circle().fill(Theme.green).frame(width: 9, height: 9) }
            Text(computer.usedLine(short: short) ?? (computer.paired ? "Paired" : "Not connected yet"))
                .font(Theme.subhead).foregroundStyle(Theme.secondary).lineLimit(1)
        }
    }
}
