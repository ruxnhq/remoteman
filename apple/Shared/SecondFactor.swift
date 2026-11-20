// The second factor (docs/PROTOCOL.md, "Second factor"): a computer that wants a code from an
// authenticator app sends nothing more until it has one. This is the sheet that asks for it.
import SwiftUI

struct SecondFactorSheet: View {
    let ask: ViewerSession.SecondFactorAsk
    let session: ViewerSession
    @State private var code = ""
    @State private var remember = false
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Label("Enter the code from your authenticator app", systemImage: "lock.shield.fill").font(.headline)
            if ask.left >= 0 {
                Text("That code didn't work (\(ask.left) \(ask.left == 1 ? "try" : "tries") left)")
                    .font(.subheadline).foregroundStyle(.red)
            }
            TextField("6-digit code", text: $code)
                .textFieldStyle(.roundedBorder).focused($focused).onSubmit(send)
                .font(.system(.title3, design: .monospaced))
                #if os(iOS)
                    .keyboardType(.numberPad).textContentType(.oneTimeCode)
                #endif
                .onChange(of: code) { _, new in
                    let digits = String(new.filter(\.isNumber).prefix(6))
                    if digits != new { code = digits }
                }
            HStack(spacing: 12) {
                Spacer()
                Button("Cancel", role: .cancel) { session.disconnect() }.keyboardShortcut(.cancelAction)
                Button("Continue", action: send).keyboardShortcut(.defaultAction).buttonStyle(.borderedProminent)
                    .disabled(code.count != 6)
            }
            Toggle("Don't ask again on this device for 30 days", isOn: $remember)
        }
        .padding(22)
        .frame(minWidth: 320, idealWidth: 400)
        .onAppear { focused = true }
    }

    private func send() {
        guard code.count == 6 else { return }
        session.answerSecondFactor(code: code, remember: remember)
    }
}

/// The sheet over whatever the window shows. Reading a wrong code again gives a new ask, so the
/// sheet is rebuilt (and its field emptied) each time.
struct SecondFactorPrompt: ViewModifier {
    @ObservedObject var session: ViewerSession
    func body(content: Content) -> some View {
        #if os(iOS)
            content.sheet(item: $session.secondFactor) { ask in
                ScrollView { SecondFactorSheet(ask: ask, session: session) }
                    .presentationDetents([.medium, .large]).interactiveDismissDisabled()
            }
        #else
            content.sheet(item: $session.secondFactor) { SecondFactorSheet(ask: $0, session: session) }
        #endif
    }
}
