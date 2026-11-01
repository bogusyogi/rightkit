#if os(macOS)
import AppKit

extension NotchSurfaceStyle {
    /// One window-level switch decides both halves of "how dark is this notch":
    /// the dynamic `NSColor`s in `Palette` and SwiftUI's `colorScheme` are both
    /// resolved against the window's appearance, so pinning it here saves
    /// threading a style through every view that picks a colour.
    ///
    /// `nil` is not a fallback — it is the whole point of the glass style. With
    /// no appearance of our own, light or dark, Clear or Tinted all arrive from
    /// the Mac's Appearance settings; naming one would quietly overrule the
    /// user there. `darkGlass` is the deliberate opposite: it asks for a notch
    /// that is dark whatever the Mac is doing, so it pins `darkAqua` like
    /// `solid` and keeps `Palette`'s frame-sampled hexes.
    ///
    /// Reduce transparency is the exception the window has to be told about:
    /// it means "no see-through chrome", which for the notch is the solid
    /// style, and a light palette on a black surface would be unreadable. The
    /// precedence is the Settings window's — reduce transparency first, then
    /// glass, then the opaque fill.
    func panelAppearance(reduceTransparency: Bool) -> NSAppearance? {
        effective == .glass && !reduceTransparency ? nil : NSAppearance(named: .darkAqua)
    }
}
#endif
