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

/// Pure click-through rule shared by the panel and by hosts that track the mouse themselves.
///
/// A borderless overlay is usually mostly transparent: only a few rectangles (the pill, a card, a
/// tooltip) should take the mouse and everything else should fall through to the app below. AppKit
/// has no per-rectangle hit testing for a window, so a host toggles the window's `ignoresMouseEvents`
/// as the pointer moves. This type is the decision: pointer inside any region means "take the mouse".
public struct RightKitOverlayHitRegions: Sendable, Equatable {
    /// Regions in the panel's window coordinate space (origin at the lower left, as AppKit uses).
    public var regions: [CGRect]

    public init(_ regions: [CGRect] = []) { self.regions = regions }

    /// Per-region toggling is active only while at least one region is set.
    public var isActive: Bool { !regions.isEmpty }

    public func contains(_ point: CGPoint) -> Bool {
        point.x.isFinite && point.y.isFinite && regions.contains { $0.contains(point) }
    }

    /// The value for `ignoresMouseEvents` with the pointer at `point`, or nil when no region is set
    /// and the current state should be left alone.
    public func ignoresMouseEvents(at point: CGPoint) -> Bool? {
        isActive ? !contains(point) : nil
    }
}

#if os(macOS)
import AppKit

/// Native host for pill/notch surfaces. It never activates the app when ordered frontmost.
@MainActor
public final class RightKitOverlayPanel: NSPanel {
    public let overlayKind: RightKitOverlayKind

    public init(contentRect: NSRect, kind: RightKitOverlayKind = .pill, level: NSWindow.Level = .statusBar) {
        overlayKind = kind
        super.init(contentRect: contentRect, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        self.level = level
        collectionBehavior = kind == .notch
            ? [.canJoinAllSpaces, .stationary, .fullScreenAuxiliary]
            : [.canJoinAllSpaces, .stationary]
        isOpaque = false
        backgroundColor = .clear
        hasShadow = false
        hidesOnDeactivate = false
        becomesKeyOnlyIfNeeded = true
        isReleasedWhenClosed = false
        ignoresMouseEvents = false
    }

    /// Interactive rectangles in window coordinates (origin lower left). Empty (the default) leaves
    /// click-through entirely to ``setClickThrough(_:)``; with one or more regions,
    /// ``updateClickThrough(forMouseAt:)`` makes only those regions take the mouse.
    public var hitRegions: [CGRect] = []

    /// Makes the whole panel transparent to the mouse (`true`) or interactive (`false`). The
    /// default panel is interactive. Assigning only on change avoids re-sending the window's event
    /// mask to WindowServer when the value is unchanged, which matters when called per mouse event.
    public func setClickThrough(_ enabled: Bool) {
        if ignoresMouseEvents != enabled { ignoresMouseEvents = enabled }
    }

    /// Toggles `ignoresMouseEvents` for a pointer at `screenPoint` (screen coordinates, such as
    /// `NSEvent.mouseLocation`): interactive inside any ``hitRegions`` rectangle, click-through
    /// elsewhere. Call it from a global/local mouse-moved monitor, because a click-through window
    /// receives no events of its own. Does nothing while ``hitRegions`` is empty.
    /// - Returns: the resulting `ignoresMouseEvents`.
    @discardableResult
    public func updateClickThrough(forMouseAt screenPoint: CGPoint) -> Bool {
        let local = convertPoint(fromScreen: screenPoint)
        if let ignores = RightKitOverlayHitRegions(hitRegions).ignoresMouseEvents(at: local) {
            setClickThrough(ignores)
        }
        return ignoresMouseEvents
    }

    public override var canBecomeKey: Bool { false }
    public override var canBecomeMain: Bool { false }

    public func showWithoutActivating() {
        orderFrontRegardless()
    }
}
#endif
