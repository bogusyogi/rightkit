#if os(macOS)
import AppKit
import RightKitOverlay
import SwiftUI

/// The full-screen surface a corner passage is drawn on — click-through, and
/// just below the notch. Put up when a drag begins and kept up until it ends,
/// empty whenever the notch is on an edge: a window brought up at the corner
/// itself arrived late and faded in, which is what going round a corner
/// looked like.
///
/// This is the AppKit side of the corner passage (NSWindow/NSHostingView);
/// the drawing it hosts, `CornerPassageView`, lives in `CornerPassage.swift`
/// and is AppKit-free.
@MainActor
final class CornerPassageOverlay {
    private var window: RightKitOverlayPanel?
    private var hosting: NSHostingView<CornerPassageView>?
    let screen: NSScreen

    init(screen: NSScreen) { self.screen = screen }

    var track: BorderTrack {
        BorderTrack(width: screen.frame.width, height: screen.frame.height)
    }

    func show(_ view: CornerPassageView) {
        if let hosting {
            hosting.rootView = view
            return
        }
        let frame = screen.frame
        let hostingView = NSHostingView(rootView: view)
        // Click-through from the start, no fade of its own in or out (it is part of the notch
        // moving), just below the notch, on every space and over full-screen ones.
        let panel = RightKitOverlayPanel(
            contentRect: frame,
            kind: .notch,
            configuration: RightKitOverlayPanelConfiguration(
                level: NSWindow.Level(rawValue: NSWindow.Level.statusBar.rawValue - 1),
                collectionBehavior: RightKitOverlayPanelConfiguration.stationaryFullScreenAuxiliary,
                canBecomeKey: false,
                animationBehavior: .none,
                startsClickThrough: true))
        panel.contentView = hostingView
        panel.setFrame(frame, display: false)
        panel.orderFront(nil)
        window = panel
        hosting = hostingView
    }

    /// Up and empty, ready for the first corner.
    func prepare() { show(.empty(on: track)) }

    /// Empty again, the notch back on an edge.
    func clear() { hosting?.rootView = .empty(on: track) }

    func hide() {
        window?.orderOut(nil)
        window = nil
        hosting = nil
    }

}
#endif
