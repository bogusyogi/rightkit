import SwiftUI
#if canImport(AppKit) && !targetEnvironment(macCatalyst)
import AppKit
#elseif canImport(UIKit)
import UIKit
#endif

/// Card entrance and exit: opacity, a slight scale and a short slide, driven by one animation each
/// way. With Reduce Motion on, the card appears and disappears without animation. The card stops
/// taking hits and is hidden from accessibility while it is not presented.
struct RightKitCardPresence: ViewModifier {
    let presented: Bool
    let animation: Animation
    let entranceScale: CGFloat
    let entranceOffset: CGFloat
    let anchor: UnitPoint
    @State private var shown = false

    func body(content: Content) -> some View {
        let reduced = Self.reduceMotion
        let visible = reduced ? presented : shown
        content
            .opacity(visible ? 1 : 0)
            .scaleEffect(visible ? 1 : entranceScale, anchor: anchor)
            .offset(y: visible ? 0 : entranceOffset)
            .allowsHitTesting(presented)
            .accessibilityHidden(!presented)
            .onAppear { present(presented) }
            .onChange(of: presented) { _, next in present(next) }
    }

    private func present(_ next: Bool) {
        if Self.reduceMotion {
            shown = next
        } else {
            withAnimation(animation) { shown = next }
        }
    }

    private static var reduceMotion: Bool {
        #if canImport(AppKit) && !targetEnvironment(macCatalyst)
        NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
        #elseif canImport(UIKit)
        UIAccessibility.isReduceMotionEnabled
        #else
        false
        #endif
    }
}

extension View {
    /// Presents the view as a card that enters and leaves with `animation`: it starts at
    /// `entranceScale` about `anchor` and `entranceOffset` points down, then settles. The host keeps
    /// its own spring, scale, offset and anchor.
    public func rightKitCardPresence(
        presented: Bool,
        animation: Animation,
        entranceScale: CGFloat,
        entranceOffset: CGFloat,
        anchor: UnitPoint
    ) -> some View {
        modifier(RightKitCardPresence(
            presented: presented,
            animation: animation,
            entranceScale: entranceScale,
            entranceOffset: entranceOffset,
            anchor: anchor
        ))
    }
}
