#if os(macOS)
import AppKit
import SwiftUI

/// A transparent overlay that reports whether the pointer is inside the view it overlays, even while
/// the window is not key (`.activeAlways`). It never takes part in hit testing, so clicks reach the
/// SwiftUI control underneath. The pointer is reconciled on every tracking-area rebuild and on every
/// SwiftUI update, so a view that appears or moves under a still pointer reports the right state.
/// `tooltip` is the system tooltip of the region (nil for none).
public struct RightKitHoverRegion: NSViewRepresentable {
    public let tooltip: String?
    public let onChange: (Bool) -> Void

    public init(tooltip: String?, onChange: @escaping (Bool) -> Void) {
        self.tooltip = tooltip
        self.onChange = onChange
    }

    public func makeNSView(context: Context) -> RightKitHoverView {
        RightKitHoverView(onChange: onChange, tooltip: tooltip)
    }

    public func updateNSView(_ view: RightKitHoverView, context: Context) {
        view.onChange = onChange
        view.toolTip = tooltip
        view.reconcilePointer()
    }
}

@MainActor
public final class RightKitHoverView: NSView {
    var onChange: (Bool) -> Void
    private var tracker: NSTrackingArea?

    init(onChange: @escaping (Bool) -> Void, tooltip: String?) {
        self.onChange = onChange
        super.init(frame: .zero)
        self.toolTip = tooltip
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    public override func hitTest(_ point: NSPoint) -> NSView? { nil }

    public override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let tracker { removeTrackingArea(tracker) }
        let tracker = NSTrackingArea(
            rect: bounds,
            options: [.activeAlways, .mouseEnteredAndExited, .inVisibleRect],
            owner: self,
            userInfo: nil
        )
        addTrackingArea(tracker)
        self.tracker = tracker
        DispatchQueue.main.async { [weak self] in
            MainActor.assumeIsolated { self?.reconcilePointer() }
        }
    }

    public override func mouseEntered(with event: NSEvent) { onChange(true) }
    public override func mouseExited(with event: NSEvent) { onChange(false) }

    func reconcilePointer() {
        guard let window else { return }
        let inWindow = window.convertPoint(fromScreen: NSEvent.mouseLocation)
        onChange(bounds.contains(convert(inWindow, from: nil)))
    }
}

/// Catches a right-click (or two-finger click) on the view it overlays. It takes part in hit testing
/// only while a secondary click is being delivered, so ordinary clicks and hover reach the SwiftUI
/// button underneath untouched.
public struct RightKitSecondaryClickRegion: NSViewRepresentable {
    public let action: () -> Void

    public init(action: @escaping () -> Void) {
        self.action = action
    }

    public func makeNSView(context: Context) -> RightKitSecondaryClickView {
        RightKitSecondaryClickView(action: action)
    }

    public func updateNSView(_ view: RightKitSecondaryClickView, context: Context) {
        view.action = action
    }
}

@MainActor
public final class RightKitSecondaryClickView: NSView {
    var action: () -> Void

    init(action: @escaping () -> Void) {
        self.action = action
        super.init(frame: .zero)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    public override func hitTest(_ point: NSPoint) -> NSView? {
        guard NSApp.currentEvent?.type == .rightMouseDown else { return nil }
        return frame.contains(point) ? self : nil
    }

    public override func rightMouseDown(with event: NSEvent) { action() }
}
#endif
