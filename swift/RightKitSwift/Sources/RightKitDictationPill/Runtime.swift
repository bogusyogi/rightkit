#if os(macOS)
import AppKit
import Foundation
import QuartzCore
import RightKitOverlay
import SwiftUI

/// The native dictation pill: a non-activating status pill with a clipboard card, hint cards, a
/// launch splash that flies into it, and a hub transition. One runtime per process, created by the
/// host (on the main thread) and held for the process lifetime.
///
/// The host owns everything a product decides: sizes, times, words, colours, window levels and the
/// views (see ``RightKitPillConfiguration`` and ``RightKitPillViews``), and the transport that
/// carries these calls (a C table, XPC, anything). Every method here runs on the main actor; a host
/// entry point on another thread hops with ``RightKitPillMain``.
///
/// Lifecycle (who owns what, what stops it):
/// - `pill` (window, hover watchdog, level pump, spring animator, mouse monitors, screen observer,
///   model timers): created by `initialize()`, `pushEvent` or `splashShow()`; stopped by `shutdown()`.
/// - `splash` (window, loader timer, deadlines, resign observer): created lazily on the first
///   `splashShow()`; stopped by `splashDismiss()` and by `shutdown()`.
/// - `hub` (one transition window, one frame animator): stopped by `shutdown()`.
@MainActor
public final class RightKitPillRuntime {
    public let configuration: RightKitPillConfiguration
    let views: RightKitPillViews
    let callbacks: RightKitPillCallbackQueue
    let sounds: RightKitPillActionSounds?
    var pill: PillPanelController?
    private(set) var splash: SplashPanelController!
    private var hub: HubTransitionController!

    /// `configuration` is validated (clamped) here. `callbacks` is the host's thread-safe callback
    /// queue, `sounds` its optional action sounds.
    public init(
        configuration: RightKitPillConfiguration,
        views: RightKitPillViews,
        callbacks: RightKitPillCallbackQueue,
        sounds: RightKitPillActionSounds?
    ) {
        self.configuration = configuration.validated()
        self.views = views
        self.callbacks = callbacks
        self.sounds = sounds
        self.splash = SplashPanelController(runtime: self)
        self.hub = HubTransitionController(runtime: self)
    }

    /// The model the views observe; nil before the pill exists and after `shutdown()`.
    public var model: RightKitPillModel? { pill?.model }
    public var splashModel: RightKitSplashModel { splash.model }

    private func ensurePill() -> PillPanelController {
        if let pill { return pill }
        let created = PillPanelController(runtime: self)
        pill = created
        return created
    }

    private func installPillControllerIfNeeded(forSplash: Bool = false) {
        let fontsStarted = CACurrentMediaTime()
        views.registerResources()
        callbacks.log("pill_startup_timing", "phase=fonts elapsed_ms=\(Int((CACurrentMediaTime() - fontsStarted) * 1000))")
        let destinationStarted = CACurrentMediaTime()
        let controller = ensurePill()
        if forSplash {
            controller.prepareForSplash()
        } else {
            controller.install()
        }
        callbacks.log(
            "pill_startup_timing",
            "phase=destination elapsed_ms=\(Int((CACurrentMediaTime() - destinationStarted) * 1000)) deferred=\(forSplash)"
        )
        if !forSplash { sounds?.prewarm() }
    }

    // MARK: Pill

    public func initialize() { installPillControllerIfNeeded() }

    public func shutdown() {
        pill?.shutdown()
        splash.dismiss()
        hub.shutdown()
    }

    /// `receivedAt` is `CACurrentMediaTime()` when the host got the event, for the queue-delay diagnostic.
    public func pushEvent(_ event: RightKitPillEvent, cancellable: Bool, receivedAt: CFTimeInterval) {
        if case .level = event {} else {
            callbacks.log(
                "pill_bridge_received",
                "event=\(event.tag) main_queue_delay_ms=\(Int((CACurrentMediaTime() - receivedAt) * 1000))"
            )
        }
        let controller = ensurePill()
        controller.install()
        controller.applyEvent(event, cancellable: cancellable)
    }

    public func reveal() { pill?.model.reveal() }

    public func setVisible(_ visible: Bool) {
        pill?.model.setVisible(visible)
        pill?.syncLevelDriver()
    }

    public var isPanelVisible: Bool { pill?.panel?.isVisible == true }

    /// Whether the launch handoff still blocks ordinary pill changes. A wall-clock backstop: once the
    /// contraction deadline passes, the splash phase is terminated before this answers "not active".
    /// Answering false alone would leave later calls alternating around stale state.
    public func splashHandoffActive() -> Bool {
        guard pill?.launchHandoff.blocksNormalPillMutation == true else { return false }
        if splash.handoffDeadlineExceeded {
            splash.forceCompleteHandoffAtDeadline()
            return false
        }
        return true
    }

    /// Terminal backstop for a stalled handoff. A no-op until the contraction has started.
    public func forceCompleteSplashHandoff() { splash.forceCompleteHandoffAtDeadline() }

    /// `json` is the engine's clipboard item list; see ``RightKitPillModel/setClipboard(json:mode:loaded:)``.
    public func setClipboard(json: String, mode: Int32, loaded: Bool) {
        pill?.model.setClipboard(json: json, mode: mode, loaded: loaded)
    }

    /// Closes the clipboard card without telling the host (the host asked for it).
    public func closeClipboard() { pill?.model.dismissClipboard(notify: false) }

    /// `id` is a host hint id; 0 hides.
    public func setHint(_ id: Int32) { pill?.model.setHint(id) }

    // MARK: Splash

    public func splashShow() {
        // The host requests the splash before its ordinary install call. Satisfy the splash's
        // hidden-destination dependency here so callback ordering cannot suppress launch or leave
        // the handoff unprepared.
        let started = CACurrentMediaTime()
        installPillControllerIfNeeded(forSplash: true)
        splash.show()
        callbacks.log("splash_startup_timing", "phase=destination_prepared elapsed_ms=\(Int((CACurrentMediaTime() - started) * 1000))")
    }

    public func splashDismiss() { splash.dismiss() }

    /// Loader step, wrapped into 0..<stepCount (a negative step wraps too).
    public func setSplashStep(_ step: Int32) {
        let count = configuration.splash.stepCount
        splash.model.step = ((Int(step) % count) + count) % count
    }

    /// nil shows the configured initial message. Cut to 256 characters.
    public func setSplashMessage(_ text: String?) {
        splash.model.message = RightKitPillLimits.text(
            text ?? configuration.splash.initialMessage, characters: RightKitPillLimits.messageCharacters
        )
    }

    public func flyIntoPill() { splash.flyIntoPill() }

    // MARK: Hub

    /// Grows the hub window out of the pill. The controller holds `hubWindow` strongly until the transition
    /// ends and restores its alpha on every exit (see `HubTransitionController`).
    public func animateHubFromPill(_ hubWindow: NSWindow) { hub.animate(to: hubWindow) }

    /// Shrinks the hub window back into the pill.
    public func animateHubToPill(_ hubWindow: NSWindow) { hub.animateBack(from: hubWindow) }
}
#endif
