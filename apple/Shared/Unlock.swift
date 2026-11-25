// A locked computer that asks for its user's password (docs/PROTOCOL.md, "A locked host"): the
// person is asked, the session is started again with the password in the hello, and the
// password can be kept in this device's keychain behind Face ID or Touch ID so that the next
// unlock needs no typing. It is kept nowhere else, and only if the person says so.
import Foundation
import LocalAuthentication
import Security
import SwiftUI

enum UnlockStore {
    private static let service = "com.xnixy.remoteman.unlock"

    private static func base(_ key: String) -> [CFString: Any] {
        [kSecClass: kSecClassGenericPassword, kSecAttrService: service, kSecAttrAccount: key]
    }

    static func has(_ key: String) -> Bool {
        var q = base(key)
        let quiet = LAContext()
        quiet.interactionNotAllowed = true
        q[kSecUseAuthenticationContext] = quiet
        let status = SecItemCopyMatching(q as CFDictionary, nil)
        return status == errSecSuccess || status == errSecInteractionNotAllowed
    }

    @discardableResult
    static func save(_ password: String, for key: String) -> Bool {
        forget(key)
        var q = base(key)
        q[kSecValueData] = Data(password.utf8)
        #if os(iOS)
            // Read back only with Face ID, Touch ID or the passcode, and never off this device.
            if let access = SecAccessControlCreateWithFlags(nil, kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly, .userPresence, nil) {
                q[kSecAttrAccessControl] = access
            }
        #else
            q[kSecAttrAccessible] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        #endif
        return SecItemAdd(q as CFDictionary, nil) == errSecSuccess
    }

    static func forget(_ key: String) { SecItemDelete(base(key) as CFDictionary) }

    /// The kept password, after the person has proved it is them. On the main thread.
    static func read(_ key: String, computer: String, then: @escaping (String?) -> Void) {
        #if os(iOS)
            let context = LAContext()
            context.localizedReason = "unlock \(computer)"
            var q = base(key)
            q[kSecReturnData] = true
            q[kSecUseAuthenticationContext] = context
            DispatchQueue.global(qos: .userInitiated).async {
                var out: CFTypeRef?
                let ok = SecItemCopyMatching(q as CFDictionary, &out) == errSecSuccess
                let text = ok ? (out as? Data).flatMap { String(data: $0, encoding: .utf8) } : nil
                DispatchQueue.main.async { then(text) }
            }
        #else
            // The Mac's keychain gives it to this app without asking; Touch ID is asked here.
            Gate.pass(for: computer) { ok in
                guard ok else { return then(nil) }
                var q = base(key)
                q[kSecReturnData] = true
                var out: CFTypeRef?
                let found = SecItemCopyMatching(q as CFDictionary, &out) == errSecSuccess
                then(found ? (out as? Data).flatMap { String(data: $0, encoding: .utf8) } : nil)
            }
        #endif
    }
}

/// What happens between a computer saying it is locked and the session starting again.
final class Unlocker: ObservableObject {
    struct Ask: Identifiable {
        let id = UUID()
        var key: String
        var name: String
        var words: String
        var canKeep: Bool
    }

    @Published var asking: Ask?
    private var retry: ((String) -> Void)?
    private var triedKept = false
    private var toKeep: (key: String, password: String)?

    /// The computer said it is locked. `key` names it in the keychain (its key's fingerprint);
    /// `retry` connects again with a password.
    func locked(words: String, key: String, name: String, retry: @escaping (String) -> Void) {
        self.retry = retry
        toKeep = nil
        let wrong = words.lowercased().contains("not right")
        if triedKept, wrong { UnlockStore.forget(key) }
        if !triedKept, !wrong, UnlockStore.has(key) {
            UnlockStore.read(key, computer: name) { [weak self] password in
                guard let self else { return }
                // Only a kept password that was sent can be the one that turns out wrong.
                if let password {
                    self.triedKept = true
                    retry(password)
                } else {
                    self.ask(key, name, words)
                }
            }
            return
        }
        ask(key, name, words)
    }

    private func ask(_ key: String, _ name: String, _ words: String) {
        var can = true
        #if os(iOS)
            can = LAContext().canEvaluatePolicy(.deviceOwnerAuthentication, error: nil)
        #endif
        asking = Ask(key: key, name: name, words: words, canKeep: can)
    }

    func submit(_ password: String, keep: Bool) {
        guard let a = asking else { return }
        asking = nil
        toKeep = keep ? (a.key, password) : nil
        if !keep { UnlockStore.forget(a.key) }
        retry?(password)
    }

    func cancel() {
        asking = nil
        retry = nil
        triedKept = false
        toKeep = nil
    }

    /// The session started: a password that worked is kept, if that was asked for.
    func connected() {
        if let k = toKeep { UnlockStore.save(k.password, for: k.key) }
        toKeep = nil
        triedKept = false
        retry = nil
    }
}

/// The sheet that asks for the computer's password.
struct UnlockSheet: View {
    let ask: Unlocker.Ask
    let unlocker: Unlocker
    @State private var password = ""
    @State private var keep = true
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Label("\(ask.name) is locked", systemImage: "lock.fill").font(.headline)
            Text(ask.words).font(.subheadline).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            SecureField("Password", text: $password)
                .textFieldStyle(.roundedBorder).focused($focused).onSubmit(send)
                .textContentType(.password)
            // The buttons sit under the field, where a phone's keyboard leaves them in view.
            HStack(spacing: 12) {
                Spacer()
                Button("Cancel", role: .cancel) { unlocker.cancel() }.keyboardShortcut(.cancelAction)
                Button("Unlock", action: send).keyboardShortcut(.defaultAction).buttonStyle(.borderedProminent).disabled(password.isEmpty)
            }
            if ask.canKeep {
                Toggle("Keep it on this device, behind \(Gate.name)", isOn: $keep)
                Text("It goes only to \(ask.name), inside the encrypted connection. Kept, the next unlock asks for \(Gate.name) instead of typing.")
                    .font(.footnote).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(22)
        .frame(minWidth: 320, idealWidth: 420)
        .onAppear { focused = true }
    }

    private func send() {
        guard !password.isEmpty else { return }
        unlocker.submit(password, keep: keep && ask.canKeep)
    }
}
