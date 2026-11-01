#if os(macOS)
import AppKit
import SwiftUI

/// The single place the Mac adapters are registered. Applied once from
/// `NotchHostingView`; idempotent.
@MainActor
enum MacNotchAdapters {
    private static var registered = false

    static func register() {
        guard !registered else { return }
        registered = true
        PointerCursorHost.current = AppKitPointerCursor()
        NearbyActivityHost.source = NearbySharing.shared
        RingBurstOverlayHost.make = { AnyView(ClipBurstDots()) }
        // step 3:
        // step 4:
    }
}

extension NearbySharing: NearbyActivitySource {}
#endif
