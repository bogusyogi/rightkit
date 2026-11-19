#if os(macOS)
import AppKit
import Darwin
import Foundation
import QuartzCore
import RightKitOverlay
import SwiftUI

/// Resizes a panel about its bottom centre and reports each frame it set.
@MainActor
final class CenteredPanelFrameCoordinator {
    private weak var panel: NSPanel?
    private let didSetFrame: @MainActor (NSRect, String) -> Void
    private let log: @MainActor (String, String) -> Void

    init(
        panel: NSPanel,
        log: @escaping @MainActor (String, String) -> Void,
        didSetFrame: @escaping @MainActor (NSRect, String) -> Void
    ) {
        self.panel = panel
        self.log = log
        self.didSetFrame = didSetFrame
    }

    func setSize(_ size: NSSize, anchoredAt anchor: NSPoint? = nil, event: String = "set-size") {
        guard let panel else { return }
        let started = CACurrentMediaTime()
        let cpuStarted = clock_gettime_nsec_np(CLOCK_THREAD_CPUTIME_ID)
        let anchorMidX = panel.screen?.frame.midX ?? anchor?.x ?? panel.frame.midX
        let anchorMinY = anchor?.y ?? panel.frame.minY
        panel.setFrame(
            NSRect(x: anchorMidX - size.width / 2, y: anchorMinY, width: size.width, height: size.height),
            display: true
        )
        didSetFrame(panel.frame, event)
        panel.contentView?.updateTrackingAreas()
        let elapsedMs = Int((CACurrentMediaTime() - started) * 1000)
        if elapsedMs >= 16 {
            let cpuMs = (clock_gettime_nsec_np(CLOCK_THREAD_CPUTIME_ID) - cpuStarted) / 1_000_000
            log(
                "pill_set_frame_slow",
                "event=\(event) elapsed_ms=\(elapsedMs) cpu_ms=\(cpuMs) requested=\(Int(size.width))x\(Int(size.height)) final=\(Int(panel.frame.width))x\(Int(panel.frame.height)) main=\(Thread.isMainThread)"
            )
        }
    }
}

/// The pill's content host. It reconciles hover against screen geometry, because resizing a
/// non-activating panel can synthesize mouse-exit events while the pointer stays over the pill.
final class PillHostingView: NSHostingView<AnyView> {
    var onHover: @MainActor (Bool) -> Void = { _ in }
    var onPointerMovement: @MainActor () -> Bool = { false }
    var capsuleSize: @MainActor () -> NSSize = { .zero }
    var watchdogInterval: TimeInterval = 0.05
    private var hoverTrackingArea: NSTrackingArea?
    nonisolated(unsafe) private var hoverWatchdog: Timer?
    private var reportedHover = false
    /// Set by the controller when the panel is ordered out; stops hover polling.
    var panelHidden = false {
        didSet { if panelHidden { reportHover(false) } }
    }
    var hoverWatchdogActive: Bool { hoverWatchdog != nil }

    deinit { hoverWatchdog?.invalidate() }

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func updateTrackingAreas() {
        if let hoverTrackingArea { removeTrackingArea(hoverTrackingArea) }
        let area = NSTrackingArea(
            rect: bounds,
            options: [.activeAlways, .mouseEnteredAndExited, .mouseMoved],
            owner: self,
            userInfo: nil
        )
        addTrackingArea(area)
        hoverTrackingArea = area
        super.updateTrackingAreas()
    }

    override func mouseEntered(with event: NSEvent) {
        super.mouseEntered(with: event)
        updateHover()
    }

    override func mouseMoved(with event: NSEvent) {
        super.mouseMoved(with: event)
        if onPointerMovement() {
            reportedHover = false
        }
        updateHover()
    }

    override func mouseExited(with event: NSEvent) {
        super.mouseExited(with: event)
        // Resizing the nonactivating panel can synthesize mouse-exit events even while the pointer
        // remains over the centered pill. Reconcile against screen geometry so those events cannot
        // collapse and re-expand the pill.
        reconcileHover(at: NSEvent.mouseLocation)
    }

    private func updateHover() { reconcileHover(at: NSEvent.mouseLocation) }

    func reconcileHover(at pointerLocation: NSPoint) {
        // A hidden pill must not keep the hover watchdog alive: the next reconcile after orderOut
        // reports not-hovered, which stops the timer.
        guard let window, !panelHidden else { return reportHover(false) }
        let size = capsuleSize()
        reportHover(Self.capsuleFrame(in: window.frame, size: size).contains(pointerLocation))
    }

    static func capsuleFrame(in windowFrame: NSRect, size: NSSize) -> NSRect {
        NSRect(
            x: windowFrame.midX - size.width / 2,
            y: windowFrame.minY,
            width: size.width,
            height: size.height
        )
    }

    private func reportHover(_ hovered: Bool) {
        guard hovered != reportedHover else { return }
        reportedHover = hovered
        if hovered {
            startHoverWatchdog()
        } else {
            hoverWatchdog?.invalidate()
            hoverWatchdog = nil
        }
        onHover(hovered)
    }

    private func startHoverWatchdog() {
        guard hoverWatchdog == nil else { return }
        let timer = Timer(timeInterval: watchdogInterval, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.reconcileHover(at: NSEvent.mouseLocation) }
        }
        RunLoop.main.add(timer, forMode: .common)
        hoverWatchdog = timer
    }

    /// Stops the watchdog without reporting. Used when the controller shuts down.
    func stopWatching() {
        hoverWatchdog?.invalidate()
        hoverWatchdog = nil
        reportedHover = false
    }
}

@MainActor
final class PillPanelController {
    unowned let runtime: RightKitPillRuntime
    let configuration: RightKitPillConfiguration
    let model: RightKitPillModel
    let launchHandoff: PillLaunchHandoff
    private(set) var panel: RightKitOverlayPanel?
    private var levelDriver: PillLevelDriver?
    private var frameCoordinator: CenteredPanelFrameCoordinator?
    private var screenObserver: NSObjectProtocol?
    private var editingReturnApp: NSRunningApplication?
    private var localMouseMonitor: Any?
    private var globalMouseMonitor: Any?
    private let capsuleAnimator = DisplayLinkFrameAnimator()
    /// Capsule size velocity (points per second) of the spring in flight. It is carried into the
    /// next animation when the target changes mid-flight, and zeroed by anything that cancels the
    /// animation outright.
    private var capsuleVelocity = CGSize.zero
    private(set) var contentReady = false
    private var contentInstallScheduled = false
    private var contentInstallGeneration = 0
    private var deferContentUntilSplashFrame = false
    private var handoffHorizon: DispatchWorkItem?

    private var geometry: RightKitPillGeometry { configuration.geometry }
    private var callbacks: RightKitPillCallbackQueue { runtime.callbacks }
    private var views: RightKitPillViews { runtime.views }

    init(runtime: RightKitPillRuntime) {
        self.runtime = runtime
        self.configuration = runtime.configuration
        self.launchHandoff = PillLaunchHandoff(restingSize: runtime.configuration.geometry.restingSize)
        self.model = RightKitPillModel(
            configuration: runtime.configuration, callbacks: runtime.callbacks,
            views: runtime.views, sounds: runtime.sounds
        )
        model.controller = self
    }

    func install() {
        installShellIfNeeded()
        guard !deferContentUntilSplashFrame, !contentInstallScheduled else { return }
        attachContentIfNeeded()
    }

    func prepareForSplash() {
        deferContentUntilSplashFrame = true
        installShellIfNeeded()
    }

    func allowContentAfterSplashFrame() {
        deferContentUntilSplashFrame = false
        scheduleContentInstallIfAllowed()
    }

    private func installShellIfNeeded() {
        guard panel == nil else { return }
        let windows = configuration.windows
        // The overlay panel applies the transparent, shadowless, non-released panel with the level
        // and collection behaviour the host chose. It never becomes key unless editing asks for it.
        let panel = RightKitOverlayPanel(
            contentRect: NSRect(origin: .zero, size: geometry.restingSize),
            kind: .pill,
            configuration: RightKitOverlayPanelConfiguration(
                level: windows.pillLevel,
                collectionBehavior: windows.pillCollectionBehavior,
                canBecomeKey: false,
                becomesKeyOnlyIfNeeded: true,
                hasShadow: false,
                startsClickThrough: false,
                hidesOnDeactivate: false
            )
        )
        panel.acceptsMouseMovedEvents = true
        panel.setClickThrough(false)

        self.panel = panel
        let handoff = launchHandoff
        frameCoordinator = CenteredPanelFrameCoordinator(
            panel: panel,
            log: { [weak self] surface, message in self?.callbacks.log(surface, message) },
            didSetFrame: { frame, event in handoff.recordOwnedFrame(event, frame: frame) }
        )
        levelDriver = PillLevelDriver(model: model)
        installScreenObserver()
        installClipboardDismissMonitors()
        positionPanel()
        runtime.splash.attachPillIfPresented(self)
    }

    private func scheduleContentInstallIfAllowed() {
        guard panel != nil, !contentReady, !contentInstallScheduled, !deferContentUntilSplashFrame else { return }
        contentInstallScheduled = true
        let generation = contentInstallGeneration
        pillMainAsync { [weak self] in
            guard let self, self.contentInstallGeneration == generation else { return }
            self.contentInstallScheduled = false
            self.attachContentIfNeeded()
        }
    }

    private func attachContentIfNeeded() {
        guard let panel, !contentReady else { return }
        let started = CACurrentMediaTime()
        views.warmContent()
        let root = views.makeRoot(model) { [weak self] size in
            self?.updateContentSize(size)
        }
        let host = PillHostingView(rootView: root)
        host.watchdogInterval = configuration.timings.hoverWatchdogInterval
        host.onHover = { [weak model] hovered in model?.setHover(hovered) }
        host.onPointerMovement = { [weak model] in model?.acknowledgePointerMovement() ?? false }
        host.capsuleSize = { [weak model] in
            guard let model else { return .zero }
            return model.presentationCapsuleSize
        }
        host.sizingOptions = []
        panel.contentView = host
        contentReady = true
        runtime.splash.attachPillIfPresented(self)
        runtime.splash.contentDidBecomeReady()
        if !launchHandoff.blocksNormalPillMutation, model.isVisible {
            panel.alphaValue = 1
            (panel.contentView as? PillHostingView)?.panelHidden = false
            panel.orderFrontRegardless()
        }
        model.syncPresentationIfNeeded()
        callbacks.log(
            "pill_startup_timing",
            "phase=content elapsed_ms=\(Int((CACurrentMediaTime() - started) * 1000))"
        )
    }

    func applyEvent(_ event: RightKitPillEvent, cancellable: Bool) {
        model.push(event, cancellable: cancellable)
        syncLevelDriver()
    }

    /// Stops every timer, monitor, observer and window the controller owns, and detaches the model.
    func shutdown() {
        if runtime.pill === self { runtime.splash.dismiss() }
        contentInstallGeneration += 1
        contentInstallScheduled = false
        deferContentUntilSplashFrame = false
        contentReady = false
        handoffHorizon?.cancel()
        handoffHorizon = nil
        cancelCapsuleAnimation()
        levelDriver?.stop()
        levelDriver = nil
        frameCoordinator = nil
        if let screenObserver { NotificationCenter.default.removeObserver(screenObserver) }
        screenObserver = nil
        if let localMouseMonitor { NSEvent.removeMonitor(localMouseMonitor) }
        if let globalMouseMonitor { NSEvent.removeMonitor(globalMouseMonitor) }
        localMouseMonitor = nil
        globalMouseMonitor = nil
        (panel?.contentView as? PillHostingView)?.stopWatching()
        panel?.orderOut(nil)
        panel?.close()
        panel = nil
        model.shutdown()
        if runtime.pill === self { runtime.pill = nil }
    }

    func showPanel() {
        guard !launchHandoff.blocksNormalPillMutation else { return }
        install()
        guard contentReady else { return }
        (panel?.contentView as? PillHostingView)?.panelHidden = false
        panel?.alphaValue = 1
        panel?.orderFrontRegardless()
    }

    func hidePanel() {
        (panel?.contentView as? PillHostingView)?.panelHidden = true
        panel?.orderOut(nil)
    }

    func prepareSplashDestination() {
        install()
        cancelCapsuleAnimation()
        model.launchExpanded = false
        model.presentationCapsuleSize = geometry.restingSize
        frameCoordinator?.setSize(geometry.restingSize, event: "prepare-size")
        positionPanel()
        panel?.alphaValue = 0
        (panel?.contentView as? PillHostingView)?.panelHidden = false
        panel?.orderFrontRegardless()
    }

    /// Pre-size the hidden native destination so splash contraction and the expanded controls share
    /// one bottom-centre frame before the reveal.
    @discardableResult
    func prepareSplashExpandedDestination() -> NSRect? {
        guard launchHandoff.phase == .contracting, let panel else { return nil }
        let anchor = NSPoint(x: panel.frame.midX, y: panel.frame.minY)
        model.launchExpanded = true
        model.presentationCapsuleSize = geometry.launchExpandedSize
        frameCoordinator?.setSize(geometry.launchExpandedSize, anchoredAt: anchor, event: "expanded-prep")
        return panel.frame
    }

    func completeSplashHandoff(splash: NSPanel) {
        guard launchHandoff.complete() else { return }
        // Completion retires the splash surface after expanded landing and compact collapse have
        // both completed; retaining alpha=1 here is an idempotent safety assignment, not a reveal.
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        splash.alphaValue = 0
        panel?.alphaValue = 1
        (panel?.contentView as? PillHostingView)?.panelHidden = false
        panel?.orderFrontRegardless()
        CATransaction.commit()
        applyDeferredPresentationAfterSplashHandoff()
        handoffHorizon?.cancel()
        let horizon = pillWorkItem { [weak self] in
            guard let self, let panel = self.panel else { return }
            self.handoffHorizon = nil
            self.launchHandoff.recordHorizon(frame: panel.frame)
        }
        handoffHorizon = horizon
        DispatchQueue.main.asyncAfter(deadline: .now() + .seconds(1), execute: horizon)
    }

    /// Terminal fallback for a splash that missed its compositor deadline. Clear the ledger and
    /// reveal the persistent destination in one main-loop turn.
    func forceCompleteSplashHandoff() {
        guard launchHandoff.forceComplete() else { return }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        panel?.alphaValue = 1
        (panel?.contentView as? PillHostingView)?.panelHidden = false
        panel?.orderFrontRegardless()
        CATransaction.commit()
        // Deadline recovery may interrupt while the splash is at expanded landing; compact the
        // native geometry before releasing normal pill mutations.
        cancelCapsuleAnimation()
        model.presentationCapsuleSize = geometry.restingSize
        model.launchExpanded = false
        frameCoordinator?.setSize(geometry.restingSize, event: "handoff-forced-collapse")
        model.completeSplashHandoff()
        applyDeferredPresentationAfterSplashHandoff()
    }

    func beginSingleClipEditing() {
        guard let panel else { return }
        editingReturnApp = NSWorkspace.shared.frontmostApplication
        panel.allowsKey = true
        panel.makeKeyAndOrderFront(nil)
    }

    func endSingleClipEditing() {
        guard let panel else { return }
        model.singleClipEditing = false
        panel.allowsKey = false
        panel.resignKey()
        editingReturnApp?.activate(options: [.activateIgnoringOtherApps])
        editingReturnApp = nil
    }

    func syncLevelDriver() {
        if (model.mode == .recording || model.mode == .processing) && model.isVisible {
            levelDriver?.start()
        } else {
            levelDriver?.stop()
        }
    }

    func prepareClipboardPresentation(_ mode: RightKitPillClipboardMode, items: [RightKitPillClipboardItem] = []) {
        guard !launchHandoff.blocksNormalPillMutation else { return }
        cancelCapsuleAnimation()
        // The card always attaches to the expanded idle shell. A single card can be requested while
        // the processing presentation is still on screen, so deriving this from the current mode
        // would clip the two outside controls.
        model.presentationCapsuleSize = geometry.clipboardCapsuleSize
        if mode == .recent {
            let limit = geometry.recentClipboardWrapCharacterCount
            let wrappedRows = items.prefix(geometry.recentClipboardLimit).filter {
                $0.transcript.contains("\n") || $0.transcript.prefix(limit + 1).count > limit
            }.count
            let size = NSSize(
                width: geometry.recentClipboardSize.width,
                height: geometry.recentClipboardSize.height + CGFloat(wrappedRows) * geometry.recentClipboardWrappedRowHeight
            )
            frameCoordinator?.setSize(size)
        }
    }

    func prepareHintPresentation() {
        guard !launchHandoff.blocksNormalPillMutation else { return }
        cancelCapsuleAnimation()
        endSingleClipEditing()
        // The capsule keeps the size of the mode it is SHOWING. Forcing the resting size here
        // would squash the recording meter and timer into the resting capsule whenever a hint
        // appeared while recording. Hover is already cleared by the caller.
        model.presentationCapsuleSize = capsuleSize()
        model.presentedMode = model.mode
        panel?.contentView?.updateTrackingAreas()
    }

    func transitionHoverPresentation(expanded: Bool, completion: (() -> Void)? = nil) {
        guard !launchHandoff.blocksNormalPillMutation else { return }
        let target = capsuleSize(for: model.mode, hovered: expanded, clipboardVisible: false)
        animateCapsule(to: target, completion: completion)
    }

    /// Returns true only when the capsule was actually resized. The caller uses that to remember
    /// what has been PRESENTED (as opposed to merely requested): a transition refused here must be
    /// retried later, or the pill stays stuck at the previous size. See `presentedMode`.
    @discardableResult
    func transitionModePresentation() -> Bool {
        guard !launchHandoff.blocksNormalPillMutation else {
            callbacks.log("pill_transition_refused", "reason=launch_handoff mode=\(model.mode)")
            return false
        }
        guard !model.clipboardVisible else {
            callbacks.log("pill_transition_refused", "reason=clipboard mode=\(model.mode)")
            return false
        }
        // A visible hint does NOT block mode transitions: with a hint up the window is sized from
        // the content (hint + capsule), and `animateCapsule` only drives the capsule, so the two
        // compose. Refusing here left the pill at the wrong size for the whole recording.
        let target = capsuleSize()
        callbacks.log(
            "pill_transition_attempt",
            "mode=\(model.mode) source=\(Int(model.presentationCapsuleSize.width))x\(Int(model.presentationCapsuleSize.height)) target=\(Int(target.width))x\(Int(target.height))"
        )
        animateCapsule(to: target)
        return true
    }

    private func applyDeferredPresentationAfterSplashHandoff() {
        guard model.mode == .recording else { return }
        let target = capsuleSize()
        model.presentationCapsuleSize = target
        frameCoordinator?.setSize(target, event: "recording-after-handoff")
        model.presentedMode = model.mode
    }

    func restoreCapsulePresentation() {
        guard !launchHandoff.blocksNormalPillMutation else { return }
        cancelCapsuleAnimation()
        let target = capsuleSize()
        model.presentationCapsuleSize = target
        frameCoordinator?.setSize(target)
        // This applies the CURRENT mode's size authoritatively, so the pill is now presenting that
        // mode: record it or the next transition into the same mode would be skipped.
        model.presentedMode = model.mode
    }

    /// While a hint or clip card plays its exit, the window keeps the card's size and only the
    /// capsule inside it moves to the size it will rest at. `finishAuxiliaryRetire` then snaps the
    /// window to the capsule.
    func settleCapsuleWhileAuxiliaryRetires() {
        guard !launchHandoff.blocksNormalPillMutation else { return }
        animateCapsule(to: capsuleSize())
    }

    /// Reconcile the model's hover state with the CURRENT pointer location. Called immediately
    /// before a mode resize: a stale `isHovered` makes the mode resize pick the wrong target and the
    /// pill ends up expanding and then snapping to the other size.
    func reconcileHoverFromPointer() {
        (panel?.contentView as? PillHostingView)?.reconcileHover(at: NSEvent.mouseLocation)
    }

    func positionPanel() {
        guard let panel else { return }
        let mouse = NSEvent.mouseLocation
        let screen = NSScreen.screens.first(where: { NSMouseInRect(mouse, $0.frame, false) })
            ?? NSScreen.main
            ?? NSScreen.screens.first
        guard let screen else { return }
        let visible = screen.visibleFrame
        // The panel grows upward; its bottom stays `bottomInset` points over the Dock.
        let origin = NSPoint(x: screen.frame.midX - panel.frame.width / 2, y: visible.minY + geometry.bottomInset)
        panel.setFrameOrigin(origin)
        launchHandoff.recordOwnedFrame("position", frame: panel.frame)
    }

    private func updateContentSize(_ proposed: CGSize) {
        guard !launchHandoff.blocksNormalPillMutation else { return }
        guard let panel, proposed.width > 0, proposed.height > 0,
              proposed.width.isFinite, proposed.height.isFinite else { return }
        // Pill-only geometry is driven on the same AppKit clock as the window. Delayed content
        // preference values must never resize it a second time.
        guard model.clipboardVisible || model.hint != RightKitPillModel.hiddenHint else { return }
        let size = fittedContentSize(proposed)
        panel.contentView?.updateTrackingAreas()
        guard abs(panel.frame.width - size.width) > 0.5 || abs(panel.frame.height - size.height) > 0.5 else {
            return
        }
        frameCoordinator?.setSize(size)
    }

    /// The content size clamped between the resting size and the host's maxima.
    func fittedContentSize(_ proposed: CGSize) -> NSSize {
        NSSize(
            width: max(geometry.restingSize.width, min(geometry.maxContentWidth, ceil(proposed.width))),
            height: max(geometry.restingSize.height, min(geometry.maxContentHeight, ceil(proposed.height)))
        )
    }

    /// The capsule size for the model's current mode and hover state.
    func capsuleSize() -> NSSize {
        if model.mode == .status || model.mode == .error {
            let measured = views.statusCapsuleSize(model.statusLabel)
            return RightKitPillLimits.size(measured)
        }
        return capsuleSize(for: model.mode, hovered: model.isHovered, clipboardVisible: model.clipboardVisible)
    }

    func capsuleSize(for mode: RightKitPillMode, hovered: Bool, clipboardVisible: Bool) -> NSSize {
        switch mode {
        case .idle: return hovered || clipboardVisible ? geometry.idleExpandedSize : geometry.restingSize
        case .recording: return hovered ? geometry.recordingExpandedSize : geometry.recordingCompactSize
        case .starting, .processing: return geometry.processingSize
        case .status, .error: return geometry.statusFallbackSize
        }
    }

    /// Drives the capsule to `target` with a velocity-preserving spring ticked from the display
    /// link. The window frame still follows the capsule each tick (it is only the capsule's size
    /// while a hint or clip owns the window). A pre-sized window with a click-through margin was
    /// rejected: hover and click handling and the always-on-top panel all key off the window frame,
    /// so a transparent margin would swallow clicks meant for other apps.
    private func animateCapsule(to target: NSSize, completion: (() -> Void)? = nil) {
        let carried = capsuleAnimator.isRunning ? capsuleVelocity : .zero
        cancelCapsuleAnimation()
        let source = model.presentationCapsuleSize
        let anchor = panel.map { NSPoint(x: $0.frame.midX, y: $0.frame.minY) }
        let reduced = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
        guard !reduced, source != target else {
            let immediateStarted = CACurrentMediaTime()
            model.presentationCapsuleSize = target
            if model.windowFollowsCapsule { frameCoordinator?.setSize(target) }
            callbacks.log(
                "pill_animation_end",
                "generation=immediate elapsed_ms=\(Int((CACurrentMediaTime() - immediateStarted) * 1000)) ticks=0 max_tick_gap_ms=0 target=\(Int(target.width))x\(Int(target.height))"
            )
            completion?()
            return
        }

        let response = configuration.timings.capsuleResponse
        let damping = configuration.timings.capsuleDamping
        var width = CapsuleSpring(position: source.width, velocity: carried.width)
        var height = CapsuleSpring(position: source.height, velocity: carried.height)
        let started = CACurrentMediaTime()
        var lastTick = started
        var maxTickGapMs = 0
        var tickCount = 0
        var finalSize = source
        var generation = 0
        generation = capsuleAnimator.start(onTick: { [weak self] now in
            guard let self else { return false }
            let gap = now - lastTick
            maxTickGapMs = max(maxTickGapMs, Int(gap * 1000))
            lastTick = now
            tickCount += 1
            let dt = min(0.05, max(0.0005, gap))
            let widthDone = width.step(toward: target.width, dt: dt, response: response, damping: damping)
            let heightDone = height.step(toward: target.height, dt: dt, response: response, damping: damping)
            self.capsuleVelocity = CGSize(width: width.velocity, height: height.velocity)
            let size = NSSize(width: max(1, width.position), height: max(1, height.position))
            finalSize = size
            let tickWorkStarted = CACurrentMediaTime()
            let tickCpuStarted = clock_gettime_nsec_np(CLOCK_THREAD_CPUTIME_ID)
            self.model.presentationCapsuleSize = size
            let modelUpdated = CACurrentMediaTime()
            if self.model.windowFollowsCapsule {
                self.frameCoordinator?.setSize(size, anchoredAt: anchor)
            }
            let resized = CACurrentMediaTime()
            if resized - tickWorkStarted >= 0.032 {
                let cpuMs = (clock_gettime_nsec_np(CLOCK_THREAD_CPUTIME_ID) - tickCpuStarted) / 1_000_000
                self.callbacks.log(
                    "pill_animation_tick_slow",
                    "generation=\(generation) model_ms=\(Int((modelUpdated - tickWorkStarted) * 1000)) resize_ms=\(Int((resized - modelUpdated) * 1000)) total_ms=\(Int((resized - tickWorkStarted) * 1000)) cpu_ms=\(cpuMs)"
                )
            }
            return widthDone && heightDone
        }, onComplete: { [weak self] in
            self?.capsuleVelocity = .zero
            self?.callbacks.log(
                "pill_animation_end",
                "generation=\(generation) elapsed_ms=\(Int((lastTick - started) * 1000)) ticks=\(tickCount) max_tick_gap_ms=\(maxTickGapMs) final=\(Int(finalSize.width))x\(Int(finalSize.height))"
            )
            completion?()
        })
        callbacks.log(
            "pill_animation_start",
            "generation=\(generation) source=\(Int(source.width))x\(Int(source.height)) target=\(Int(target.width))x\(Int(target.height)) spring_response_ms=\(Int(response * 1000))"
        )
    }

    private func cancelCapsuleAnimation() {
        if capsuleAnimator.isRunning {
            callbacks.log(
                "pill_animation_cancel",
                "generation=\(capsuleAnimator.generation) size=\(Int(model.presentationCapsuleSize.width))x\(Int(model.presentationCapsuleSize.height))"
            )
        }
        capsuleAnimator.cancel()
        capsuleVelocity = .zero
    }

    private func installScreenObserver() {
        screenObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.didChangeScreenParametersNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in MainActor.assumeIsolated { self?.positionPanel() } }
    }

    private func installClipboardDismissMonitors() {
        localMouseMonitor = NSEvent.addLocalMonitorForEvents(matching: .leftMouseDown) { [weak self] event in
            MainActor.assumeIsolated {
                if let self, self.model.clipboardVisible, event.window !== self.panel {
                    self.model.dismissClipboard()
                }
            }
            return event
        }
        globalMouseMonitor = NSEvent.addGlobalMonitorForEvents(matching: .leftMouseDown) { [weak self] _ in
            pillMainAsync {
                guard let self, self.model.clipboardVisible else { return }
                self.model.dismissClipboard()
            }
        }
    }
}
#endif
