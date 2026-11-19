// A monitor connected to the iPad as the session's second display: the computer is asked for a
// display the monitor's size, and its picture fills the monitor. The monitor takes no touches,
// so the iPad's own screen can be made the touch surface for it (ViewerSession.touchDisplay).
import SwiftUI
import UIKit

final class ExternalDisplay: ObservableObject {
    static let shared = ExternalDisplay()
    /// The connected monitor's pixels, scale and refresh rate; nil with none connected.
    @Published private(set) var monitor: ViewerSession.Display?
    fileprivate weak var view: ExternalView?
    private weak var session: ViewerSession?

    fileprivate func connected(_ scene: UIWindowScene, view: ExternalView) {
        let s = scene.screen
        monitor = ViewerSession.Display(
            width: Int(s.nativeBounds.width) & ~1, height: Int(s.nativeBounds.height) & ~1, scale: Float(s.nativeScale),
            refresh: s.maximumFramesPerSecond)
        self.view = view
        view.session = session
    }

    fileprivate func disconnected() {
        monitor = nil
        view = nil
        // The monitor went: the computer gets its display back.
        if let s = session, s.secondOn { s.setSecondDisplay(nil) }
    }

    /// Show this session's second picture on the monitor and ask the computer for it, or stop.
    func use(_ on: Bool, for session: ViewerSession) {
        if on, let monitor {
            self.session = session
            view?.session = session
            session.setSecondDisplay(monitor)
        } else {
            session.touchDisplay = 0
            session.setSecondDisplay(nil)
            view?.session = nil
            self.session = nil
        }
    }
}

/// The monitor's scene: one window, the second picture or nothing.
final class ExternalSceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?

    func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options: UIScene.ConnectionOptions) {
        guard let scene = scene as? UIWindowScene else { return }
        let w = UIWindow(windowScene: scene)
        let controller = UIViewController()
        let view = ExternalView()
        controller.view = view
        w.rootViewController = controller
        w.isHidden = false
        window = w
        ExternalDisplay.shared.connected(scene, view: view)
    }

    func sceneDidDisconnect(_ scene: UIScene) {
        window = nil
        ExternalDisplay.shared.disconnected()
    }
}

final class ExternalView: UIView {
    weak var session: ViewerSession? {
        didSet {
            oldValue?.renderer2?.layer.removeFromSuperlayer()
            if let l = session?.renderer2?.layer { layer.addSublayer(l) }
            setNeedsLayout()
        }
    }
    private var link: CADisplayLink?

    override func didMoveToWindow() {
        super.didMoveToWindow()
        backgroundColor = .black
        link?.invalidate()
        link = nil
        guard let window else { return }
        let l = CADisplayLink(target: self, selector: #selector(tick))
        let top = Float(window.screen.maximumFramesPerSecond)
        l.preferredFrameRateRange = CAFrameRateRange(minimum: min(60, top), maximum: top, preferred: top)
        l.add(to: .main, forMode: .common)
        link = l
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        guard let r = session?.renderer2 else { return }
        let scale = window?.screen.nativeScale ?? contentScaleFactor
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        r.layer.frame = bounds
        r.layer.contentsScale = scale
        r.drawableSize = CGSize(width: bounds.width * scale, height: bounds.height * scale)
        CATransaction.commit()
        r.setNeedsRedraw()
    }

    @objc private func tick() { session?.renderer2?.draw() }
}

/// Gives a connected monitor its own scene; every other window is SwiftUI's as usual.
final class AppDelegate: NSObject, UIApplicationDelegate {
    func application(_ application: UIApplication, configurationForConnecting session: UISceneSession, options: UIScene.ConnectionOptions) -> UISceneConfiguration {
        let config = UISceneConfiguration(name: nil, sessionRole: session.role)
        if session.role == .windowExternalDisplayNonInteractive { config.delegateClass = ExternalSceneDelegate.self }
        return config
    }
}
