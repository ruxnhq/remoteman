// The pieces both Mac apps (viewer and host) are drawn with: the design's colours and the few
// shapes it repeats (grouped cards and their rows, group labels, footnotes, status dots, the
// large capsule button). Controls themselves are the system's.
import AppKit
import SwiftUI

/// The design's colours, each with its light and dark value.
enum Tokens {
    private static func rgb(_ hex: UInt32, _ alpha: CGFloat = 1) -> NSColor {
        NSColor(
            srgbRed: CGFloat((hex >> 16) & 0xff) / 255, green: CGFloat((hex >> 8) & 0xff) / 255,
            blue: CGFloat(hex & 0xff) / 255, alpha: alpha)
    }
    private static func pair(_ light: NSColor, _ dark: NSColor) -> Color {
        Color(nsColor: NSColor(name: nil) { $0.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua ? dark : light })
    }
    static func fixed(_ hex: UInt32, _ alpha: CGFloat = 1) -> Color { Color(nsColor: rgb(hex, alpha)) }

    static let win = pair(rgb(0xFFFFFF), rgb(0x1F1F22))
    static let bar = pair(rgb(0xF6F6F8), rgb(0x29292D))
    static let label = pair(rgb(0x1C1C1E), rgb(0xF1F1F3))
    static let l2 = pair(rgb(0x5D5D64), rgb(0xAAAAB2))
    static let l3 = pair(rgb(0x6A6A71), rgb(0x9A9AA2))
    static let sep = pair(rgb(0x000000, 0.10), rgb(0xFFFFFF, 0.12))
    static let tint = fixed(0x0A66D6)
    static let tintfg = pair(rgb(0x0A62CF), rgb(0x69B4FF))
    static let fill = pair(rgb(0x000000, 0.065), rgb(0xFFFFFF, 0.10))
    static let green = pair(rgb(0x1C7A36), rgb(0x4AD26A))
    static let orange = pair(rgb(0xA64B00), rgb(0xFFAB45))
    static let red = pair(rgb(0xC2261C), rgb(0xFF9088))
    static let box = pair(rgb(0x000000, 0.028), rgb(0xFFFFFF, 0.045))
    static let boxBorder = pair(rgb(0x000000, 0.075), rgb(0xFFFFFF, 0.085))
    static let sel = pair(rgb(0x000000, 0.085), rgb(0xFFFFFF, 0.13))
    static let appIcon = fixed(0x20324A)
    static let thumbnail = fixed(0x27425E)
}

// MARK: Grouped cards

/// A grouped-form card: its children are rows, with a hairline between each two.
struct Card<Content: View>: View {
    @ViewBuilder var content: Content
    var body: some View { _VariadicView.Tree(CardRoot()) { content } }
}

private struct CardRoot: _VariadicView_UnaryViewRoot {
    func body(children: _VariadicView.Children) -> some View {
        let last = children.last?.id
        VStack(spacing: 0) {
            ForEach(children) { child in
                child
                if child.id != last {
                    Rectangle().fill(Tokens.sep).frame(height: 1).padding(.horizontal, 12)
                }
            }
        }
        .background(RoundedRectangle(cornerRadius: 10).fill(Tokens.box))
        .clipShape(RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(Tokens.boxBorder, lineWidth: 1))
    }
}

/// One row of a card: a title, an optional caption under it, and whatever sits at the right.
struct Row<Leading: View, Trailing: View>: View {
    var title: String
    var caption: String?
    var strong = false
    @ViewBuilder var leading: Leading
    @ViewBuilder var trailing: Trailing

    var body: some View {
        HStack(spacing: 12) {
            leading
            VStack(alignment: .leading, spacing: 1) {
                Text(title).font(.system(size: 13, weight: strong ? .semibold : .regular)).foregroundStyle(Tokens.label)
                if let caption {
                    Text(caption).font(.system(size: 11)).foregroundStyle(Tokens.l2)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 12)
            trailing
        }
        .padding(.vertical, 6)
        .padding(.horizontal, 12)
        .frame(minHeight: 40)
    }
}

extension Row where Leading == EmptyView {
    init(_ title: String, caption: String? = nil, strong: Bool = false, @ViewBuilder trailing: () -> Trailing) {
        self.init(title: title, caption: caption, strong: strong, leading: { EmptyView() }, trailing: trailing)
    }
}

extension Row where Leading == EmptyView, Trailing == ValueText {
    /// A read-only row: a title and its value.
    init(_ title: String, caption: String? = nil, value: String, dim: Bool = false, mono: Bool = false) {
        self.init(title: title, caption: caption, leading: { EmptyView() }, trailing: { ValueText(value, dim: dim, mono: mono) })
    }
}

/// A read-only value at the right of a row.
struct ValueText: View {
    var text: String
    var dim: Bool
    var mono: Bool
    init(_ text: String, dim: Bool = false, mono: Bool = false) {
        self.text = text
        self.dim = dim
        self.mono = mono
    }
    var body: some View {
        Text(text)
            .font(mono ? .system(size: 12, design: .monospaced) : .system(size: 13))
            .foregroundStyle(dim ? Tokens.l2 : Tokens.label)
            .multilineTextAlignment(.trailing)
            .textSelection(.enabled)
    }
}

/// A labelled group: the label, the card, and an optional footnote under it.
struct CardGroup<Content: View>: View {
    var label: String?
    var footnote: String?
    @ViewBuilder var content: Content
    init(_ label: String? = nil, footnote: String? = nil, @ViewBuilder content: () -> Content) {
        self.label = label
        self.footnote = footnote
        self.content = content()
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let label {
                Text(label).font(.system(size: 13, weight: .semibold)).foregroundStyle(Tokens.label)
                    .padding(.horizontal, 2).padding(.bottom, 6)
            }
            Card { content }
            if let footnote {
                Text(footnote).font(.system(size: 11)).foregroundStyle(Tokens.l2)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, 12).padding(.top, 6)
            }
        }
    }
}

struct StatusDot: View {
    var color: Color
    var body: some View { Circle().fill(color).frame(width: 8, height: 8) }
}

/// The design's large capsule button (36 high), filled with the accent when primary.
struct LargeButtonStyle: ButtonStyle {
    var primary = false
    @Environment(\.isEnabled) private var enabled
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 14, weight: .semibold))
            .foregroundStyle(primary ? Color.white : Tokens.label)
            .padding(.horizontal, 20)
            .frame(height: 36)
            .background(Capsule().fill(primary ? Tokens.tint : Tokens.fill))
            .opacity(enabled ? (configuration.isPressed ? 0.8 : 1) : 0.45)
            .contentShape(Capsule())
    }
}

/// The app's icon as the design draws it: a dark tile with a display outline and a dot.
struct AppIconTile: View {
    var size: CGFloat = 64
    var body: some View {
        RoundedRectangle(cornerRadius: size * 15 / 64, style: .continuous)
            .fill(Tokens.appIcon)
            .frame(width: size, height: size)
            .overlay {
                ZStack {
                    Image(systemName: "display").font(.system(size: size * 0.5, weight: .regular))
                    Circle().frame(width: size * 0.1, height: size * 0.1).offset(y: -size * 0.05)
                }
                .foregroundStyle(.white)
            }
            .accessibilityHidden(true)
    }
}

/// A coloured square with a white glyph (a device in the host panel, a permission in the first run).
struct GlyphTile: View {
    var symbol: String
    var color: Color = Tokens.tint
    var size: CGFloat = 36
    var body: some View {
        RoundedRectangle(cornerRadius: 10, style: .continuous).fill(color).frame(width: size, height: size)
            .overlay(Image(systemName: symbol).font(.system(size: size * 0.5)).foregroundStyle(.white))
            .accessibilityHidden(true)
    }
}

extension View {
    /// A control for something the engine can't do yet: drawn disabled, doing nothing.
    func available(_ flag: Bool) -> some View {
        disabled(!flag).help(flag ? "" : "Not available yet")
    }
}

/// Reports the window a view sits in.
struct WindowReader: NSViewRepresentable {
    var found: (NSWindow?) -> Void
    final class V: NSView {
        var found: ((NSWindow?) -> Void)?
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            let w = window
            DispatchQueue.main.async { self.found?(w) }
        }
    }
    func makeNSView(context: Context) -> V {
        let v = V()
        v.found = found
        return v
    }
    func updateNSView(_ v: V, context: Context) { v.found = found }
}

enum Fingerprint {
    /// A key's fingerprint in groups of four, as the design shows it; long ones are cut short.
    static func shown(_ raw: String) -> String {
        guard !raw.isEmpty else { return "Shown after the first connection" }
        let hex = raw.uppercased().filter { $0.isHexDigit }
        guard hex.count >= 16, hex.count == raw.filter({ !$0.isWhitespace && $0 != ":" }).count else { return raw }
        let groups = stride(from: 0, to: 16, by: 4).map { i -> String in
            let s = hex.index(hex.startIndex, offsetBy: i)
            return String(hex[s..<hex.index(s, offsetBy: 4)])
        }
        return groups.joined(separator: " ")
    }
}
