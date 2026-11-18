// The host's interface: the menu-bar panel, the Settings window, "Pair a Device" and the first
// run. What the host can't do yet is drawn as designed and disabled (Features.swift).
import SwiftUI

private enum HostWording {
    static func place(_ p: Int) -> String { ["Extra display", "Main display", "Only display"][min(max(p, 0), 2)] }

    /// "42 min", "1 h 5 min", "under a minute".
    static func length(_ seconds: TimeInterval) -> String {
        let minutes = Int(seconds / 60)
        if minutes < 1 { return "under a minute" }
        if minutes < 60 { return "\(minutes) min" }
        return minutes % 60 == 0 ? "\(minutes / 60) h" : "\(minutes / 60) h \(minutes % 60) min"
    }

    /// The symbol for a device, as far as its name tells.
    static func symbol(_ name: String) -> String {
        let n = name.lowercased()
        if n.contains("ipad") { return "ipad" }
        if n.contains("iphone") { return "iphone" }
        if n.contains("macbook") { return "laptopcomputer" }
        return "desktopcomputer"
    }

    static func countdown(_ seconds: TimeInterval) -> String {
        let s = max(0, Int(seconds.rounded(.up)))
        return "\(s / 60):" + String(format: "%02d", s % 60)
    }
}

// MARK: The menu-bar panel

struct HostPanel: View {
    @ObservedObject var controller: HostController
    var pair: () -> Void
    var settings: () -> Void
    var quit: () -> Void
    var close: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 1) {
                    Text("remoteman").font(.system(size: 15, weight: .bold))
                    Text(controller.sharing ? "Sharing \(controller.name)" : "Not sharing \(controller.name)")
                        .font(.system(size: 11)).foregroundStyle(Tokens.l2)
                }
                Spacer()
                Toggle("Share this Mac", isOn: Binding(get: { controller.sharing }, set: { controller.setSharing($0) }))
                    .labelsHidden().toggleStyle(.switch)
            }
            .padding(.horizontal, 4)

            if let c = controller.client {
                connected(c)
            } else {
                status
                devices
            }

            Rectangle().fill(Tokens.sep).frame(height: 1).padding(.horizontal, 5)
            VStack(spacing: 0) {
                PanelItem(symbol: "qrcode", title: "Pair a Device…") {
                    close()
                    pair()
                }
                .disabled(!controller.sharing)
                PanelItem(
                    symbol: controller.sharing ? "pause" : "play",
                    title: controller.sharing ? "Pause Sharing" : "Resume Sharing",
                    hint: controller.sharing ? "Until I turn it on" : ""
                ) {
                    controller.setSharing(!controller.sharing)
                }
                Rectangle().fill(Tokens.sep).frame(height: 1).padding(.vertical, 5).padding(.horizontal, 10)
                PanelItem(symbol: "gearshape", title: "Settings…", hint: "⌘,") {
                    close()
                    settings()
                }
                .keyboardShortcut(",", modifiers: .command)
                PanelItem(symbol: "power", title: "Quit remoteman", hint: "⌘Q", action: quit)
                    .keyboardShortcut("q", modifiers: .command)
            }
        }
        .padding(12)
        .frame(width: 340)
    }

    /// Nobody is connected: ready, paused, or unable to share.
    private var status: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                if let error = controller.startError {
                    StatusDot(color: Tokens.orange)
                    Text("Can't share").fontWeight(.semibold)
                    Text(error).foregroundStyle(Tokens.l2).lineLimit(2)
                } else if controller.sharing {
                    StatusDot(color: Tokens.green)
                    Text("Ready").fontWeight(.semibold)
                    Text("No device is connected").foregroundStyle(Tokens.l2)
                } else {
                    StatusDot(color: Tokens.l3)
                    Text("Paused").fontWeight(.semibold)
                    Text("No device can connect").foregroundStyle(Tokens.l2)
                }
            }
            .font(.system(size: 13))
            if controller.sharing {
                Text(
                    controller.screenRecording
                        ? "Paired devices can connect while you are logged in."
                        : "Screen Recording isn't allowed yet, so devices are refused. See Settings."
                )
                .font(.system(size: 11)).foregroundStyle(Tokens.l2).fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 10).fill(Tokens.box))
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(Tokens.boxBorder, lineWidth: 1))
    }

    /// "Paired devices": the devices this Mac trusts. Revoking is in Settings.
    private var devices: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Paired devices").font(.system(size: 11, weight: .bold)).foregroundStyle(Tokens.l2)
            if controller.devices.isEmpty {
                Text(controller.sharing ? "No device is paired yet." : "Shown while this Mac is shared.")
                    .font(.system(size: 11)).foregroundStyle(Tokens.l3)
            }
            ForEach(controller.devices.prefix(6)) { d in
                HStack(spacing: 6) {
                    Image(systemName: HostWording.symbol(d.name)).foregroundStyle(Tokens.l2).frame(width: 16)
                    Text(d.name).font(.system(size: 12)).lineLimit(1)
                    Spacer()
                }
            }
            if controller.devices.count > 6 {
                Text("and \(controller.devices.count - 6) more, in Settings").font(.system(size: 11)).foregroundStyle(Tokens.l3)
            }
        }
        .padding(.horizontal, 10)
    }

    private func connected(_ c: MacHost.Client) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                GlyphTile(symbol: HostWording.symbol(c.name))
                VStack(alignment: .leading, spacing: 2) {
                    Text(c.name).font(.system(size: 13, weight: .semibold)).lineLimit(1)
                    HStack(spacing: 5) {
                        StatusDot(color: Tokens.green)
                        Text("Connected").fontWeight(.semibold).foregroundStyle(Tokens.green)
                        Text("for \(HostWording.length(controller.now.timeIntervalSince(c.since)))").foregroundStyle(Tokens.l2)
                    }
                    .font(.system(size: 11))
                }
                Spacer()
                Button("Disconnect") { controller.disconnectClient() }.foregroundStyle(Tokens.red)
            }
            Grid(alignment: .leading, horizontalSpacing: 12, verticalSpacing: 4) {
                fact("Display", "\(HostWording.place(c.placement)), \(c.width) × \(c.height) at \(c.refresh) Hz")
                fact(
                    "Picture",
                    controller.bitsPerSecond > 0
                        ? "HEVC 10-bit, \(max(1, controller.bitsPerSecond / 1_000_000)) Mb/s" : "HEVC 10-bit")
                fact("Sound", "Not available yet", available: Features.sound)
                fact("Microphone", "Not available yet", available: Features.deviceCameraAndMicrophone)
                fact("Camera", "Not available yet", available: Features.deviceCameraAndMicrophone)
            }
            .font(.system(size: 11))
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 10).fill(Tokens.box))
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(Tokens.boxBorder, lineWidth: 1))
    }

    private func fact(_ key: String, _ value: String, available: Bool = true) -> some View {
        GridRow {
            Text(key).foregroundStyle(Tokens.l2)
            Text(value).foregroundStyle(available ? Tokens.label : Tokens.l3).opacity(available ? 1 : 0.7)
        }
    }
}

/// A menu line in the panel: icon, title, a hint at the right; highlighted under the pointer.
private struct PanelItem: View {
    var symbol: String
    var title: String
    var hint = ""
    var action: () -> Void
    @State private var hover = false
    @Environment(\.isEnabled) private var enabled

    var body: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                Image(systemName: symbol).frame(width: 16)
                Text(title)
                Spacer()
                Text(hint).foregroundStyle(hover && enabled ? Color.white.opacity(0.85) : Tokens.l2)
            }
            .font(.system(size: 13))
            .foregroundStyle(hover && enabled ? Color.white : Tokens.label)
            .padding(.horizontal, 10)
            .frame(height: 24)
            .background(RoundedRectangle(cornerRadius: 7).fill(hover && enabled ? Tokens.tint : .clear))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .opacity(enabled ? 1 : 0.4)
        .onHover { hover = $0 }
    }
}

// MARK: Settings

struct HostSettings: View {
    @ObservedObject var controller: HostController
    var pair: () -> Void
    @State private var revoking: MacHost.Device?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                CardGroup {
                    Row(
                        title: "Share this Mac",
                        caption: "Paired devices can see and control this Mac, each with a display of its own size. Shown to them as \(controller.name).",
                        strong: true,
                        leading: { AppIconTile(size: 40) },
                        trailing: {
                            Toggle("Share this Mac", isOn: Binding(get: { controller.sharing }, set: { controller.setSharing($0) }))
                                .labelsHidden().toggleStyle(.switch)
                        })
                    if let error = controller.startError {
                        Row("Sharing couldn't start", caption: error) { EmptyView() }
                    }
                    Row("Open remoteman at login") { fixedSwitch("Open remoteman at login", false, Features.openAtLogin) }
                }
                CardGroup("Address and key") {
                    Row(
                        "Address",
                        caption: controller.addresses.isEmpty
                            ? "What to enter on a device to reach this Mac."
                            : "What to enter on a device to reach this Mac. Also: \(controller.addresses.joined(separator: ", ")).",
                        value: controller.sharing ? controller.addressToEnter : "Not sharing", dim: !controller.sharing)
                    Row(
                        "Key fingerprint", caption: "A device shows the same fingerprint once it is paired.",
                        value: controller.fingerprint.isEmpty ? "Shown while sharing" : Fingerprint.shown(controller.fingerprint),
                        dim: controller.fingerprint.isEmpty, mono: true)
                }
                CardGroup("Getting in") {
                    Row(
                        "Devices join the session you are logged in to",
                        caption: "This Mac has to be logged in. If it is locked, the device shows the lock screen and unlocks it from there.",
                        value: "Attach")
                }
                CardGroup(
                    "Where a device's display sits",
                    footnote: "Extra display: beside this computer's own screens. Main display: the device becomes the main one and the others stay on. Only display: this computer's own screens are switched off while the device is connected, so nobody in the room sees the desktop."
                ) {
                    Row("Default for a new device", caption: "A device can pick any other allowed place, also during a session.") {
                        Picker("Default for a new device", selection: .constant(0)) { Text("Extra Display").tag(0) }
                            .labelsHidden().fixedSize().available(Features.placementPolicy)
                    }
                    Row("Devices may choose") {
                        HStack(spacing: 14) {
                            Toggle("Extra display", isOn: .constant(true))
                            Toggle("Main display", isOn: .constant(true))
                            Toggle("Only display", isOn: .constant(true))
                        }
                        .toggleStyle(.checkbox).available(Features.placementPolicy)
                    }
                }
                CardGroup(
                    "What paired devices may do",
                    footnote: "Both are off until you turn them on. A connected device is told at once."
                ) {
                    Row("Share the clipboard", caption: "What is copied on this Mac or on the device can be pasted on the other.") {
                        Toggle("Share the clipboard", isOn: Binding(get: { controller.allowClipboard }, set: { controller.setAllowed(clipboard: $0) }))
                            .labelsHidden().toggleStyle(.switch)
                    }
                    Row("Receive files", caption: "Files a device sends are saved in Downloads.") {
                        Toggle("Receive files", isOn: Binding(get: { controller.allowFiles }, set: { controller.setAllowed(files: $0) }))
                            .labelsHidden().toggleStyle(.switch)
                    }
                    Row(
                        "Use a device's microphone on this Mac",
                        caption: controller.microphoneNote.isEmpty
                            ? (controller.microphoneInstalled
                                ? "\"remoteman Microphone\" is installed. Apps on this Mac can pick it while a device has its microphone on."
                                : "Installs \"remoteman Microphone\", a sound driver. macOS asks for your password, and sound on this Mac stops for a moment.")
                            : controller.microphoneNote
                    ) {
                        Button(controller.microphoneInstalled ? "Remove…" : "Install…") { controller.installMicrophone() }
                    }
                    Row(
                        "Use a device's camera on this Mac",
                        caption: controller.cameraNote.isEmpty
                            ? (controller.cameraInstalled
                                ? "\"remoteman Camera\" is installed. Apps on this Mac can pick it while a device has its camera on."
                                : "Installs \"remoteman Camera\". macOS asks you to allow it once. remoteman has to be in the Applications folder.")
                            : controller.cameraNote
                    ) {
                        Button(controller.cameraInstalled ? "Remove…" : "Install…") { controller.installCamera() }
                    }
                }
                CardGroup("While a device is connected") {
                    Row("Lock this Mac when the device leaves") {
                        fixedSwitch("Lock this Mac when the device leaves", false, Features.lockOnLeave)
                    }
                    Row("Notify me when a device connects") {
                        fixedSwitch("Notify me when a device connects", false, Features.notifyOnConnect)
                    }
                    Row(
                        "Use the device's camera and microphone on this Mac",
                        caption: "They appear here as a camera and a microphone, for calls made on this Mac. The device decides when they are on."
                    ) {
                        fixedSwitch("Use the device's camera and microphone on this Mac", false, Features.deviceCameraAndMicrophone)
                    }
                }
                CardGroup("Paired devices") {
                    ForEach(controller.devices) { d in
                        Row(
                            title: d.name, caption: d.connected ? "Connected now" : "Key \(d.fingerprint.prefix(8).uppercased())", strong: d.connected,
                            leading: { Image(systemName: HostWording.symbol(d.name)).foregroundStyle(Tokens.tintfg).frame(width: 20) },
                            trailing: { Button("Revoke…") { revoking = d }.foregroundStyle(Tokens.red) })
                    }
                    if controller.devices.isEmpty {
                        Row(
                            controller.sharing ? "No device is paired yet" : "Paired devices are shown while this Mac is shared",
                            caption: nil
                        ) { EmptyView() }
                        .opacity(0.7)
                    }
                    Row(title: "", caption: "A revoked device has to be paired again before it can connect.", leading: { EmptyView() }) {
                        Button("Pair a Device…", action: pair).disabled(!controller.sharing)
                    }
                }
                CardGroup("Picture") {
                    Row("Bitrate limit") {
                        Slider(value: .constant(0.63)).frame(width: 200).available(Features.bitrateLimit)
                        Text("Automatic").foregroundStyle(Tokens.l2)
                    }
                    Row("Encoder", caption: "Every device receives HEVC from this Mac.", value: "HEVC, hardware (VideoToolbox)")
                    Row("Allow sharp text (4:4:4)", caption: "Full colour detail for small text, at a higher bitrate.") {
                        fixedSwitch("Allow sharp text (4:4:4)", false, Features.sharpText)
                    }
                    Row("10-bit colour") { fixedSwitch("10-bit colour", true, Features.tenBitChoice) }
                }
                CardGroup("Permissions") {
                    Row("Screen Recording") {
                        PermissionStatus(allowed: controller.screenRecording, open: controller.openScreenRecordingSettings)
                    }
                    Row("Accessibility") {
                        PermissionStatus(allowed: controller.accessibility, open: controller.openAccessibilitySettings)
                    }
                }
            }
            .padding(.vertical, 20).padding(.horizontal, 28)
        }
        .frame(width: 760)
        .background(Tokens.win)
        .confirmationDialog(
            "Revoke \(revoking?.name ?? "this device")?", isPresented: Binding(get: { revoking != nil }, set: { if !$0 { revoking = nil } }),
            titleVisibility: .visible, presenting: revoking
        ) { d in
            // The device is handed in: `revoking` is already cleared when the action runs.
            Button("Revoke", role: .destructive) { controller.revoke(d) }
            Button("Cancel", role: .cancel) {}
        } message: { d in
            Text(d.connected ? "It is disconnected now, and has to be paired again before it can connect." : "It has to be paired again before it can connect.")
        }
    }

    private func fixedSwitch(_ title: String, _ value: Bool, _ flag: Bool) -> some View {
        Toggle(title, isOn: .constant(value)).labelsHidden().toggleStyle(.switch).available(flag)
    }
}

/// "Allowed" with a check, or the way to System Settings.
private struct PermissionStatus: View {
    var allowed: Bool
    var open: () -> Void
    var body: some View {
        if allowed {
            Label("Allowed", systemImage: "checkmark")
                .font(.system(size: 13, weight: .semibold)).foregroundStyle(Tokens.green)
        } else {
            Button("Open System Settings…", action: open)
        }
    }
}

// MARK: Pair a Device

struct PairDevice: View {
    @ObservedObject var controller: HostController
    var close: () -> Void

    var body: some View {
        VStack(spacing: 14) {
            Text("Scan with remoteman on your iPad or iPhone").font(.system(size: 17, weight: .bold)).tracking(-0.2)
                .multilineTextAlignment(.center)
            Text("In the app, tap the plus button and point the camera at this code.")
                .font(.system(size: 13)).foregroundStyle(Tokens.l2).multilineTextAlignment(.center)

            // The QR code isn't there yet: its tile is, empty.
            RoundedRectangle(cornerRadius: 16, style: .continuous).fill(Color.white)
                .frame(width: 232, height: 232)
                .overlay {
                    VStack(spacing: 8) {
                        Image(systemName: "qrcode").font(.system(size: 72, weight: .light))
                        Text("Pairing with a QR code isn't available yet").font(.system(size: 11)).multilineTextAlignment(.center)
                    }
                    .foregroundStyle(Color.black.opacity(0.3))
                    .padding(16)
                }
                .opacity(Features.qrPairing ? 1 : 0.6)

            Card {
                VStack(spacing: 8) {
                    Text("For now, on the other device add this Mac by its address and choose to pair on the first connection:")
                        .font(.system(size: 13)).foregroundStyle(Tokens.l2).multilineTextAlignment(.center)
                        .fixedSize(horizontal: false, vertical: true)
                    Text(controller.addressToEnter).font(.system(size: 17, weight: .semibold, design: .monospaced))
                        .textSelection(.enabled)
                    if !controller.addresses.isEmpty {
                        Text("or \(controller.addresses.map { controller.port == "7791" ? $0 : "\($0):\(controller.port)" }.joined(separator: ", "))")
                            .font(.system(size: 11, design: .monospaced)).foregroundStyle(Tokens.l2).textSelection(.enabled)
                    }
                    Text("– – –  – – –").font(.system(size: 30, weight: .semibold, design: .monospaced)).tracking(1.8)
                        .foregroundStyle(Tokens.l3).opacity(Features.codePairing ? 1 : 0.4)
                        .accessibilityLabel("Pairing code, not available yet")
                    Text("Comparing a 6-digit code isn't available yet.").font(.system(size: 11)).foregroundStyle(Tokens.l3)
                }
                .padding(14)
                .frame(maxWidth: .infinity)
            }

            progress
            Spacer(minLength: 0)
            HStack {
                Spacer()
                Button(controller.pairing ? "Cancel" : "Close", action: close).keyboardShortcut(.cancelAction)
            }
        }
        .padding(.top, 20).padding(.horizontal, 28).padding(.bottom, 20)
        .frame(width: 520, height: 700)
        .background(Tokens.win)
    }

    @ViewBuilder private var progress: some View {
        if controller.pairing {
            let left = controller.pairingEnds.map { $0.timeIntervalSince(controller.now) }
            VStack(spacing: 6) {
                ProgressView(value: left.map { max(0, min(1, $0 / HostController.pairingSeconds)) } ?? 1)
                    .progressViewStyle(.linear)
                Text(
                    left.map { "Pairing closes in \(HostWording.countdown($0)). It accepts one new device, then closes." }
                        ?? "Pairing is open. It accepts one new device, then closes.")
                    .font(.system(size: 11)).foregroundStyle(Tokens.l2)
            }
        } else if controller.pairedJustNow {
            Label("A device was paired. Pairing has closed.", systemImage: "checkmark.circle.fill")
                .font(.system(size: 13, weight: .semibold)).foregroundStyle(Tokens.green)
        } else if !controller.sharing {
            Text("This Mac isn't being shared, so no device can pair. Turn on Share this Mac first.")
                .font(.system(size: 11)).foregroundStyle(Tokens.l2).multilineTextAlignment(.center)
        } else {
            VStack(spacing: 6) {
                Text("Pairing has closed.").font(.system(size: 11)).foregroundStyle(Tokens.l2)
                Button("Open Pairing Again") { controller.beginPairing() }
            }
        }
    }
}

// MARK: First run

struct FirstRun: View {
    @ObservedObject var controller: HostController
    var close: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            Spacer(minLength: 52)
            VStack(spacing: 14) {
                AppIconTile(size: 64)
                Text("Let your devices use this Mac").font(.system(size: 24, weight: .bold))
                Text("macOS asks before an app may show the screen to another device or control the Mac. remoteman needs both before your iPad, iPhone or another computer can connect.")
                    .font(.system(size: 14)).lineSpacing(4).foregroundStyle(Tokens.l2)
                    .multilineTextAlignment(.center).frame(maxWidth: 540)
                Card {
                    Row(
                        title: "Screen Recording", caption: "To send this Mac's picture to your device.", strong: true,
                        leading: { GlyphTile(symbol: "record.circle", color: Tokens.fixed(0xC93A5B)) },
                        trailing: { PermissionStatus(allowed: controller.screenRecording, open: controller.openScreenRecordingSettings) })
                    Row(
                        title: "Accessibility", caption: "To pass on the keyboard, pointer and touch from your device.", strong: true,
                        leading: { GlyphTile(symbol: "accessibility", color: Tokens.tint) },
                        trailing: { PermissionStatus(allowed: controller.accessibility, open: controller.openAccessibilitySettings) })
                }
                .frame(maxWidth: 560)
                Text("In System Settings, turn on remoteman-host under Privacy & Security, then come back here. You can take either permission away again at any time.")
                    .font(.system(size: 11)).foregroundStyle(Tokens.l2)
                    .multilineTextAlignment(.center).frame(maxWidth: 540)
            }
            Spacer()
            HStack {
                Button("Not Now", action: close)
                Spacer()
                Button("Share This Mac") {
                    controller.setSharing(true)
                    close()
                }
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(!(controller.screenRecording && controller.accessibility))
            }
        }
        .padding(.horizontal, 28).padding(.bottom, 20)
        .frame(width: 700, height: 640)
        .background(Tokens.win)
        .ignoresSafeArea()
    }
}
