// The taps an iPhone gives under the finger. An iPad has no Taptic Engine, so these do nothing
// there. Kept sparse: a tap where a physical control would click, and a mark when something
// the user is waiting for finishes or fails.
import UIKit

enum Haptics {
    private static let light = UIImpactFeedbackGenerator(style: .light)
    private static let rigid = UIImpactFeedbackGenerator(style: .rigid)
    private static let selection = UISelectionFeedbackGenerator()
    private static let notice = UINotificationFeedbackGenerator()

    /// A key on the key strip.
    static func key() { light.impactOccurred(intensity: 0.7) }
    /// A modifier latched or released, a control switched on or off.
    static func toggle(_ on: Bool) { rigid.impactOccurred(intensity: on ? 1 : 0.6) }
    /// A choice in a menu.
    static func choice() { selection.selectionChanged() }
    /// The picture arrived; a file got there.
    static func success() { notice.notificationOccurred(.success) }
    /// Something destructive was done.
    static func warning() { notice.notificationOccurred(.warning) }
    /// The connection failed or ended by itself; a file didn't get there.
    static func failure() { notice.notificationOccurred(.error) }
}
