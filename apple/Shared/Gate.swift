// Touch ID or Face ID (or the device's passcode or password) before connecting, for those who
// turn it on. Nothing is asked of the system until then.
import Foundation
import LocalAuthentication

enum Gate {
    /// What this device calls it: "Face ID", "Touch ID", or "your passcode".
    /// Asked of the system once: screens name it in every row they draw.
    static let name: String = {
        let c = LAContext()
        _ = c.canEvaluatePolicy(.deviceOwnerAuthentication, error: nil)
        switch c.biometryType {
        case .faceID: return "Face ID"
        case .touchID: return "Touch ID"
        case .opticID: return "Optic ID"
        default:
            #if os(macOS)
                return "your password"
            #else
                return "your passcode"
            #endif
        }
    }()

    /// Calls `then` on the main thread with whether the person proved it is them. A device with
    /// neither biometrics nor a passcode lets everyone through: there is nothing to ask.
    static func pass(for computer: String, then: @escaping (Bool) -> Void) {
        let c = LAContext()
        var error: NSError?
        guard c.canEvaluatePolicy(.deviceOwnerAuthentication, error: &error) else {
            then(true)
            return
        }
        c.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: "connect to \(computer)") { ok, _ in
            DispatchQueue.main.async { then(ok) }
        }
    }
}
