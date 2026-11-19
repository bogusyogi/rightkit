#if os(macOS)
import AppKit
import Foundation

enum PillLaunchPhase: Equatable {
    case idle
    case splashPrepared
    case contracting
    case handedOff

    var blocksNormalPillMutation: Bool {
        self == .splashPrepared || self == .contracting
    }
}

struct PillLaunchFrame: Equatable {
    let elapsedMilliseconds: Int
    let event: String
    let pillSize: NSSize
    let phase: PillLaunchPhase
}

/// The ledger of AppKit-owned pill geometry during launch. Every window `setFrame` or
/// `setFrameOrigin` the pill owns writes here. At most ``capacity`` entries are kept, the oldest
/// dropped first.
final class PillLaunchLifecycleRecorder {
    static let capacity = 256
    private(set) var frames: [PillLaunchFrame] = []

    func reset() { frames.removeAll(keepingCapacity: true) }

    func record(_ event: String, at elapsedMilliseconds: Int, pillSize: NSSize, phase: PillLaunchPhase) {
        if frames.count >= Self.capacity { frames.removeFirst(frames.count - Self.capacity + 1) }
        frames.append(PillLaunchFrame(elapsedMilliseconds: elapsedMilliseconds, event: event, pillSize: pillSize, phase: phase))
    }

    func recordOwnedFrame(_ event: String, frame: NSRect, phase: PillLaunchPhase) {
        guard phase != .idle else { return }
        record(event, at: 0, pillSize: frame.size, phase: phase)
    }
}

/// The launch handoff state machine: splash prepared, contracting into the pill, handed off.
/// While it is `splashPrepared` or `contracting` every ordinary pill mutation is refused.
@MainActor
final class PillLaunchHandoff {
    private(set) var phase: PillLaunchPhase = .idle
    private(set) var requiresPointerMovement = false
    let recorder = PillLaunchLifecycleRecorder()
    private let restingSize: NSSize

    init(restingSize: NSSize) { self.restingSize = restingSize }

    var blocksNormalPillMutation: Bool { phase.blocksNormalPillMutation }

    @discardableResult
    func prepare() -> Bool {
        guard phase == .idle else { return false }
        phase = .splashPrepared
        requiresPointerMovement = true
        recorder.reset()
        recorder.record("splash-start", at: 0, pillSize: restingSize, phase: phase)
        return true
    }

    @discardableResult
    func beginContraction() -> Bool {
        guard phase == .splashPrepared else { return false }
        phase = .contracting
        recorder.record("contraction-start", at: 0, pillSize: restingSize, phase: phase)
        return true
    }

    @discardableResult
    func complete() -> Bool {
        guard phase == .contracting else { return false }
        phase = .handedOff
        recorder.record("handoff", at: 0, pillSize: restingSize, phase: phase)
        return true
    }

    /// Retire a stalled native launch handoff. Callers must clear this ledger, not merely stop
    /// consulting it, or later pill mutations alternate between deferred and allowed behind the
    /// stale phase.
    @discardableResult
    func forceComplete() -> Bool {
        guard phase == .splashPrepared || phase == .contracting else { return false }
        phase = .handedOff
        requiresPointerMovement = false
        recorder.record("handoff-forced", at: 0, pillSize: restingSize, phase: phase)
        return true
    }

    func recordHorizon(frame: NSRect) {
        guard phase == .handedOff else { return }
        recorder.record("post-handoff-horizon", at: 1000, pillSize: frame.size, phase: phase)
    }

    @discardableResult
    func acknowledgePointerMovement() -> Bool {
        guard phase == .handedOff, requiresPointerMovement else { return false }
        requiresPointerMovement = false
        recorder.record("pointer-moved", at: 0, pillSize: restingSize, phase: phase)
        return true
    }

    func recordOwnedFrame(_ event: String, frame: NSRect) {
        recorder.recordOwnedFrame(event, frame: frame, phase: phase)
    }
}
#endif
