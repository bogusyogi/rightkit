#if os(macOS)
import AppKit
import Foundation
import QuartzCore
import RightKitOverlay
import SwiftUI

/// The splash's observable state: the loader step, the message ladder and the stall flag. The
/// words come from ``RightKitPillSplashConfiguration`` and the host.
@MainActor
public final class RightKitSplashModel: ObservableObject {
    @Published public var step = 0
    @Published public var message: String
    @Published public var stalled = false
    @Published public var note: String?
    private var timer: Timer?
    private var started = Date()
    private var lastPoll = Date.distantPast
    private let configuration: RightKitPillSplashConfiguration
    private let callbacks: RightKitPillCallbackQueue
    private let actionNote: @MainActor (Int32) -> String?

    init(
        configuration: RightKitPillSplashConfiguration, callbacks: RightKitPillCallbackQueue,
        actionNote: @escaping @MainActor (Int32) -> String?
    ) {
        self.configuration = configuration
        self.callbacks = callbacks
        self.actionNote = actionNote
        self.message = configuration.initialMessage
    }

    /// Starts the one repeating timer (`tickInterval`). Restarting replaces it.
    func start() {
        timer?.invalidate()
        started = Date()
        lastPoll = Date.distantPast
        step = 0
        message = configuration.initialMessage
        stalled = false
        note = nil
        let steps = configuration.stepCount
        timer = Timer.scheduledTimer(withTimeInterval: configuration.tickInterval, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.step = (self.step + 1) % steps
                let waited = Date().timeIntervalSince(self.started)
                self.pollStall(waited: waited)
                if let next = self.configuration.progressMessages.last(where: { waited >= $0.afterSeconds }) {
                    self.message = next.text
                }
            }
        }
    }

    func stop() { timer?.invalidate(); timer = nil }

    /// Stall recovery: the host decides (failed, or no progress for a while) and is asked about
    /// every `stallPollInterval`. Never reveals the app, only offers controls.
    private func pollStall(waited: TimeInterval) {
        guard Date().timeIntervalSince(lastPoll) >= configuration.stallPollInterval else { return }
        lastPoll = Date()
        stalled = callbacks.splashStalled(waitedSecs: Int(min(waited, 86_400)))
    }

    /// Runs a stall action the host defined (an opaque code) and shows the host's note for it.
    public func perform(action: Int32) {
        note = actionNote(action)
        callbacks.splashAction(action)
    }
}

@MainActor
final class SplashPanelController {
    unowned let runtime: RightKitPillRuntime
    let configuration: RightKitPillSplashConfiguration
    let model: RightKitSplashModel
    private var panel: RightKitOverlayPanel?
    private var shownAt: Date?
    private var pendingFly: DispatchWorkItem?
    private var contractionWatchdog: DispatchWorkItem?
    private var resignObserver: NSObjectProtocol?
    private var handoffFinalized = false
    private var flyPendingForContent = false
    private var startupGeneration = 0
    private var hostGeneration: UInt64?
    private(set) var preparingFirstFrame = false
    /// True only once `beginContraction()` has actually run: the engine reported warm and the host
    /// asked for the fly-in. Both the deadline force-complete below and the host's backstop must be
    /// no-ops before this is true: forcing completion earlier tears down the splash and reveals the
    /// pill while the engine is still not ready to record.
    private var contractionStarted = false
    /// Independent wall-clock backstop for the handoff query. Core Animation on a non-activating
    /// panel can stall entirely once the app loses focus (a 900 ms contraction took 27 s in
    /// production), and during that stall the stored phase alone kept every pill mutation deferred.
    /// This timestamp lets the bridge answer "not active" purely from elapsed time.
    private(set) var contractionDeadline: Date?

    private var callbacks: RightKitPillCallbackQueue { runtime.callbacks }
    private var views: RightKitPillViews { runtime.views }

    init(runtime: RightKitPillRuntime) {
        self.runtime = runtime
        self.configuration = runtime.configuration.splash
        self.model = RightKitSplashModel(
            configuration: runtime.configuration.splash, callbacks: runtime.callbacks,
            actionNote: { [views = runtime.views] action in views.splashActionNote(action) }
        )
    }

    var handoffDeadlineExceeded: Bool {
        guard let contractionDeadline else { return false }
        return Date() >= contractionDeadline
    }

    /// Force the current handoff ledger to its terminal state exactly once.
    ///
    /// This is a watchdog for a STALLED contraction animation, never a substitute for engine
    /// readiness: it must not fire before `beginContraction()` has started. It is reachable both
    /// from this file's own deadline check and directly from the host's backstop, which has no
    /// visibility into this state, so the guard belongs here too.
    func forceCompleteHandoffAtDeadline() {
        let hostGeneration = self.hostGeneration
        guard !handoffFinalized else { return }
        guard contractionStarted else {
            callbacks.log("splash_lifecycle", "phase=handoff_force_skipped reason=engine_not_ready")
            return
        }
        if let splash = panel {
            let fallbackTarget = runtime.pill?.panel.map { collapsedPillTarget(from: $0.frame) } ?? splash.frame
            callbacks.log("splash_lifecycle", "phase=handoff_forced reason=deadline")
            forceCompleteHandoff(panel: splash, target: fallbackTarget, generation: startupGeneration)
            return
        }

        // A prepared destination can outlive its splash callback during slow startup. Clear that
        // state even when there is no panel to snap.
        pendingFly?.cancel(); pendingFly = nil
        contractionWatchdog?.cancel(); contractionWatchdog = nil
        removeResignObserver()
        contractionDeadline = nil
        model.stop()
        handoffFinalized = true
        runtime.pill?.forceCompleteSplashHandoff()
        callbacks.log("splash_lifecycle", "phase=handoff_forced reason=deadline")
        callbacks.splashClosed(generation: hostGeneration)
    }

    func remainingVisibleDuration(shownAt: Date, now: Date) -> TimeInterval {
        max(0, configuration.minimumVisibleDuration - now.timeIntervalSince(shownAt))
    }

    func collapsedPillTarget(from frame: NSRect) -> NSRect {
        let resting = runtime.configuration.geometry.restingSize
        return NSRect(x: frame.midX - resting.width / 2, y: frame.minY, width: resting.width, height: resting.height)
    }

    static func centeredFrame(size: NSSize, in screenFrame: NSRect) -> NSRect {
        NSRect(
            x: screenFrame.midX - size.width / 2,
            y: screenFrame.midY - size.height / 2,
            width: size.width,
            height: size.height
        )
    }

    /// No shadow, and the rounded backdrop clips the content to its viewport. Without the layer
    /// mask, the fixed-size splash content can paint through the tiny final frame as a hard
    /// rectangular box while it contracts.
    func disableContractionShadow(on panel: NSPanel) {
        panel.hasShadow = false
        guard let content = panel.contentView else { return }
        let backdrop = views.splashBackdrop()
        content.wantsLayer = true
        content.layer?.cornerRadius = backdrop.cornerRadius
        content.layer?.masksToBounds = true
        content.layer?.backgroundColor = backdrop.color.cgColor
    }

    /// Keep the rounded backdrop as the persistent object, but remove all splash copy and loader
    /// pixels before the first moving frame.
    static func hideContractionContent(on panel: NSPanel) {
        panel.contentView?.subviews.forEach { $0.alphaValue = 0 }
    }

    var canShow: Bool {
        panel == nil && !preparingFirstFrame && runtime.pill?.launchHandoff.phase != .handedOff
    }

    func ownsActiveSplash(generation: UInt64) -> Bool {
        hostGeneration == generation && (panel != nil || preparingFirstFrame)
    }

    @discardableResult
    func show(generation: UInt64? = nil) -> Bool {
        guard canShow, let controller = runtime.pill else { return false }
        hostGeneration = generation
        preparingFirstFrame = true
        contractionStarted = false
        let generation = startupGeneration
        attachPillIfPresented(controller)
        // Published destination changes can synchronously dismiss or replace this launch.
        // Its teardown owns the flags; leave them intact when this admission was superseded.
        guard startupGeneration == generation, preparingFirstFrame,
              runtime.pill === controller else { return false }
        let warm = views.warmSplash
        let log = callbacks
        // Separate AppKit's cold window setup, host view construction and first layout so
        // input and watchdog callbacks can run between stages.
        DispatchQueue.global(qos: .userInitiated).async {
            let started = CACurrentMediaTime()
            warm()
            log.log(
                "splash_startup_timing",
                "phase=text_prepared background=true elapsed_ms=\(Int((CACurrentMediaTime() - started) * 1000))"
            )
            pillMainAsync { [weak self, weak controller] in
                guard let self, let controller, self.startupGeneration == generation,
                      self.runtime.pill === controller else { return }
                let started = CACurrentMediaTime()
                self.create()
                self.callbacks.log("splash_startup_timing", "phase=create elapsed_ms=\(Int((CACurrentMediaTime() - started) * 1000))")
                pillMainAsync { [weak self, weak controller] in
                    guard let self, let controller, self.startupGeneration == generation,
                          self.runtime.pill === controller else { return }
                    self.presentFirstFrame()
                    self.preparingFirstFrame = false
                    controller.allowContentAfterSplashFrame()
                }
            }
        }
        return true
    }

    private func presentFirstFrame() {
        let started = CACurrentMediaTime()
        model.start()
        let mouse = NSEvent.mouseLocation
        let screen = NSScreen.screens.first(where: { NSMouseInRect(mouse, $0.frame, false) })
            ?? NSScreen.main
            ?? NSScreen.screens.first
        if let panel, let screen {
            panel.setFrame(Self.centeredFrame(size: panel.frame.size, in: screen.frame), display: false)
        }
        shownAt = Date()
        panel?.orderFrontRegardless()
        commitFirstFrame()
        callbacks.log("splash_lifecycle", "phase=shown size=\(Int(configuration.size.width))x\(Int(configuration.size.height))")
        callbacks.log("splash_startup_timing", "phase=first_frame elapsed_ms=\(Int((CACurrentMediaTime() - started) * 1000))")
    }

    /// Commit splash pixels before deferred pill-host construction starts. `orderFront` queues
    /// AppKit work; forcing display and a Core Animation flush makes the first frame observable
    /// before the next main-loop turn does view work.
    func commitFirstFrame() {
        guard let panel else { return }
        let started = CACurrentMediaTime()
        panel.displayIfNeeded()
        panel.contentView?.displayIfNeeded()
        CATransaction.flush()
        callbacks.log(
            "splash_lifecycle",
            "phase=first_frame_committed elapsed_ms=\(Int((CACurrentMediaTime() - started) * 1000))"
        )
    }

    func contentDidBecomeReady() {
        guard flyPendingForContent else { return }
        flyPendingForContent = false
        flyIntoPill()
    }

    /// Used when a required downloadable runtime is missing. This is not a handoff: no pill may be
    /// revealed until the download and later prewarm finish. Stops every timer, observer and the
    /// window the splash owns.
    func dismiss() {
        let wasPreparing = preparingFirstFrame
        startupGeneration += 1
        preparingFirstFrame = false
        pendingFly?.cancel(); pendingFly = nil
        flyPendingForContent = false
        contractionWatchdog?.cancel(); contractionWatchdog = nil
        removeResignObserver()
        contractionDeadline = nil
        model.stop()
        panel?.orderOut(nil); panel?.close()
        panel = nil
        shownAt = nil
        handoffFinalized = false
        contractionStarted = false
        if wasPreparing { runtime.pill?.allowContentAfterSplashFrame() }
        callbacks.log("splash_lifecycle", "phase=dismissed reason=required_runtime_download")
    }

    @discardableResult
    func attachPillIfPresented() -> Bool {
        attachPillIfPresented(runtime.pill)
    }

    @discardableResult
    func attachPillIfPresented(_ controller: PillPanelController?) -> Bool {
        guard panel != nil || preparingFirstFrame, let controller else { return false }
        switch controller.launchHandoff.phase {
        case .idle:
            let prepared = controller.model.prepareForSplashHandoff()
            if prepared {
                let resting = runtime.configuration.geometry.restingSize
                callbacks.log("splash_lifecycle", "phase=destination_prepared size=\(Int(resting.width))x\(Int(resting.height)) alpha=0")
            }
            return prepared
        case .splashPrepared, .contracting:
            return true
        case .handedOff:
            return false
        }
    }

    func flyIntoPill() {
        let hostGeneration = self.hostGeneration
        if preparingFirstFrame {
            flyPendingForContent = true
            return
        }
        attachPillIfPresented()
        guard let panel else {
            callbacks.log("splash_lifecycle", "phase=contraction_skipped reason=missing_panel")
            if !handoffFinalized { callbacks.splashClosed(generation: hostGeneration) }
            return
        }
        guard let controller = runtime.pill else {
            callbacks.log("splash_lifecycle", "phase=contraction_skipped reason=missing_controller")
            return
        }
        guard controller.contentReady else {
            flyPendingForContent = true
            callbacks.log("splash_lifecycle", "phase=contraction_deferred reason=content_pending")
            return
        }
        guard let shownAt else {
            callbacks.log("splash_lifecycle", "phase=contraction_skipped reason=missing_shown_time")
            return
        }
        let remaining = remainingVisibleDuration(shownAt: shownAt, now: Date())
        if remaining > 0 {
            guard pendingFly == nil else {
                callbacks.log("splash_lifecycle", "phase=contraction_skipped reason=already_deferred")
                return
            }
            callbacks.log(
                "splash_lifecycle",
                "phase=contraction_deferred remaining_ms=\(Int((remaining * 1000).rounded(.up)))"
            )
            let generation = startupGeneration
            let work = pillWorkItem { [weak self, weak panel] in
                guard let self, let panel, self.startupGeneration == generation,
                      self.panel === panel else { return }
                self.pendingFly = nil
                self.beginContraction()
            }
            pendingFly = work
            DispatchQueue.main.asyncAfter(deadline: .now() + remaining, execute: work)
            return
        }
        beginContraction()
    }

    private func beginContraction() {
        attachPillIfPresented()
        guard let panel else {
            callbacks.log("splash_lifecycle", "phase=contraction_skipped reason=missing_panel")
            return
        }
        guard runtime.pill?.launchHandoff.beginContraction() == true else {
            callbacks.log("splash_lifecycle", "phase=contraction_skipped reason=invalid_state")
            return
        }
        model.stop()
        guard let targetPanel = runtime.pill?.panel else {
            callbacks.log("splash", "fly ignored: pill not initialized")
            return
        }
        let target = collapsedPillTarget(from: targetPanel.frame)
        let duration = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion ? 0 : configuration.contractionDuration
        disableContractionShadow(on: panel)
        Self.hideContractionContent(on: panel)
        callbacks.log(
            "splash_lifecycle",
            "phase=contraction_started duration_ms=\(Int(duration * 1000)) target=\(Int(target.width))x\(Int(target.height))"
        )
        handoffFinalized = false
        contractionStarted = true
        contractionDeadline = Date().addingTimeInterval(configuration.handoffDeadline)
        let generation = startupGeneration
        // AppKit callbacks carry identity only. Resolve the panel on the main actor.
        let panelID = ObjectIdentifier(panel)
        armResignObserver(panelID: panelID, generation: generation)
        let watchdog = pillWorkItem { [weak self] in
            guard let self, self.isCurrentHandoff(panel: panel, generation: generation) else { return }
            self.callbacks.log("splash_lifecycle", "phase=handoff_forced reason=watchdog")
            self.forceCompleteHandoff(panel: panel, target: target, generation: generation)
        }
        contractionWatchdog = watchdog
        DispatchQueue.main.asyncAfter(deadline: .now() + duration + configuration.contractionWatchdogSlack, execute: watchdog)
        let points = configuration.contractionTimingControlPoints
        NSAnimationContext.runAnimationGroup { context in
            context.duration = duration
            context.timingFunction = CAMediaTimingFunction(controlPoints: points[0], points[1], points[2], points[3])
            panel.animator().setFrame(target, display: true)
        } completionHandler: { [weak self] in
            MainActor.assumeIsolated {
                guard let self, let panel = self.panel, ObjectIdentifier(panel) == panelID else { return }
                self.finalizeHandoff(panel: panel, generation: generation)
            }
        }
    }

    private func isCurrentHandoff(panel: RightKitOverlayPanel, generation: Int) -> Bool {
        startupGeneration == generation && self.panel === panel && contractionStarted && !handoffFinalized
    }

    /// Runs exactly once per contraction regardless of which of the three completion paths (real
    /// animation completion, watchdog timeout, resign snap) reaches it first. A late real
    /// completion arriving after a forced finish is a no-op.
    private func finalizeHandoff(panel: RightKitOverlayPanel, generation: Int) {
        guard isCurrentHandoff(panel: panel, generation: generation) else { return }
        let hostGeneration = self.hostGeneration
        handoffFinalized = true
        contractionWatchdog?.cancel()
        contractionWatchdog = nil
        contractionDeadline = nil
        removeResignObserver()
        if runtime.pill?.launchHandoff.phase == .contracting {
            runtime.pill?.completeSplashHandoff(splash: panel)
            runtime.pill?.model.completeSplashHandoff()
        } else {
            // Watchdog, deadline or resign recovery may fire while the ledger is still
            // `.splashPrepared`; force-complete clears either blocking state.
            runtime.pill?.forceCompleteSplashHandoff()
        }
        callbacks.log("splash_lifecycle", "phase=handoff_complete splash_alpha=0 pill_alpha=1")
        panel.orderOut(nil); panel.close()
        self.shownAt = nil
        self.panel = nil
        callbacks.splashClosed(generation: hostGeneration)
    }

    /// Watchdog and resign-snap both need the panel to land at its final geometry with no further
    /// animation before finalizing, since the in-flight animation group may still be stalled.
    private func forceCompleteHandoff(panel: RightKitOverlayPanel, target: NSRect, generation: Int) {
        guard isCurrentHandoff(panel: panel, generation: generation) else { return }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        panel.setFrame(target, display: true)
        CATransaction.commit()
        finalizeHandoff(panel: panel, generation: generation)
    }

    private func armResignObserver(panelID: ObjectIdentifier, generation: Int) {
        removeResignObserver()
        resignObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.didResignActiveNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, let panel = self.panel, ObjectIdentifier(panel) == panelID,
                      self.isCurrentHandoff(panel: panel, generation: generation) else { return }
                let fallbackTarget = self.runtime.pill?.panel.map { self.collapsedPillTarget(from: $0.frame) } ?? panel.frame
                self.callbacks.log("splash_lifecycle", "phase=handoff_forced reason=resign_active")
                self.forceCompleteHandoff(panel: panel, target: fallbackTarget, generation: generation)
            }
        }
    }

    private func removeResignObserver() {
        if let resignObserver {
            NotificationCenter.default.removeObserver(resignObserver)
        }
        resignObserver = nil
    }

    private func create() {
        // The overlay panel applies the transparent, shadowless look; the level, Space behaviour
        // and hide-on-deactivate are the host's.
        let windows = runtime.configuration.windows
        let panel = RightKitOverlayPanel(
            contentRect: NSRect(origin: .zero, size: configuration.size),
            kind: .pill,
            configuration: RightKitOverlayPanelConfiguration(
                level: windows.splashLevel,
                collectionBehavior: windows.splashCollectionBehavior,
                canBecomeKey: false,
                becomesKeyOnlyIfNeeded: false,
                hasShadow: false,
                startsClickThrough: false,
                hidesOnDeactivate: true
            )
        )
        let container = NSView(frame: NSRect(origin: .zero, size: configuration.size))
        let host = NSHostingView(rootView: views.makeSplashRoot(model))
        // The splash has a fixed viewport. Avoid intrinsic-size probing of the cold view tree
        // while Auto Layout discovers the same size.
        host.sizingOptions = []
        host.frame = container.bounds
        host.autoresizingMask = [.width, .height]
        container.addSubview(host)
        panel.contentView = container
        // Keep the native host on the same clipping contract from its first frame, not only after
        // the launch handoff begins.
        disableContractionShadow(on: panel)
        self.panel = panel
    }
}
#endif
