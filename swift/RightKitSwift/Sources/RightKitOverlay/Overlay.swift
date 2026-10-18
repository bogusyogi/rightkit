import Foundation
#if canImport(CoreGraphics)
import CoreGraphics
#endif

/// A display identifier. On macOS it is `CGDirectDisplayID`; elsewhere a plain `UInt32` with the same
/// width, so the pure display-plan code compiles on every platform.
#if os(macOS)
public typealias RightKitOverlayDisplayID = CGDirectDisplayID
#else
public typealias RightKitOverlayDisplayID = UInt32
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
    public let id: RightKitOverlayDisplayID
    public let frame: CGRect
    public let visibleFrame: CGRect

    public init(id: RightKitOverlayDisplayID, frame: CGRect, visibleFrame: CGRect) {
        self.id = id; self.frame = frame; self.visibleFrame = visibleFrame
    }
}

public enum RightKitOverlayDisplays {
    /// What to create and tear down to go from the displays you have a surface on (`current`) to
    /// the ones you want one on (`desired`). `hasDuplicates` reports a repeated id in `desired`;
    /// the sets ignore the repeat, and what to do about it stays with the host.
    public static func plan(current: Set<RightKitOverlayDisplayID>, desired: [RightKitOverlayDisplayID])
        -> (add: Set<RightKitOverlayDisplayID>, remove: Set<RightKitOverlayDisplayID>, hasDuplicates: Bool) {
        let wanted = Set(desired)
        return (wanted.subtracting(current), current.subtracting(wanted), wanted.count != desired.count)
    }
}

extension RightKitOverlayDisplays {
    /// ``plan(current:desired:)`` with deterministic arrays, for hosts that create and tear down
    /// surfaces in a defined order: `add` lists the wanted displays that have no surface, **in the
    /// order they were asked for and without repeats**; `remove` lists the surfaces that are no longer
    /// wanted, **sorted ascending by display id**. `hasDuplicates` reports a repeated id in `desired`.
    /// `current` may be a set, an array or any sequence (a repeated id counts once).
    public static func orderedPlan(current: some Sequence<RightKitOverlayDisplayID>, desired: [RightKitOverlayDisplayID])
        -> (add: [RightKitOverlayDisplayID], remove: [RightKitOverlayDisplayID], hasDuplicates: Bool) {
        let have = Set(current)
        var seen = Set<RightKitOverlayDisplayID>()
        var add: [RightKitOverlayDisplayID] = []
        for id in desired where seen.insert(id).inserted && !have.contains(id) { add.append(id) }
        let remove = have.subtracting(seen).sorted()
        return (add, remove, seen.count != desired.count)
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

extension RightKitOverlayFullScreen {
    /// Which of the two full-screen rules apply for a frontmost app, given the host's ignore sets.
    /// An app in `ignoringBundleIDs` skips both; `geometryIgnoring` skips only the window-geometry
    /// rule; `visibleFrameIgnoring` skips only the visible-frame fallback (a Finder window is the
    /// usual case: it may be large, but is not full screen, so the fallback must not count it while
    /// a true full-screen Finder window would still match by geometry).
    public static func rulesToApply(
        frontmostBundleID: String?,
        ignoringBundleIDs: Set<String> = [],
        geometryIgnoring: Set<String> = [],
        visibleFrameIgnoring: Set<String> = []
    ) -> (geometry: Bool, visibleFrame: Bool) {
        guard let id = frontmostBundleID else { return (true, true) }
        if ignoringBundleIDs.contains(id) { return (false, false) }
        return (!geometryIgnoring.contains(id), !visibleFrameIgnoring.contains(id))
    }

    /// The visible-frame fallback: on a full-screen Space without a camera notch the menu bar
    /// autohides, so the visible frame matches the whole frame (within a point).
    public static func visibleFrameCoversScreen(frame: CGRect, visibleFrame: CGRect) -> Bool {
        abs(visibleFrame.width - frame.width) <= 1 && abs(visibleFrame.height - frame.height) <= 1
    }

    /// A window as the geometry rule sees it: owner pid, window layer, CoreGraphics bounds.
    public typealias WindowInfo = (pid: pid_t, layer: Int, bounds: CGRect)

    /// What the main thread knows about the full-screen question, so the blocking window-list call
    /// (``onScreenWindows()``, a WindowServer round trip) can run on any other thread. Build it on the
    /// main actor with ``context(on:ignoringBundleIDs:geometryIgnoring:visibleFrameIgnoring:)``, then
    /// call ``evaluate(_:windows:)`` anywhere.
    public struct Context: Sendable, Equatable {
        /// The screen in CoreGraphics coordinates (origin top left of the primary screen).
        public var screenBounds: CGRect
        public var frontmostPID: pid_t
        public var safeAreaTopInset: CGFloat
        /// Whether the window-geometry rule applies to the frontmost app. When false, no window
        /// list is needed for that rule.
        public var appliesGeometryRule: Bool
        /// The visible-frame fallback already answers yes (menu bar autohidden): no window list is
        /// needed at all when this is true.
        public var visibleFrameCoversScreen: Bool

        public init(screenBounds: CGRect, frontmostPID: pid_t, safeAreaTopInset: CGFloat = 0,
                    appliesGeometryRule: Bool = true, visibleFrameCoversScreen: Bool = false) {
            self.screenBounds = screenBounds
            self.frontmostPID = frontmostPID
            self.safeAreaTopInset = safeAreaTopInset
            self.appliesGeometryRule = appliesGeometryRule
            self.visibleFrameCoversScreen = visibleFrameCoversScreen
        }

        /// True when the answer is already known without reading the window list.
        public var needsWindowList: Bool { !visibleFrameCoversScreen && appliesGeometryRule }
    }

    /// Pure second half of the split: the full-screen answer for `context` and a window list read
    /// on any thread. Matches the one-call ``frontmostAppIsFullScreen(on:ignoringBundleIDs:)``.
    public static func evaluate(_ context: Context, windows: [WindowInfo]) -> Bool {
        if context.visibleFrameCoversScreen { return true }
        guard context.appliesGeometryRule else { return false }
        return isFullScreen(screenBounds: context.screenBounds, frontmostPID: context.frontmostPID,
                            windows: windows, safeAreaTopInset: context.safeAreaTopInset)
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
    /// Whether the panel hides while the application is not active. Defaults to `false`, which is
    /// what the panel has always done; a host whose panel mirrors an `NSPanel` default of `true`
    /// sets it here.
    public var hidesOnDeactivate: Bool
    /// The panel's style mask, applied at creation (AppKit ties the non-activating behaviour to the
    /// window's creation-time tags, so it cannot be changed later through this configuration).
    /// Defaults to ``standardStyleMask`` (`.borderless` + `.nonactivatingPanel`: the app is never
    /// activated). Drop `.nonactivatingPanel` for a panel that activates the app when it becomes key
    /// (an unlock or text-entry panel). Combine with `canBecomeKey: true`.
    public var styleMask: NSWindow.StyleMask

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
        hitRegionOrigin: RightKitOverlayCoordinateOrigin = .lowerLeft,
        hidesOnDeactivate: Bool = false,
        styleMask: NSWindow.StyleMask = RightKitOverlayPanelConfiguration.standardStyleMask
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
        self.hidesOnDeactivate = hidesOnDeactivate
        self.styleMask = styleMask
    }

    /// `.borderless` + `.nonactivatingPanel`, the style the panel has always used.
    public static let standardStyleMask: NSWindow.StyleMask = [.borderless, .nonactivatingPanel]

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
///
/// The class is `open` so a host can subclass it (for its own window overrides, such as a custom
/// `constrainFrameRect` or `accessibilityRole`) and keep the configuration, hit-region and click-through
/// machinery. ``canBecomeKey`` is `open` and answers ``allowsKey`` (seeded from the configuration, settable at
/// runtime); ``canBecomeMain`` stays `final` and false. The event
/// overrides (`sendEvent`, the mouse handlers, `resignKey`) are `open`; an override must call `super` or
/// the filter and unhandled-mouse hooks stop working.
@MainActor
open class RightKitOverlayPanel: NSPanel {
    public let overlayKind: RightKitOverlayKind
    public let configuration: RightKitOverlayPanelConfiguration

    /// Whether the panel may become key right now. Starts as `configuration.canBecomeKey` and may be
    /// changed at any time (a clipboard strip that takes the keyboard only while its text field is
    /// being edited). Turning it off while the panel is key does not resign key by itself; call
    /// `resignKey()` through AppKit (`orderOut`, or make another window key) if the host needs that.
    public var allowsKey: Bool

    public init(contentRect: NSRect, kind: RightKitOverlayKind = .pill, configuration: RightKitOverlayPanelConfiguration) {
        overlayKind = kind
        self.configuration = configuration
        hitRegionOrigin = configuration.hitRegionOrigin
        allowsKey = configuration.canBecomeKey
        super.init(contentRect: contentRect, styleMask: configuration.styleMask, backing: .buffered, defer: false)
        // `isFloatingPanel` rewrites `level` (floating or normal), so it goes first and the
        // configured level is applied after it.
        isFloatingPanel = configuration.isFloatingPanel
        level = configuration.level
        collectionBehavior = configuration.collectionBehavior
        isOpaque = false
        backgroundColor = .clear
        hasShadow = configuration.hasShadow
        hidesOnDeactivate = configuration.hidesOnDeactivate
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

    /// Mouse events that **no view took**. Unlike ``eventFilter`` (which sees every event before any
    /// view and would pre-empt SwiftUI buttons), this runs only for a press, drag or release that
    /// reached the window itself: hit-testing found no view with a handler, so the event travelled the
    /// responder chain up to the panel's own `mouseDown`, `mouseDragged`, `mouseUp` or `rightMouseDown`.
    /// A host's click, Option-drag and grip-drag logic belongs here. The handler may run a nested
    /// event loop (`nextEvent(matching:)`) from inside the call, as a window-level `mouseDown`
    /// override can. Return true when the event was handled; false lets it continue up the chain.
    ///
    /// A subclass may override the mouse handlers and ``canBecomeKey``; this hook is the way in
    /// that needs no subclass.
    public var unhandledMouseEvent: (@MainActor (NSEvent) -> Bool)?

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

    open override func sendEvent(_ event: NSEvent) {
        if let eventFilter, eventFilter(event) { return }
        super.sendEvent(event)
    }

    open override func mouseDown(with event: NSEvent) {
        if let unhandledMouseEvent, unhandledMouseEvent(event) { return }
        super.mouseDown(with: event)
    }

    open override func mouseDragged(with event: NSEvent) {
        if let unhandledMouseEvent, unhandledMouseEvent(event) { return }
        super.mouseDragged(with: event)
    }

    open override func mouseUp(with event: NSEvent) {
        if let unhandledMouseEvent, unhandledMouseEvent(event) { return }
        super.mouseUp(with: event)
    }

    open override func rightMouseDown(with event: NSEvent) {
        if let unhandledMouseEvent, unhandledMouseEvent(event) { return }
        super.rightMouseDown(with: event)
    }

    open override func resignKey() {
        super.resignKey()
        onResignKey?()
    }

    /// Answers ``allowsKey``. `open`: a subclass may override it (call `super` to keep the runtime
    /// flag, or replace the rule entirely).
    open override var canBecomeKey: Bool { allowsKey }
    public final override var canBecomeMain: Bool { false }

    public func showWithoutActivating() {
        orderFrontRegardless()
    }

    /// Orders the panel front and makes it key. Needs ``allowsKey``; without it the
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
    /// False for a tracker built without a panel: it then runs until stopped instead of stopping
    /// itself when the (absent) panel is gone.
    private let tracksPanel: Bool
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
        self.tracksPanel = true
    }

    /// A tracker with no panel: `onPointer` receives the pointer location (screen coordinates) on
    /// every mouse-moved/dragged event and every poll, and the host does whatever it needs there, such
    /// as toggling `ignoresMouseEvents` on a panel that is not a ``RightKitOverlayPanel``, or driving
    /// hover and fold state. Same monitors, timer, lifecycle and cost as the panel form; runs until
    /// ``stop()`` or deinit.
    public init(pollInterval: TimeInterval = 0.3, onPointer: @escaping @MainActor (CGPoint) -> Void) {
        self.panel = nil
        self.pollInterval = pollInterval
        self.onPointer = onPointer
        self.tracksPanel = false
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
        if tracksPanel, panel == nil { stop(); return }
        let location = locationProvider()
        panel?.updateClickThrough(forMouseAt: location)
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
        frontmostAppIsFullScreen(on: Optional(screen), ignoringBundleIDs: ignoringBundleIDs,
                                 geometryIgnoring: [], visibleFrameIgnoring: [])
    }

    /// As ``frontmostAppIsFullScreen(on:ignoringBundleIDs:)`` with finer control and a missing
    /// screen allowed.
    ///
    /// - `screen`: nil falls back to `NSScreen.main`, then the first screen; false when there is none.
    /// - `ignoringBundleIDs`: the frontmost app never counts (both rules skipped).
    /// - `geometryIgnoring`: skip only the window-geometry rule for these apps.
    /// - `visibleFrameIgnoring`: skip only the visible-frame fallback for these apps, so a large
    ///   Finder window does not count by the fallback while a genuinely full-screen one still
    ///   matches by geometry.
    @MainActor
    public static func frontmostAppIsFullScreen(
        on screen: NSScreen?,
        ignoringBundleIDs: Set<String> = [],
        geometryIgnoring: Set<String>,
        visibleFrameIgnoring: Set<String>
    ) -> Bool {
        guard let ctx = context(on: screen, ignoringBundleIDs: ignoringBundleIDs,
                                geometryIgnoring: geometryIgnoring, visibleFrameIgnoring: visibleFrameIgnoring)
        else { return false }
        return evaluate(ctx, windows: ctx.needsWindowList ? onScreenWindows() : [])
    }

    /// Main-actor half of the off-main split: reads `NSScreen` and `NSWorkspace` facts only (no
    /// WindowServer window list). Nil means "not full screen" (no screen, or no frontmost app).
    /// The ignore sets mean what they do in ``rulesToApply(frontmostBundleID:ignoringBundleIDs:geometryIgnoring:visibleFrameIgnoring:)``;
    /// an app in `ignoringBundleIDs` (the host itself, for one) yields a context that answers false.
    /// Pass the result to ``evaluate(_:windows:)`` together with ``onScreenWindows()`` run elsewhere.
    @MainActor
    public static func context(
        on screen: NSScreen?,
        ignoringBundleIDs: Set<String> = [],
        geometryIgnoring: Set<String> = [],
        visibleFrameIgnoring: Set<String> = []
    ) -> Context? {
        guard let screen = screen ?? NSScreen.main ?? NSScreen.screens.first else { return nil }
        guard let frontApp = NSWorkspace.shared.frontmostApplication else { return nil }
        let rules = rulesToApply(frontmostBundleID: frontApp.bundleIdentifier, ignoringBundleIDs: ignoringBundleIDs,
                                 geometryIgnoring: geometryIgnoring, visibleFrameIgnoring: visibleFrameIgnoring)
        // NSScreen uses AppKit coordinates (origin bottom left of the primary screen);
        // CoreGraphics window bounds use origin top left of the primary screen.
        let primaryHeight = NSScreen.screens.first?.frame.height ?? screen.frame.height
        let cgScreenBounds = CGRect(x: screen.frame.minX, y: primaryHeight - screen.frame.maxY,
                                    width: screen.frame.width, height: screen.frame.height)
        return Context(
            screenBounds: cgScreenBounds,
            frontmostPID: frontApp.processIdentifier,
            safeAreaTopInset: screen.safeAreaInsets.top,
            appliesGeometryRule: rules.geometry,
            visibleFrameCoversScreen: rules.visibleFrame
                && visibleFrameCoversScreen(frame: screen.frame, visibleFrame: screen.visibleFrame))
    }

    /// Second half of the split's input: the on-screen windows. A blocking WindowServer call that
    /// is safe on any thread; call it off the main thread.
    public static func onScreenWindows() -> [WindowInfo] {
        var windows: [WindowInfo] = []
        guard let infoList = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
            as? [[String: Any]] else { return windows }
        for info in infoList {
            guard let pid = info[kCGWindowOwnerPID as String] as? pid_t,
                  let layer = info[kCGWindowLayer as String] as? Int,
                  let dict = info[kCGWindowBounds as String] as? NSDictionary,
                  let bounds = CGRect(dictionaryRepresentation: dict as CFDictionary) else { continue }
            windows.append((pid: pid, layer: layer, bounds: bounds))
        }
        return windows
    }
}
#endif
