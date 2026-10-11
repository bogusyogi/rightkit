import Foundation
#if canImport(CoreGraphics)
import CoreGraphics
#endif

/// The overlay surface a host presents. Raw values are the persisted/wire identifiers and never
/// change; the case names are product-neutral. The product-named spellings remain as deprecated
/// aliases so existing call sites keep compiling.
public enum RightKitOverlayKind: String, Sendable, Codable {
    /// A small pill-shaped status surface.
    case pill = "heardRightPill"
    /// A notch-attached surface that may join full-screen spaces.
    case notch = "cockpitNotch"

    @available(*, deprecated, renamed: "pill")
    public static var heardRightPill: RightKitOverlayKind { .pill }
    @available(*, deprecated, renamed: "notch")
    public static var cockpitNotch: RightKitOverlayKind { .notch }
}

public enum RightKitPillMode: String, Sendable, Codable {
    case idle, starting, recording, processing, status, error
}

public enum RightKitOverlayEvent: Sendable, Equatable {
    case hide
    case idle
    case starting
    case recording
    case processing(cancellable: Bool)
    case status(String)
    case error(String)

    public var mode: RightKitPillMode? {
        switch self {
        case .hide: nil
        case .idle: .idle
        case .starting: .starting
        case .recording: .recording
        case .processing: .processing
        case .status: .status
        case .error: .error
        }
    }
}

extension RightKitOverlayEvent: Codable {
    private enum CodingKeys: String, CodingKey { case kind, cancellable, label }
    private enum Kind: String, Codable { case hide, idle, starting, recording, processing, status, error }

    public init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        switch try values.decode(Kind.self, forKey: .kind) {
        case .hide: self = .hide
        case .idle: self = .idle
        case .starting: self = .starting
        case .recording: self = .recording
        case .processing: self = .processing(cancellable: try values.decode(Bool.self, forKey: .cancellable))
        case .status: self = .status(try values.decode(String.self, forKey: .label))
        case .error: self = .error(try values.decode(String.self, forKey: .label))
        }
    }

    public func encode(to encoder: Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .hide: try values.encode(Kind.hide, forKey: .kind)
        case .idle: try values.encode(Kind.idle, forKey: .kind)
        case .starting: try values.encode(Kind.starting, forKey: .kind)
        case .recording: try values.encode(Kind.recording, forKey: .kind)
        case .processing(let cancellable):
            try values.encode(Kind.processing, forKey: .kind); try values.encode(cancellable, forKey: .cancellable)
        case .status(let label):
            try values.encode(Kind.status, forKey: .kind); try values.encode(label, forKey: .label)
        case .error(let label):
            try values.encode(Kind.error, forKey: .kind); try values.encode(label, forKey: .label)
        }
    }
}

public struct RightKitOverlayRect: Sendable, Equatable {
    public var x: Double
    public var y: Double
    public var width: Double
    public var height: Double

    public init(x: Double, y: Double, width: Double, height: Double) {
        self.x = x; self.y = y; self.width = width; self.height = height
    }

    public var isFinite: Bool {
        [x, y, width, height].allSatisfy { $0.isFinite } && width >= 0 && height >= 0
    }

    public func contains(x: Double, y: Double) -> Bool {
        isFinite && x.isFinite && y.isFinite && x >= self.x && x <= self.x + width && y >= self.y && y <= self.y + height
    }
}

public struct RightKitOverlayWorkArea: Sendable, Equatable {
    public var left: Double
    public var top: Double
    public var right: Double
    public var bottom: Double

    public init(left: Double, top: Double, right: Double, bottom: Double) {
        self.left = left; self.top = top; self.right = right; self.bottom = bottom
    }

    public func clamping(_ rect: RightKitOverlayRect) -> RightKitOverlayRect {
        guard rect.isFinite else { return rect }
        let maxX = max(left, right - rect.width)
        let maxY = max(top, bottom - rect.height)
        return RightKitOverlayRect(x: min(max(rect.x, left), maxX), y: min(max(rect.y, top), maxY), width: rect.width, height: rect.height)
    }
}

public struct RightKitOverlayModel: Sendable, Equatable {
    public let kind: RightKitOverlayKind
    public private(set) var visible = false
    public private(set) var mode: RightKitPillMode?
    public var frame: RightKitOverlayRect
    public var hitbox: RightKitOverlayRect?

    public init(kind: RightKitOverlayKind, frame: RightKitOverlayRect) {
        self.kind = kind; self.frame = frame
    }

    public mutating func apply(_ event: RightKitOverlayEvent) {
        mode = event.mode
        visible = mode != nil
    }

    /// Clamps the frame into `workArea`. A nil `hitbox` follows the frame by definition; an explicit
    /// hitbox is translated by the same displacement so it keeps its offset from the frame.
    public mutating func place(in workArea: RightKitOverlayWorkArea) {
        let placed = workArea.clamping(frame)
        if var box = hitbox, placed.isFinite {
            box.x += placed.x - frame.x; box.y += placed.y - frame.y
            hitbox = box
        }
        frame = placed
    }

    public func hitTest(x: Double, y: Double) -> Bool { visible && (hitbox ?? frame).contains(x: x, y: y) }
}

/// Which corner a set of hit rectangles is measured from. AppKit window coordinates have their
/// origin at the lower left (the default, and what every region meant before this type existed);
/// SwiftUI and most drawing code measure from the top left.
public enum RightKitOverlayCoordinateOrigin: Sendable, Equatable {
    case lowerLeft, topLeft
}

/// Pure click-through rule shared by the panel and by hosts that track the mouse themselves.
///
/// A borderless overlay is usually mostly transparent: only a few rectangles (the pill, a card, a
/// tooltip) should take the mouse and everything else should fall through to the app below. AppKit
/// has no per-rectangle hit testing for a window, so a host toggles the window's `ignoresMouseEvents`
/// as the pointer moves. This type is the decision: pointer inside any region means "take the mouse".
public struct RightKitOverlayHitRegions: Sendable, Equatable {
    /// Regions in the panel's window coordinate space, measured from ``origin``.
    public var regions: [CGRect]
    /// The corner `regions` are measured from. Defaults to `.lowerLeft`, as AppKit uses.
    public var origin: RightKitOverlayCoordinateOrigin

    public init(_ regions: [CGRect] = [], origin: RightKitOverlayCoordinateOrigin = .lowerLeft) {
        self.regions = regions
        self.origin = origin
    }

    /// Per-region toggling is active only while at least one region is set.
    public var isActive: Bool { !regions.isEmpty }

    /// `point` is in the same space as the regions (``origin`` is not applied).
    public func contains(_ point: CGPoint) -> Bool {
        point.x.isFinite && point.y.isFinite && regions.contains { $0.contains(point) }
    }

    /// `point` is in lower-left window coordinates (what AppKit hands out); it is converted when
    /// ``origin`` is `.topLeft`.
    public func contains(windowPoint point: CGPoint, windowHeight: CGFloat) -> Bool {
        guard point.x.isFinite, point.y.isFinite else { return false }
        switch origin {
        case .lowerLeft: return contains(point)
        case .topLeft:
            guard windowHeight.isFinite else { return false }
            return contains(CGPoint(x: point.x, y: windowHeight - point.y))
        }
    }

    /// The value for `ignoresMouseEvents` with the pointer at `point`, or nil when no region is set
    /// and the current state should be left alone. `point` is in the regions' own space.
    public func ignoresMouseEvents(at point: CGPoint) -> Bool? {
        isActive ? !contains(point) : nil
    }

    /// As ``ignoresMouseEvents(at:)`` for a lower-left window point, honouring ``origin``.
    public func ignoresMouseEvents(atWindowPoint point: CGPoint, windowHeight: CGFloat) -> Bool? {
        isActive ? !contains(windowPoint: point, windowHeight: windowHeight) : nil
    }
}

/// A display as the overlay layer sees it.
public struct RightKitOverlayDisplay: Sendable, Equatable {
    public let id: CGDirectDisplayID
    public let frame: CGRect
    public let visibleFrame: CGRect

    public init(id: CGDirectDisplayID, frame: CGRect, visibleFrame: CGRect) {
        self.id = id; self.frame = frame; self.visibleFrame = visibleFrame
    }
}

public enum RightKitOverlayDisplays {
    /// What to create and tear down to go from the displays you have a surface on (`current`) to
    /// the ones you want one on (`desired`). `hasDuplicates` reports a repeated id in `desired`;
    /// the sets ignore the repeat, and what to do about it stays with the host.
    public static func plan(current: Set<CGDirectDisplayID>, desired: [CGDirectDisplayID])
        -> (add: Set<CGDirectDisplayID>, remove: Set<CGDirectDisplayID>, hasDuplicates: Bool) {
        let wanted = Set(desired)
        return (wanted.subtracting(current), current.subtracting(wanted), wanted.count != desired.count)
    }
}

public enum RightKitOverlayFullScreen {
    /// Pure geometry: does any layer-0 window of `frontmostPID` match or span `screenBounds`
    /// (CoreGraphics coordinates, origin top left)? Ported unchanged from Pulse's detector.
    public static func isFullScreen(
        screenBounds: CGRect,
        frontmostPID: pid_t,
        windows: [(pid: pid_t, layer: Int, bounds: CGRect)],
        safeAreaTopInset: CGFloat = 0
    ) -> Bool {
        for window in windows {
            guard window.pid == frontmostPID, window.layer == 0 else { continue }
            let b = window.bounds

            // Must match screen width (within a small tolerance for borders and rounding).
            guard abs(b.origin.x - screenBounds.origin.x) <= 4,
                  abs(b.width - screenBounds.width) <= 4 else { continue }

            // Case 1: spans the full screen height.
            if abs(b.origin.y - screenBounds.origin.y) <= 4 && abs(b.height - screenBounds.height) <= 4 {
                return true
            }

            // Case 2: starts below a notch or menu bar, reaches the bottom, fills the display area.
            let maxTopInset = max(safeAreaTopInset, 40.0) + 4.0
            let reachesBottom = abs(b.maxY - screenBounds.maxY) <= 4
            let startsNearTop = b.origin.y >= screenBounds.origin.y - 4
                && b.origin.y <= screenBounds.origin.y + maxTopInset
            let occupiesMainArea = b.height >= screenBounds.height - (maxTopInset + 10)
            if reachesBottom && startsNearTop && occupiesMainArea { return true }
        }
        return false
    }
}

#if os(macOS)
import AppKit

/// Everything about a ``RightKitOverlayPanel`` that a host may vary. ``standard(kind:level:)`` is
/// exactly what the panel has always built; a host starts from it and changes what it needs.
public struct RightKitOverlayPanelConfiguration: Sendable {
    public var level: NSWindow.Level
    public var collectionBehavior: NSWindow.CollectionBehavior
    /// Whether the panel may become key (for a text field or a hide-on-resign launcher). It can
    /// never become main, whatever this says.
    public var canBecomeKey: Bool
    public var becomesKeyOnlyIfNeeded: Bool
    public var isFloatingPanel: Bool
    public var hasShadow: Bool
    public var isMovable: Bool
    public var isMovableByWindowBackground: Bool
    public var animationBehavior: NSWindow.AnimationBehavior
    /// Start with `ignoresMouseEvents == true` instead of interactive.
    public var startsClickThrough: Bool
    public var hitRegionOrigin: RightKitOverlayCoordinateOrigin

    public init(
        level: NSWindow.Level = .statusBar,
        collectionBehavior: NSWindow.CollectionBehavior = RightKitOverlayPanelConfiguration.stationaryAllSpaces,
        canBecomeKey: Bool = false,
        becomesKeyOnlyIfNeeded: Bool = true,
        isFloatingPanel: Bool = false,
        hasShadow: Bool = false,
        isMovable: Bool = true,
        isMovableByWindowBackground: Bool = false,
        animationBehavior: NSWindow.AnimationBehavior = .default,
        startsClickThrough: Bool = false,
        hitRegionOrigin: RightKitOverlayCoordinateOrigin = .lowerLeft
    ) {
        self.level = level
        self.collectionBehavior = collectionBehavior
        self.canBecomeKey = canBecomeKey
        self.becomesKeyOnlyIfNeeded = becomesKeyOnlyIfNeeded
        self.isFloatingPanel = isFloatingPanel
        self.hasShadow = hasShadow
        self.isMovable = isMovable
        self.isMovableByWindowBackground = isMovableByWindowBackground
        self.animationBehavior = animationBehavior
        self.startsClickThrough = startsClickThrough
        self.hitRegionOrigin = hitRegionOrigin
    }

    /// Exactly what `RightKitOverlayPanel(contentRect:kind:level:)` builds: a pill joins all
    /// spaces stationary, a notch also joins full-screen spaces; interactive, never key.
    public static func standard(kind: RightKitOverlayKind, level: NSWindow.Level = .statusBar) -> Self {
        Self(level: level, collectionBehavior: kind == .notch ? stationaryFullScreenAuxiliary : stationaryAllSpaces)
    }

    public static let stationaryAllSpaces: NSWindow.CollectionBehavior = [.canJoinAllSpaces, .stationary]
    public static let stationaryFullScreenAuxiliary: NSWindow.CollectionBehavior =
        [.canJoinAllSpaces, .stationary, .fullScreenAuxiliary]
    public static let transientFullScreenAuxiliary: NSWindow.CollectionBehavior =
        [.canJoinAllSpaces, .fullScreenAuxiliary, .transient]
}

/// Native host for pill/notch surfaces. It never activates the app when ordered frontmost.
@MainActor
public final class RightKitOverlayPanel: NSPanel {
    public let overlayKind: RightKitOverlayKind
    public let configuration: RightKitOverlayPanelConfiguration

    public init(contentRect: NSRect, kind: RightKitOverlayKind = .pill, configuration: RightKitOverlayPanelConfiguration) {
        overlayKind = kind
        self.configuration = configuration
        hitRegionOrigin = configuration.hitRegionOrigin
        super.init(contentRect: contentRect, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        // `isFloatingPanel` rewrites `level` (floating or normal), so it goes first and the
        // configured level is applied after it.
        isFloatingPanel = configuration.isFloatingPanel
        level = configuration.level
        collectionBehavior = configuration.collectionBehavior
        isOpaque = false
        backgroundColor = .clear
        hasShadow = configuration.hasShadow
        hidesOnDeactivate = false
        becomesKeyOnlyIfNeeded = configuration.becomesKeyOnlyIfNeeded
        isMovable = configuration.isMovable
        isMovableByWindowBackground = configuration.isMovableByWindowBackground
        animationBehavior = configuration.animationBehavior
        isReleasedWhenClosed = false
        ignoresMouseEvents = configuration.startsClickThrough
    }

    public convenience init(contentRect: NSRect, kind: RightKitOverlayKind = .pill, level: NSWindow.Level = .statusBar) {
        self.init(contentRect: contentRect, kind: kind, configuration: .standard(kind: kind, level: level))
    }

    /// Interactive rectangles in window coordinates, measured from ``hitRegionOrigin``. Empty (the
    /// default) leaves click-through entirely to ``setClickThrough(_:)``; with one or more regions
    /// (or a ``hitShape``), ``updateClickThrough(forMouseAt:)`` makes only those areas take the mouse.
    public var hitRegions: [CGRect] = []

    /// The corner ``hitRegions`` and ``hitShape`` are measured from.
    public var hitRegionOrigin: RightKitOverlayCoordinateOrigin

    /// A live predicate in the region coordinate space (see ``hitRegionOrigin``). When set it
    /// replaces ``hitRegions``; it is evaluated at hit time, so it can read an animating geometry.
    public var hitShape: (@MainActor (CGPoint) -> Bool)?

    /// Sees every event before the window does. Return true to consume it.
    public var eventFilter: (@MainActor (NSEvent) -> Bool)?

    /// Called after the panel resigns key (for hide-on-resign launchers).
    public var onResignKey: (@MainActor () -> Void)?

    /// Whether a ``hitShape`` or any ``hitRegions`` restrict the mouse at all.
    var restrictsMouse: Bool { hitShape != nil || !hitRegions.isEmpty }

    /// The one hit decision: does the point take the mouse? `point` is in lower-left window
    /// coordinates. With neither a shape nor regions set nothing is restricted, so it answers true.
    public func takesMouse(atWindowPoint point: CGPoint) -> Bool {
        guard restrictsMouse else { return true }
        guard point.x.isFinite, point.y.isFinite else { return false }
        if let hitShape {
            switch hitRegionOrigin {
            case .lowerLeft: return hitShape(point)
            case .topLeft: return hitShape(CGPoint(x: point.x, y: frame.height - point.y))
            }
        }
        return RightKitOverlayHitRegions(hitRegions, origin: hitRegionOrigin)
            .contains(windowPoint: point, windowHeight: frame.height)
    }

    /// Makes the whole panel transparent to the mouse (`true`) or interactive (`false`). The
    /// default panel is interactive. Assigning only on change avoids re-sending the window's event
    /// mask to WindowServer when the value is unchanged, which matters when called per mouse event.
    public func setClickThrough(_ enabled: Bool) {
        if ignoresMouseEvents != enabled { ignoresMouseEvents = enabled }
    }

    /// Toggles `ignoresMouseEvents` for a pointer at `screenPoint` (screen coordinates, such as
    /// `NSEvent.mouseLocation`): interactive inside the hit area, click-through elsewhere. Call it
    /// from a global/local mouse-moved monitor, because a click-through window receives no events
    /// of its own. Does nothing while neither ``hitRegions`` nor ``hitShape`` is set.
    /// - Returns: the resulting `ignoresMouseEvents`.
    @discardableResult
    public func updateClickThrough(forMouseAt screenPoint: CGPoint) -> Bool {
        if restrictsMouse {
            setClickThrough(!takesMouse(atWindowPoint: convertPoint(fromScreen: screenPoint)))
        }
        return ignoresMouseEvents
    }

    public override func sendEvent(_ event: NSEvent) {
        if let eventFilter, eventFilter(event) { return }
        super.sendEvent(event)
    }

    public override func resignKey() {
        super.resignKey()
        onResignKey?()
    }

    public override var canBecomeKey: Bool { configuration.canBecomeKey }
    public override var canBecomeMain: Bool { false }

    public func showWithoutActivating() {
        orderFrontRegardless()
    }

    /// Orders the panel front and makes it key. Needs `configuration.canBecomeKey`; without it the
    /// panel is ordered front and simply does not become key. The panel is non-activating, so the
    /// application stays as it was.
    public func showAsKey() {
        makeKeyAndOrderFront(nil)
    }

    public func hide() {
        orderOut(nil)
    }
}

/// Content view for a mostly transparent panel. It never claims a point itself: a point outside
/// the panel's hit area, or one no subview wants, returns nil so the click passes on.
@MainActor
public final class RightKitOverlayContentView: NSView {
    public let content: NSView

    public init(frame: NSRect, hosting content: NSView) {
        self.content = content
        super.init(frame: frame)
        content.frame = bounds
        content.autoresizingMask = [.width, .height]
        addSubview(content)
    }

    @available(*, unavailable)
    public required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    public override func hitTest(_ point: NSPoint) -> NSView? {
        let local = convert(point, from: superview)
        guard bounds.contains(local) else { return nil }
        if let panel = window as? RightKitOverlayPanel {
            let windowPoint = convert(local, to: nil)
            guard panel.takesMouse(atWindowPoint: windowPoint) else { return nil }
        }
        let hit = content.hitTest(local)
        return hit === self ? nil : hit
    }
}

/// Keeps a panel's click-through in step with the pointer: global and local mouse-moved/dragged
/// monitors, plus a slow poll for a pointer that is parked while the window moves under it.
/// Continuous while running; costs one poll per `pollInterval` and one callback per mouse event
/// system-wide. Stops in ``stop()`` and on deinit.
@MainActor
public final class RightKitOverlayPointerTracker {
    private weak var panel: RightKitOverlayPanel?
    private let pollInterval: TimeInterval
    private let onPointer: (@MainActor (CGPoint) -> Void)?
    /// Where the pointer is; replaced by tests so no real pointer is read.
    var locationProvider: @MainActor () -> CGPoint = { NSEvent.mouseLocation }

    nonisolated(unsafe) private var globalMonitor: Any?
    nonisolated(unsafe) private var localMonitor: Any?
    nonisolated(unsafe) private var timer: Timer?

    public init(panel: RightKitOverlayPanel, pollInterval: TimeInterval = 0.3,
                onPointer: (@MainActor (CGPoint) -> Void)? = nil) {
        self.panel = panel
        self.pollInterval = pollInterval
        self.onPointer = onPointer
    }

    deinit {
        if let globalMonitor { NSEvent.removeMonitor(globalMonitor) }
        if let localMonitor { NSEvent.removeMonitor(localMonitor) }
        timer?.invalidate()
    }

    public var isRunning: Bool { timer != nil || globalMonitor != nil || localMonitor != nil }

    var hasGlobalMonitor: Bool { globalMonitor != nil }
    var hasLocalMonitor: Bool { localMonitor != nil }
    var hasTimer: Bool { timer != nil }

    /// Idempotent: a second call while running changes nothing.
    public func start() {
        guard !isRunning else { return }
        let mask: NSEvent.EventTypeMask = [.mouseMoved, .leftMouseDragged]
        let weakSelf = WeakTracker(self)
        globalMonitor = NSEvent.addGlobalMonitorForEvents(matching: mask) { _ in
            MainActor.assumeIsolated { weakSelf.value?.tick() }
        }
        localMonitor = NSEvent.addLocalMonitorForEvents(matching: mask) { event in
            MainActor.assumeIsolated { weakSelf.value?.tick() }
            return event
        }
        let timer = Timer(timeInterval: pollInterval, repeats: true) { _ in
            MainActor.assumeIsolated { weakSelf.value?.tick() }
        }
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
        tick()
    }

    /// Idempotent: removes both monitors and the timer.
    public func stop() {
        if let globalMonitor { NSEvent.removeMonitor(globalMonitor) }
        if let localMonitor { NSEvent.removeMonitor(localMonitor) }
        timer?.invalidate()
        globalMonitor = nil; localMonitor = nil; timer = nil
    }

    /// One evaluation at the current pointer location.
    func tick() {
        guard let panel else { stop(); return }
        let location = locationProvider()
        panel.updateClickThrough(forMouseAt: location)
        onPointer?(location)
    }
}

private final class WeakTracker: @unchecked Sendable {
    weak var value: RightKitOverlayPointerTracker?
    init(_ value: RightKitOverlayPointerTracker) { self.value = value }
}

extension RightKitOverlayDisplays {
    /// The attached screens that have a display id, in `NSScreen.screens` order.
    @MainActor
    public static func current() -> [RightKitOverlayDisplay] {
        NSScreen.screens.compactMap { screen in
            guard let number = screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber
            else { return nil }
            return RightKitOverlayDisplay(id: CGDirectDisplayID(number.uint32Value),
                                          frame: screen.frame, visibleFrame: screen.visibleFrame)
        }
    }
}

extension RightKitOverlayFullScreen {
    /// Is the frontmost app occupying all of `screen`? Apps in `ignoringBundleIDs` (Finder, the
    /// host itself) never count; the host decides what to ignore.
    @MainActor
    public static func frontmostAppIsFullScreen(on screen: NSScreen, ignoringBundleIDs: Set<String> = []) -> Bool {
        guard let frontApp = NSWorkspace.shared.frontmostApplication else { return false }
        if let id = frontApp.bundleIdentifier, ignoringBundleIDs.contains(id) { return false }

        // NSScreen uses AppKit coordinates (origin bottom left of the primary screen);
        // CoreGraphics window bounds use origin top left of the primary screen.
        let primaryHeight = NSScreen.screens.first?.frame.height ?? screen.frame.height
        let cgScreenBounds = CGRect(x: screen.frame.minX, y: primaryHeight - screen.frame.maxY,
                                    width: screen.frame.width, height: screen.frame.height)

        if let infoList = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
            as? [[String: Any]] {
            var windows: [(pid: pid_t, layer: Int, bounds: CGRect)] = []
            for info in infoList {
                guard let pid = info[kCGWindowOwnerPID as String] as? pid_t,
                      let layer = info[kCGWindowLayer as String] as? Int,
                      let dict = info[kCGWindowBounds as String] as? NSDictionary,
                      let bounds = CGRect(dictionaryRepresentation: dict as CFDictionary) else { continue }
                windows.append((pid: pid, layer: layer, bounds: bounds))
            }
            if isFullScreen(screenBounds: cgScreenBounds, frontmostPID: frontApp.processIdentifier,
                            windows: windows, safeAreaTopInset: screen.safeAreaInsets.top) {
                return true
            }
        }

        // On a full-screen Space without a camera notch the menu bar autohides, so visibleFrame
        // matches the whole frame.
        return abs(screen.visibleFrame.width - screen.frame.width) <= 1
            && abs(screen.visibleFrame.height - screen.frame.height) <= 1
    }
}
#endif
