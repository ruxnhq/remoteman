// The look of the iPad and iPhone app, from the approved design: colours for light and dark,
// type roles, radii, and the few shapes used everywhere (glass capsules, inset groups, rows).
import SwiftUI
import UIKit

extension Color {
    init(light: UInt32, dark: UInt32, lightAlpha: Double = 1, darkAlpha: Double = 1) {
        self.init(
            UIColor { traits in
                let hex = traits.userInterfaceStyle == .dark ? dark : light
                let alpha = traits.userInterfaceStyle == .dark ? darkAlpha : lightAlpha
                return UIColor(
                    red: CGFloat((hex >> 16) & 0xff) / 255, green: CGFloat((hex >> 8) & 0xff) / 255,
                    blue: CGFloat(hex & 0xff) / 255, alpha: alpha)
            })
    }

    init(hex: UInt32, alpha: Double = 1) {
        self.init(
            .sRGB, red: Double((hex >> 16) & 0xff) / 255, green: Double((hex >> 8) & 0xff) / 255,
            blue: Double(hex & 0xff) / 255, opacity: alpha)
    }
}

enum Theme {
    static let bg = Color(light: 0xF2F2F7, dark: 0x000000)
    static let card = Color(light: 0xFFFFFF, dark: 0x1C1C1E)
    static let label = Color(light: 0x000000, dark: 0xFFFFFF)
    static let secondary = Color(light: 0x5C5C61, dark: 0xA6A6AE)
    static let tertiary = Color(light: 0x6E6E73, dark: 0x93939B)
    static let separator = Color(light: 0x3C3C43, dark: 0x787880, lightAlpha: 0.20, darkAlpha: 0.36)
    static let tint = Color(light: 0x0A66D6, dark: 0x4AA3FF)
    static let onTint = Color(light: 0xFFFFFF, dark: 0x00213F)
    static let fill = Color(light: 0x767680, dark: 0x787880, lightAlpha: 0.14, darkAlpha: 0.28)
    static let green = Color(light: 0x1F7A37, dark: 0x45D166)
    static let orange = Color(light: 0xA84A00, dark: 0xFFA53D)
    static let red = Color(light: 0xC4221A, dark: 0xFF6259)
    static let live = Color(hex: 0x1F8F3D)
    static let sessionRed = Color(hex: 0xFF7A70)
    static let sessionGlass = Color(hex: 0x16161A, alpha: 0.70)
    static let hudGlass = Color(hex: 0x121216, alpha: 0.66)

    static let largeTitle = Font.system(size: 34, weight: .bold)
    static let stateHeadline = Font.system(size: 28, weight: .bold)
    static let title2 = Font.system(size: 22, weight: .bold)
    static let headline = Font.system(size: 17, weight: .semibold)
    static let body = Font.system(size: 17)
    static let subhead = Font.system(size: 15)
    static let groupHeader = Font.system(size: 15, weight: .semibold)
    static let footnote = Font.system(size: 13)

    static let cardRadius: CGFloat = 26
}

/// A glass capsule, as the toolbars and round buttons use.
struct Glass: ViewModifier {
    var radius: CGFloat = 24
    func body(content: Content) -> some View {
        content
            .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: radius, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: radius, style: .continuous).strokeBorder(.white.opacity(0.18), lineWidth: 1))
            .shadow(color: .black.opacity(0.14), radius: 14, y: 8)
    }
}

/// The dark glass that floats over the picture during a session, whatever the system theme.
struct SessionGlass: ViewModifier {
    var radius: CGFloat = 30
    func body(content: Content) -> some View {
        content
            .background(Theme.sessionGlass, in: RoundedRectangle(cornerRadius: radius, style: .continuous))
            .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: radius, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: radius, style: .continuous).strokeBorder(.white.opacity(0.16), lineWidth: 1))
            .environment(\.colorScheme, .dark)
    }
}

extension View {
    func glass(radius: CGFloat = 24) -> some View { modifier(Glass(radius: radius)) }
    func sessionGlass(radius: CGFloat = 30) -> some View { modifier(SessionGlass(radius: radius)) }

    /// A control for something remoteman can't do yet: drawn as designed, but dimmed and inert.
    func available(_ on: Bool) -> some View {
        disabled(!on).opacity(on ? 1 : 0.4)
    }
}

/// A round glass button with an icon, 44 points across.
struct CircleButton: View {
    let icon: String
    let label: String
    var tinted = false
    let action: () -> Void
    var body: some View {
        Button(action: action) {
            Image(systemName: icon).font(.system(size: 19, weight: .medium))
                .foregroundStyle(tinted ? Theme.onTint : Theme.label)
                .frame(width: 44, height: 44)
                .background { if tinted { Circle().fill(Theme.tint) } }
        }
        .buttonStyle(.plain)
        .modifier(GlassIf(on: !tinted, radius: 22))
        .accessibilityLabel(label)
    }
}

private struct GlassIf: ViewModifier {
    let on: Bool
    let radius: CGFloat
    func body(content: Content) -> some View {
        if on { content.glass(radius: radius) } else { content }
    }
}

/// The wide capsule buttons at the bottom of pages and sheets.
struct WideButtonStyle: ButtonStyle {
    enum Kind { case primary, secondary, destructive }
    var kind: Kind = .primary
    var small = false
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: small ? 15 : 17, weight: .semibold))
            .foregroundStyle(kind == .primary ? Theme.onTint : kind == .destructive ? Theme.red : Theme.label)
            .padding(.horizontal, small ? 18 : 24)
            .frame(maxWidth: small ? nil : .infinity, minHeight: small ? 44 : 50)
            .background(kind == .primary ? Theme.tint : Theme.fill, in: Capsule())
            .opacity(configuration.isPressed ? 0.7 : 1)
    }
}

/// An inset group of rows with an optional header and footer, as the design's lists are.
struct InsetGroup<Content: View>: View {
    var header: String?
    var footer: String?
    @ViewBuilder var content: Content
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let header {
                Text(header).font(Theme.groupHeader).foregroundStyle(Theme.secondary)
                    .padding(.horizontal, 20).padding(.bottom, 8)
            }
            VStack(spacing: 0) { content }
                .background(Theme.card, in: RoundedRectangle(cornerRadius: Theme.cardRadius, style: .continuous))
            if let footer {
                Text(footer).font(Theme.footnote).foregroundStyle(Theme.secondary)
                    .padding(.horizontal, 20).padding(.top, 8)
            }
        }
    }
}

/// One row of an inset group: a label, an optional subtitle, and something trailing.
struct Row<Trailing: View>: View {
    let title: String
    var subtitle: String?
    var last = false
    @ViewBuilder var trailing: Trailing
    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 14) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(Theme.body).foregroundStyle(Theme.label)
                    if let subtitle { Text(subtitle).font(Theme.subhead).foregroundStyle(Theme.secondary) }
                }
                Spacer(minLength: 8)
                trailing
            }
            .padding(.horizontal, 20).padding(.vertical, 8)
            .frame(minHeight: 54)
            // The whole row takes a tap, the empty middle too, when it is a button's label.
            .contentShape(Rectangle())
            if !last { Rectangle().fill(Theme.separator).frame(height: 0.5).padding(.leading, 20) }
        }
    }
}

/// The right-hand value of a row, with the chevron of a row that opens something.
struct RowValue: View {
    let text: String
    var chevron = false
    var mono = false
    var body: some View {
        HStack(spacing: 6) {
            Text(text).font(mono ? .system(size: 15, design: .monospaced) : Theme.body).foregroundStyle(Theme.secondary)
                .multilineTextAlignment(.trailing)
            if chevron { Image(systemName: "chevron.right").font(.system(size: 13, weight: .bold)).foregroundStyle(Theme.tertiary) }
        }
    }
}

/// The centred icon, headline and text of a state page ("… is paired", "Can't reach …").
struct StateBlock: View {
    let icon: String
    var iconColor: Color = Theme.label
    var circle: Color = Theme.fill
    let headline: String
    let text: String
    var body: some View {
        VStack(spacing: 14) {
            Image(systemName: icon).font(.system(size: 44, weight: .light)).foregroundStyle(iconColor)
                .frame(width: 96, height: 96).background(circle, in: Circle())
            Text(headline).font(Theme.stateHeadline).multilineTextAlignment(.center)
            Text(text).font(Theme.body).foregroundStyle(Theme.secondary).multilineTextAlignment(.center)
        }
    }
}
