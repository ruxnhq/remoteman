// The panel under the menu-bar item. A panel of the app's own, put exactly under the item:
// the system's popover opened some 40 points below the menu bar on this macOS, floating over
// whatever was on screen, and let that show through. This one sits against the menu bar,
// centred under the item (kept inside the screen), with an opaque menu background, and goes
// away on a click anywhere else, on Escape, or when another app comes forward.
import AppKit
import SwiftUI

final class MenuPanel: NSObject {
    private let panel: NSPanel
    private let host: NSHostingView<AnyView>
    private var monitors: [Any] = []
    var isShown: Bool { panel.isVisible }

    init<V: View>(content: V) {
        host = NSHostingView(rootView: AnyView(content))
        panel = KeyPanel(contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: true)
        super.init()
        panel.isFloatingPanel = true
        panel.level = .popUpMenu
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.hidesOnDeactivate = false
        panel.collectionBehavior = [.transient, .ignoresCycle, .fullScreenAuxiliary]
        let back = NSVisualEffectView()
        back.material = .menu
        back.state = .active
        back.blendingMode = .behindWindow
        back.wantsLayer = true
        back.layer?.cornerRadius = 14
        back.layer?.cornerCurve = .continuous
        back.layer?.masksToBounds = true
        back.layer?.borderWidth = 0.5
        back.layer?.borderColor = NSColor.separatorColor.cgColor
        host.translatesAutoresizingMaskIntoConstraints = false
        back.addSubview(host)
        NSLayoutConstraint.activate([
            host.leadingAnchor.constraint(equalTo: back.leadingAnchor), host.trailingAnchor.constraint(equalTo: back.trailingAnchor),
            host.topAnchor.constraint(equalTo: back.topAnchor), host.bottomAnchor.constraint(equalTo: back.bottomAnchor),
        ])
        panel.contentView = back
    }

    /// Where the panel goes for an item whose window is at `item` on `screen`: its top against
    /// the menu bar's bottom edge (the item's own bottom), centred under the item, inside the screen.
    static func frame(size: NSSize, item: NSRect, screen: NSRect) -> NSRect {
        let gap: CGFloat = 4, margin: CGFloat = 8
        var x = item.midX - size.width / 2
        x = min(max(x, screen.minX + margin), screen.maxX - size.width - margin)
        let top = item.minY - gap
        return NSRect(x: x.rounded(), y: (top - size.height).rounded(), width: size.width, height: size.height)
    }

    func show(under button: NSStatusBarButton) {
        guard let window = button.window else { return }
        let size = host.fittingSize
        let screen = (window.screen ?? NSScreen.main)?.frame ?? .zero
        panel.setFrame(Self.frame(size: size, item: window.frame, screen: screen), display: true)
        panel.makeKeyAndOrderFront(nil)
        button.highlight(true)
        // A click anywhere else, in this app or another, puts it away; so does Escape.
        let outside: (NSEvent) -> Void = { [weak self, weak button] event in
            guard let self, self.panel.isVisible else { return }
            if event.window === self.panel || event.window === button?.window { return }
            self.close(button)
        }
        if let m = NSEvent.addGlobalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown], handler: outside) { monitors.append(m) }
        if let m = NSEvent.addLocalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown, .keyDown], handler: { [weak self, weak button] event in
            if event.type == .keyDown {
                if event.keyCode == 53, self?.panel.isVisible == true {
                    self?.close(button)
                    return nil
                }
                return event
            }
            outside(event)
            return event
        }) { monitors.append(m) }
        NotificationCenter.default.addObserver(forName: NSApplication.didResignActiveNotification, object: nil, queue: .main) { [weak self, weak button] _ in
            self?.close(button)
        }
    }

    func close(_ button: NSStatusBarButton? = nil) {
        panel.orderOut(nil)
        button?.highlight(false)
        monitors.forEach { NSEvent.removeMonitor($0) }
        monitors = []
        NotificationCenter.default.removeObserver(self, name: NSApplication.didResignActiveNotification, object: nil)
    }

    var frame: NSRect { panel.frame }
    var screenName: String { panel.screen?.localizedName ?? "no screen" }
}

/// A borderless panel that may take the keyboard (for Escape and the panel's own controls).
private final class KeyPanel: NSPanel {
    override var canBecomeKey: Bool { true }
}
