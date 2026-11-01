import Foundation
import notify

/// Pulse fork: which of the Finder extension's menu items are on.
///
/// The extension is sandboxed and shares no preferences with Pulse, so the
/// choice travels as the 64-bit state of one system notification name that the
/// extension reads each time Finder asks for its menu (`FinderSync.menu(for:)`).
/// A bit set means that item is hidden: 1 Cut, 2 Copy Path, 4 Open in Terminal.
/// Zero, which is also what an unset name reads as, shows all three.
/// The state lives in the system's notification service, not on disk, so it is
/// republished at every launch of Pulse; until then (or if Pulse never ran)
/// the extension shows all three.
@MainActor
enum FinderMenuState {
    static let name = "dev.orthic.pulse.finder.menu"
    private static var token: Int32 = 0
    private static var registered = false

    static func publish(cut: Bool, copyPath: Bool, terminal: Bool) {
        if !registered {
            guard notify_register_check(name, &token) == 0 else { return }
            registered = true
        }
        var hidden: UInt64 = 0
        if !cut { hidden |= 1 }
        if !copyPath { hidden |= 2 }
        if !terminal { hidden |= 4 }
        _ = notify_set_state(token, hidden)
        _ = notify_post(name)
    }
}
