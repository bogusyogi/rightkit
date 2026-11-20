#if os(macOS)
import AppKit
import CoreVideo
import Foundation
import QuartzCore

/// 8 ms-tick, generation-guarded animation driver (hub frame interpolation, and the fallback of
/// ``DisplayLinkFrameAnimator``).
///
/// It owns only the timer cadence, the generation guard that invalidates a stale run, and the
/// teardown and completion ordering. Progress, easing, per-tick effects and completion semantics
/// stay with the caller. Main queue only.
final class GenerationGuardedFrameAnimator {
    private var timer: DispatchSourceTimer?
    private(set) var generation = 0

    var isRunning: Bool { timer != nil }

    /// Invalidates the current generation and cancels any in-flight timer. A tick already queued
    /// from an earlier generation becomes a no-op when it sees the bumped generation.
    func cancel() {
        generation &+= 1
        timer?.cancel()
        timer = nil
    }

    deinit { timer?.cancel() }

    /// Starts a run under a freshly bumped generation. `onTick` runs once per tick with the current
    /// media time and returns whether the run is done; then the timer is torn down and
    /// `onComplete` runs. Returns the generation id.
    @discardableResult
    func start(onTick: @escaping @MainActor (_ now: CFTimeInterval) -> Bool, onComplete: @escaping @MainActor () -> Void) -> Int {
        generation &+= 1
        let generation = self.generation
        let timer = DispatchSource.makeTimerSource(queue: .main)
        timer.schedule(deadline: .now(), repeating: .milliseconds(8), leeway: .milliseconds(1))
        timer.setEventHandler { [weak self] in
            MainActor.assumeIsolated {
                guard let self, self.generation == generation else { return }
                let now = CACurrentMediaTime()
                guard onTick(now) else { return }
                guard self.timer != nil else { return }
                self.timer = nil
                timer.cancel()
                onComplete()
            }
        }
        self.timer = timer
        timer.resume()
        return generation
    }
}

/// Same contract as ``GenerationGuardedFrameAnimator``, but ticks come from a `CVDisplayLink`, so
/// frames line up with the display refresh. If no display link can be created it falls back to the
/// 8 ms timer driver so the capsule still settles.
///
/// Main thread only, except the link callback, which records the latest frame time under `lock`
/// and hops to the main queue (one pending hop at a time, so a busy main queue never accumulates ticks).
final class DisplayLinkFrameAnimator: @unchecked Sendable {
    private let lock = NSLock()
    private var link: CVDisplayLink?
    private var latestFrameTime: CFTimeInterval?
    private var drainScheduled = false
    private var onTick: (@MainActor (CFTimeInterval) -> Bool)?
    private var onComplete: (@MainActor () -> Void)?
    private let fallback = GenerationGuardedFrameAnimator()
    private(set) var generation = 0
    private var running = false

    var isRunning: Bool { running }

    func cancel() {
        generation &+= 1
        running = false
        onTick = nil
        onComplete = nil
        fallback.cancel()
        stopLink()
        lock.lock()
        latestFrameTime = nil
        lock.unlock()
    }

    @discardableResult
    func start(onTick: @escaping @MainActor (_ now: CFTimeInterval) -> Bool, onComplete: @escaping @MainActor () -> Void) -> Int {
        cancel()
        generation &+= 1
        let started = generation
        running = true
        guard startLink() else {
            // The fallback owns its own generation; completion is mirrored here.
            fallback.start(onTick: onTick, onComplete: { [weak self] in
                guard let self, self.generation == started else { return }
                self.running = false
                onComplete()
            })
            return started
        }
        self.onTick = onTick
        self.onComplete = onComplete
        return started
    }

    private func startLink() -> Bool {
        if link == nil {
            var newLink: CVDisplayLink?
            guard CVDisplayLinkCreateWithActiveCGDisplays(&newLink) == kCVReturnSuccess,
                  let newLink else { return false }
            let context = Unmanaged.passUnretained(self).toOpaque()
            CVDisplayLinkSetOutputCallback(newLink, { _, _, _, _, _, context in
                guard let context else { return kCVReturnError }
                Unmanaged<DisplayLinkFrameAnimator>.fromOpaque(context).takeUnretainedValue().linkFired()
                return kCVReturnSuccess
            }, context)
            link = newLink
        }
        if let link, !CVDisplayLinkIsRunning(link) { CVDisplayLinkStart(link) }
        return true
    }

    private func stopLink() {
        if let link, CVDisplayLinkIsRunning(link) { CVDisplayLinkStop(link) }
    }

    deinit {
        if let link { CVDisplayLinkStop(link) }
    }

    private func linkFired() {
        lock.lock()
        latestFrameTime = CACurrentMediaTime()
        guard !drainScheduled else {
            lock.unlock()
            return
        }
        drainScheduled = true
        lock.unlock()
        DispatchQueue.main.async { [weak self] in MainActor.assumeIsolated { self?.drain() } }
    }

    @MainActor
    private func drain() {
        lock.lock()
        let frameTime = latestFrameTime
        latestFrameTime = nil
        drainScheduled = false
        lock.unlock()
        guard running, let frameTime, let onTick else { return }
        let activeGeneration = generation
        guard onTick(frameTime), generation == activeGeneration, running else { return }
        let finished = onComplete
        running = false
        self.onTick = nil
        self.onComplete = nil
        stopLink()
        finished?()
    }
}

/// Analytic damped spring for one scalar, velocity preserving. Exact for any time step, so a slow
/// frame cannot blow it up and a retarget keeps momentum. `response` and `damping` come from the host
/// (see ``RightKitPillTimings``).
struct CapsuleSpring {
    var position: Double
    var velocity: Double = 0

    /// Advances toward `target` by `dt` seconds and returns true once at rest.
    mutating func step(toward target: Double, dt: Double, response: Double, damping: Double) -> Bool {
        let omega = 2 * Double.pi / response
        let zeta = damping
        let x = position - target
        let v = velocity
        let decay = exp(-zeta * omega * dt)
        let newX: Double
        let newV: Double
        if zeta >= 0.999 {
            newX = decay * (x * (1 + omega * dt) + v * dt)
            newV = decay * (v * (1 - omega * dt) - x * omega * omega * dt)
        } else {
            let damped = omega * (1 - zeta * zeta).squareRoot()
            let sine = sin(damped * dt)
            let cosine = cos(damped * dt)
            let k = zeta * omega / damped
            newX = decay * (x * (cosine + k * sine) + v * sine / damped)
            newV = decay * (v * (cosine - k * sine) - x * omega * omega / damped * sine)
        }
        position = target + newX
        velocity = newV
        if abs(newX) < 0.4 && abs(newV) < 6 {
            position = target
            velocity = 0
            return true
        }
        return false
    }
}

/// Feeds display-link frame times to the model's level integration, one pending frame at a time.
final class PillLevelDriver: @unchecked Sendable {
    private let lock = NSLock()
    private weak var model: RightKitPillModel?
    private var link: CVDisplayLink?
    // A display link can tick while AppKit is busy with a resize or animation. Keep one latest
    // frame instead of queueing an unbounded series of main-queue blocks.
    private var latestFrameTime: CFTimeInterval?
    private var frameTaskScheduled = false

    init(model: RightKitPillModel) { self.model = model }

    func start() {
        guard link == nil else { return }
        var newLink: CVDisplayLink?
        guard CVDisplayLinkCreateWithActiveCGDisplays(&newLink) == kCVReturnSuccess,
              let newLink else { return }
        let context = Unmanaged.passUnretained(self).toOpaque()
        CVDisplayLinkSetOutputCallback(newLink, { _, _, _, _, _, context in
            guard let context else { return kCVReturnError }
            let driver = Unmanaged<PillLevelDriver>.fromOpaque(context).takeUnretainedValue()
            driver.integrate()
            return kCVReturnSuccess
        }, context)
        link = newLink
        CVDisplayLinkStart(newLink)
    }

    func stop() {
        if let link { CVDisplayLinkStop(link) }
        link = nil
        lock.lock()
        latestFrameTime = nil
        lock.unlock()
    }

    deinit {
        if let link { CVDisplayLinkStop(link) }
    }

    private func integrate() {
        lock.lock()
        latestFrameTime = CACurrentMediaTime()
        guard !frameTaskScheduled else {
            lock.unlock()
            return
        }
        frameTaskScheduled = true
        lock.unlock()
        DispatchQueue.main.async { [weak self] in MainActor.assumeIsolated { self?.drainLatestFrame() } }
    }

    @MainActor
    private func drainLatestFrame() {
        lock.lock()
        let frameTime = latestFrameTime
        latestFrameTime = nil
        lock.unlock()

        if let frameTime {
            model?.integrateLevels(now: frameTime)
        }

        lock.lock()
        let scheduleNext = latestFrameTime != nil
        if !scheduleNext { frameTaskScheduled = false }
        lock.unlock()
        if scheduleNext {
            DispatchQueue.main.async { [weak self] in MainActor.assumeIsolated { self?.drainLatestFrame() } }
        }
    }
}
#endif
