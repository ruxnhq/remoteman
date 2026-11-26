// What the window and Settings show of updates (Updater.swift): the notice that a newer remoteman
// is out, with Install, and Settings > General > Updates.
import SwiftUI

/// "remoteman X is available" with Install, or how the install goes, or why it can't be. Nothing
/// while there is nothing newer. In the sidebar (`compact`) it can be put away for that version.
struct UpdateNotice: View {
    @EnvironmentObject private var model: AppModel
    @ObservedObject var updater: Updater
    var compact = false

    var body: some View {
        if let newer = shown {
            let (words, button) = say(newer)
            if compact {
                VStack(alignment: .leading, spacing: 6) {
                    Text("remoteman \(newer.description) is available").font(.system(size: 12, weight: .semibold)).foregroundStyle(Tokens.label)
                    Text(words).font(.system(size: 11)).foregroundStyle(Tokens.l2).fixedSize(horizontal: false, vertical: true)
                    HStack(spacing: 8) {
                        if let button { Button(button) { model.installUpdate() }.controlSize(.small) }
                        if !updater.state.busy { Button("Later") { updater.putAwayNotice() }.controlSize(.small) }
                        if updater.state.busy { ProgressView().controlSize(.small) }
                    }
                }
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: 8).fill(Tokens.box))
                .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(Tokens.boxBorder, lineWidth: 1))
                .padding(.horizontal, 10).padding(.bottom, 10)
            } else {
                Row("remoteman \(newer.description) is available", caption: words, strong: true) {
                    if updater.state.busy { ProgressView().controlSize(.small) }
                    if let button { Button(button) { model.installUpdate() } }
                }
            }
        }
    }

    /// The version the notice is about: one that is newer, or whose install failed. In the
    /// sidebar not once it is put away, unless it is being installed or failed.
    private var shown: UpdateVersion? {
        let v = updater.state.newer ?? (updater.installFailed ? updater.offered : nil)
        guard let v else { return nil }
        if compact, updater.putAway == v.description, !updater.state.busy, !updater.installFailed { return nil }
        return v
    }

    private func say(_ newer: UpdateVersion) -> (String, String?) {
        let this = updater.this.description
        switch updater.state {
        case .downloading: return ("Downloading \(newer.description)…", nil)
        case .installing: return ("Installing \(newer.description): remoteman quits and opens again.", nil)
        case .failed(let why): return ("This Mac has \(this). \(UpdateState.failed(why).words(this: updater.this))", "Try Again")
        default:
            if let why = updater.whyCantInstall { return ("This Mac has \(this). \(why)", nil) }
            return ("This Mac has \(this), and installing gets \(newer.description) from remoteman.ruxn.com. remoteman quits and opens again.", "Install")
        }
    }
}

/// Settings > General > Updates.
struct UpdateSettings: View {
    @ObservedObject var updater: Updater
    @AppStorage(Updater.lookKey) private var look = true
    @AppStorage(Updater.installKey) private var install = false

    var body: some View {
        CardGroup("Updates", footnote: "remoteman reads its download page on remoteman.ruxn.com and downloads from files.ruxn.com. A new version is installed only when it is signed by remoteman's developer. Nothing about this Mac is sent.") {
            UpdateNotice(updater: updater)
            Row("Version \(updater.this.description)", caption: caption) {
                Button("Check Now") { updater.check() }.disabled(updater.state.busy)
            }
            Row("Look for updates by itself", caption: "A minute after remoteman opens, then every 12 hours. When there is one, it says so here, in the window and once as a notification.") {
                Toggle("Look for updates by itself", isOn: Binding(get: { look }, set: { look = $0; updater.lookingChanged() }))
                    .labelsHidden().toggleStyle(.switch)
            }
            Row("Install updates by themselves", caption: updater.whyCantInstall ?? "When there is a new version and no session is running. remoteman quits and opens again.") {
                Toggle("Install updates by themselves", isOn: $install).labelsHidden().toggleStyle(.switch)
                    .disabled(!look || updater.whyCantInstall != nil)
            }
        }
    }

    private var caption: String {
        var words = updater.state.words(this: updater.this)
        if case .notYet = updater.state, let from = updater.updatedFrom { words = "Updated from \(from.description) to \(updater.this.description)." }
        if let when = updater.lastLooked {
            words += " Looked " + when.formatted(date: .omitted, time: .shortened) + "."
        }
        if !look { words += " remoteman doesn't look for updates by itself." }
        return words
    }
}
