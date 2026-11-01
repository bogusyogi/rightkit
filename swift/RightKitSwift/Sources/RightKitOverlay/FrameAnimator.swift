#if os(macOS)
import AppKit
import QuartzCore

/// Base for the app's own AppKit panels that must never take key or main
/// window status away from the application receiving dictation or focus.
/// `HeardRightPillPanel` overrides `canBecomeKey` again on top of this to add
/// its narrow, explicitly-opted-in editing-focus exception.
class NonActivatingPanel: NSPanel {
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

/// Shared 8ms-tick, generation-guarded animation driver used by
/// `PillPanelController.animateCapsule` (capsule resize) and
/// `HubTransitionController.startFrameTransition` (hub frame interpolation).
///
/// This owns only the timer cadence, the generation guard that invalidates a
/// stale run, and the teardown/completion ordering. Progress math, easing,
/// per-tick side effects (resizing a panel, updating model state, logging)
/// and completion semantics all stay with the caller, so each call site keeps
/// its own exact behaviour.
final class GenerationGuardedFrameAnimator {
    private var timer: DispatchSourceTimer?
    private(set) var generation = 0

    /// True while a run started by `start` is still ticking (i.e. has not yet
    /// reported completion via `onTick` returning `true`, and has not been
    /// cancelled).
    var isRunning: Bool { timer != nil }

    /// Invalidates the current generation and cancels any in-flight timer.
    /// A tick from a prior generation that is already queued on the main
    /// queue becomes a no-op once it observes the bumped generation.
    func cancel() {
        generation &+= 1
        timer?.cancel()
        timer = nil
    }

    /// Starts a new 8ms-tick run under a freshly bumped generation. `onTick`
    /// runs once per tick (already generation-guarded) with the current
    /// `CACurrentMediaTime()`; it owns its own progress/easing math and
    /// per-tick side effects, and reports back whether the run is done by
    /// returning `true`. Once `onTick` reports completion the timer is torn
    /// down and `onComplete()` runs. Returns the new generation id.
    @discardableResult
    func start(onTick: @escaping (_ now: CFTimeInterval) -> Bool, onComplete: @escaping () -> Void) -> Int {
        generation &+= 1
        let generation = self.generation
        let timer = DispatchSource.makeTimerSource(queue: .main)
        timer.schedule(deadline: .now(), repeating: .milliseconds(8), leeway: .milliseconds(1))
        timer.setEventHandler { [weak self] in
            guard let self, self.generation == generation else { return }
            let now = CACurrentMediaTime()
            guard onTick(now) else { return }
            guard self.timer != nil else { return }
            self.timer = nil
            timer.cancel()
            onComplete()
        }
        self.timer = timer
        timer.resume()
        return generation
    }
}
#endif
