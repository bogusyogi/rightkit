import Foundation

public enum RightKitOverlayKind: String, Sendable, Codable {
    case heardRightPill
    case cockpitNotch
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

#if os(macOS)
import AppKit

/// Native host for pill/notch surfaces. It never activates the app when ordered frontmost.
@MainActor
public final class RightKitOverlayPanel: NSPanel {
    public let overlayKind: RightKitOverlayKind

    public init(contentRect: NSRect, kind: RightKitOverlayKind = .heardRightPill, level: NSWindow.Level = .statusBar) {
        overlayKind = kind
        super.init(contentRect: contentRect, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        self.level = level
        collectionBehavior = kind == .cockpitNotch
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

    public override var canBecomeKey: Bool { false }
    public override var canBecomeMain: Bool { false }

    public func showWithoutActivating() {
        orderFrontRegardless()
    }
}
#endif
