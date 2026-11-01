#if os(macOS)
import AppKit

struct AppKitPointerCursor: PointerCursor {
    func pointingHand() { NSCursor.pointingHand.set() }
    func reset() { NSCursor.arrow.set() }
}
#endif
