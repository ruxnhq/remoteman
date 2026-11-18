// Pairing a computer, and the screen shown while a connection is being made.
import SwiftUI

/// "Pair a Computer". The design pairs by scanning the computer's QR code; that isn't built
/// yet, so the camera area is drawn as designed but inert, and a computer is added by its
/// address instead: the first connection pairs, while the computer is in pairing mode.
struct PairSheet: View {
    @EnvironmentObject private var model: AppModel
    @EnvironmentObject private var store: ComputerStore
    @Environment(\.dismiss) private var dismiss
    @State private var address = ""
    @State private var name = ""
    private var device: String { UIDevice.current.userInterfaceIdiom == .pad ? "iPad" : "iPhone" }

    private var trimmed: String { address.trimmingCharacters(in: .whitespaces) }

    var body: some View {
        VStack(spacing: 22) {
            ZStack {
                Text("Pair a Computer").font(Theme.headline)
                HStack {
                    CircleButton(icon: "xmark", label: "Close") { dismiss() }
                    Spacer()
                }
            }
            .padding(.top, 14)

            ZStack {
                RoundedRectangle(cornerRadius: 28, style: .continuous).fill(Color(hex: 0x17181B))
                VStack(spacing: 10) {
                    Image(systemName: "qrcode.viewfinder").font(.system(size: 56, weight: .ultraLight)).foregroundStyle(.white.opacity(0.6))
                    Text("Scanning the computer's code isn't available yet").font(Theme.subhead).foregroundStyle(.white.opacity(0.7))
                }
            }
            .frame(maxWidth: .infinity).frame(height: 190)
            .available(Features.qrPairing)

            VStack(spacing: 8) {
                Text("Add the computer by its address").font(Theme.title2).multilineTextAlignment(.center)
                Text("On the computer, start remoteman with pairing on. The first connection from this \(device) pairs the two, and no code is needed after that.")
                    .font(Theme.body).foregroundStyle(Theme.secondary).multilineTextAlignment(.center)
            }

            InsetGroup(footer: "The address is the computer's name or number on your network, for example blackbox or 10.0.0.76. Add :port if it isn't 7791.") {
                Row(title: "Address") {
                    TextField("blackbox or 10.0.0.76", text: $address)
                        .multilineTextAlignment(.trailing).textInputAutocapitalization(.never).autocorrectionDisabled()
                        .keyboardType(.URL).submitLabel(.go).onSubmit(add)
                }
                Row(title: "Name", last: true) {
                    TextField("Optional", text: $name).multilineTextAlignment(.trailing).submitLabel(.go).onSubmit(add)
                }
            }

            Spacer(minLength: 0)

            VStack(spacing: 10) {
                Button("Pair and Connect", action: add).buttonStyle(WideButtonStyle()).disabled(trimmed.isEmpty)
                    .opacity(trimmed.isEmpty ? 0.5 : 1)
                Button("Compare a 6-Digit Code Instead") {}.buttonStyle(WideButtonStyle(kind: .secondary))
                    .available(Features.codePairing)
            }
        }
        .padding(.horizontal, 28).padding(.bottom, 24)
        .background(Theme.bg.ignoresSafeArea())
        .presentationDetents([.large])
        .presentationCornerRadius(40)
    }

    private func add() {
        guard !trimmed.isEmpty else { return }
        let c = store.add(name: name.trimmingCharacters(in: .whitespaces), address: trimmed)
        dismiss()
        // Let the sheet go before the connecting screen takes over.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.35) { model.connect(c) }
    }
}

/// "Connecting to …": the steps of getting in, over a dark ground, or why it didn't work.
struct GettingInScreen: View {
    @EnvironmentObject private var model: AppModel
    @EnvironmentObject private var session: ViewerSession
    @Environment(\.horizontalSizeClass) private var sizeClass
    let failure: String?

    private var name: String { model.current?.name ?? "the computer" }
    private var device: String { UIDevice.current.userInterfaceIdiom == .pad ? "iPad" : "iPhone" }

    var body: some View {
        ZStack {
            LinearGradient(colors: [Color(hex: 0x1B2430), Color(hex: 0x0B0D12)], startPoint: .top, endPoint: .bottom)
                .ignoresSafeArea()
            VStack(spacing: 26) {
                if let failure { failed(failure) } else { connecting }
            }
            .padding(sizeClass == .regular ? 36 : 20)
            .frame(maxWidth: 580)
            .background {
                if sizeClass == .regular {
                    RoundedRectangle(cornerRadius: 40, style: .continuous).fill(.white.opacity(0.06))
                        .overlay(RoundedRectangle(cornerRadius: 40, style: .continuous).strokeBorder(.white.opacity(0.14), lineWidth: 1))
                }
            }
            .padding(20)
        }
        .overlay(alignment: .topLeading) {
            CircleButton(icon: "chevron.left", label: "Cancel and go back") { model.cancelConnecting() }
                .padding(.leading, 28).padding(.top, 56)
        }
        .environment(\.colorScheme, .dark)
    }

    @ViewBuilder
    private var connecting: some View {
        StateBlock(
            icon: "display", iconColor: .white, circle: .white.opacity(0.12), headline: "Connecting to \(name)",
            text: model.current?.paired == false ? "Pairing this \(device) with the computer." : "Reaching your session.")
        VStack(alignment: .leading, spacing: 14) {
            step(session.stage >= 1 ? .done : .current, "Reaching \(name)", "At \(model.current?.address ?? "its address").")
            step(
                session.stage >= 2 ? .done : session.stage == 1 ? .current : .pending, "Recognising it by its key",
                model.current?.paired == false ? "This first connection pairs the two." : nil)
            step(session.stage >= 2 ? .current : .pending, placementTitle, nil)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        Button("Cancel") { model.cancelConnecting() }.buttonStyle(WideButtonStyle(kind: .secondary))
    }

    private var placementTitle: String {
        switch model.current?.placementChoice ?? .extend {
        case .extend: return "This \(device) joins as an extra display"
        case .main: return "This \(device) becomes the main display"
        case .only: return "This \(device) becomes the only display"
        }
    }

    @ViewBuilder
    private func failed(_ reason: String) -> some View {
        StateBlock(
            icon: "wifi.slash", iconColor: .white.opacity(0.8), circle: .white.opacity(0.12), headline: "Can't reach \(name)",
            text: reason)
        VStack(spacing: 10) {
            Button("Try Again") { if let c = model.current { model.connect(c) } }.buttonStyle(WideButtonStyle())
            Button("Back to Computers") { model.cancelConnecting() }.buttonStyle(WideButtonStyle(kind: .secondary))
        }
    }

    private enum StepState { case done, current, pending }

    private func step(_ state: StepState, _ title: String, _ sub: String?) -> some View {
        HStack(alignment: .top, spacing: 12) {
            Group {
                switch state {
                case .done: Image(systemName: "checkmark.circle.fill").foregroundStyle(Color(hex: 0x45D166))
                case .current: ProgressView().controlSize(.small)
                case .pending: Image(systemName: "circle").foregroundStyle(.white.opacity(0.4))
                }
            }
            .font(.system(size: 20)).frame(width: 24, height: 24)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(Theme.headline).foregroundStyle(state == .pending ? .white.opacity(0.6) : .white)
                if let sub { Text(sub).font(Theme.subhead).foregroundStyle(.white.opacity(0.6)) }
            }
        }
    }
}
