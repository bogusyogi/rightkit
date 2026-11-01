#if os(macOS)
import SwiftUI

/// The pointer the notch's cards ask for. The Mac supplies the AppKit
/// implementation at the root; the default does nothing.
@MainActor
protocol PointerCursor {
    func pointingHand()
    func reset()
}

struct NoOpPointerCursor: PointerCursor {
    func pointingHand() {}
    func reset() {}
}

/// Holds the registered implementation, set once by the platform adapters.
@MainActor
enum PointerCursorHost {
    static var current: any PointerCursor = NoOpPointerCursor()
}

/// Environment default: forwards to whatever is registered at call time.
struct RegisteredPointerCursor: PointerCursor {
    nonisolated init() {}
    func pointingHand() { PointerCursorHost.current.pointingHand() }
    func reset() { PointerCursorHost.current.reset() }
}

private struct PointerCursorKey: EnvironmentKey {
    static let defaultValue: any PointerCursor = RegisteredPointerCursor()
}

extension EnvironmentValues {
    var pointerCursor: any PointerCursor {
        get { self[PointerCursorKey.self] }
        set { self[PointerCursorKey.self] = newValue }
    }
}
#endif
