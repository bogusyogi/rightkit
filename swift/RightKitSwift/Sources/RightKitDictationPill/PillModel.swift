#if os(macOS)
import AppKit
import Foundation
import QuartzCore
import RightKitOverlay
import SwiftUI

public enum RightKitPillClipboardMode: Int32, Sendable { case single = 0, recent = 1 }

/// Feedback on the idle pill's copy-last button.
public enum RightKitPillCopyLastState: Equatable, Sendable { case ready, copied, empty, failed }

/// One clipboard card row. The keys are the wire format the host encodes for the card.
public struct RightKitPillClipboardItem: Identifiable, Decodable, Equatable, Sendable {
    public let deliveryID: String
    public var transcript: String
    public var rawTranscript: String?
    public var id: String { deliveryID }

    enum CodingKeys: String, CodingKey {
        case deliveryID = "delivery_id", transcript, rawTranscript = "raw_transcript"
    }
}

/// What the engine tells the pill. The host decodes its wire into these.
public enum RightKitPillEvent: Sendable, Equatable {
    case hide
    case idle
    case recording
    /// An input level 0...1 (clamped). Never changes the mode.
    case level(Double)
    case processing
    /// A plain "done" status; shows ``RightKitPillLabels/done``.
    case statusDone
    /// A status with an optional label; nil shows ``RightKitPillLabels/saved``.
    case status(label: String?)
    case error(label: String?)

    /// A stable tag for diagnostics. Never carries a label.
    var tag: String {
        switch self {
        case .hide: "hide"
        case .idle: "idle"
        case .recording: "recording"
        case .level: "level"
        case .processing: "processing"
        case .statusDone: "status_done"
        case .status: "status"
        case .error: "error"
        }
    }
}

/// The pill's state machine: mode, hover, clipboard card, hint, status timers, copy-last feedback.
/// Everything a view reads is published; every time, size, word and curve comes from the host.
///
/// Main actor only. Timers are `DispatchWorkItem`s that this model cancels and replaces (see the
/// lifecycle table in HANDOFF.md); none outlives ``shutdown()``.
@MainActor
public final class RightKitPillModel: ObservableObject {
    public static let hiddenHint: Int32 = 0
    /// Clipboard JSON larger than this is treated as empty. The engine's own bounds are tighter.
    static let maxClipboardJSONBytes = 4 * 1024 * 1024
    static let maxClipboardItems = 64

    public let configuration: RightKitPillConfiguration
    public let callbacks: RightKitPillCallbackQueue
    let views: RightKitPillViews
    let sounds: RightKitPillActionSounds?
    weak var controller: PillPanelController?

    @Published public var mode: RightKitPillMode = .idle
    @Published public var statusLabel: String
    @Published public var cancellable = false
    @Published public var isVisible = false
    @Published public var isHovered = false
    @Published public var isHoverCollapsing = false
    /// Launch handoff briefly presents the native controls at their expanded size before settling.
    @Published public var launchExpanded = false
    @Published public var clipboardItems: [RightKitPillClipboardItem] = []
    @Published public var clipboardMode: RightKitPillClipboardMode = .recent
    @Published public var clipboardLoaded = false
    @Published public var clipboardVisible = false
    /// The clip card is leaving: still mounted so its exit animation can play, but no longer logically open.
    @Published public var clipboardRetiring = false
    /// The hint that is leaving (mounted for its exit animation); ``hiddenHint`` when none.
    @Published public var retiringHint: Int32 = RightKitPillModel.hiddenHint
    @Published public var copyLastState: RightKitPillCopyLastState = .ready
    /// Processing bars show only after ``RightKitPillTimings/processingIndicatorDelay``.
    @Published public var processingAnimationVisible = false
    /// The mode whose capsule size is actually on screen. Diverges from `mode` whenever
    /// `transitionModePresentation()` refused the resize; the refusal sites call
    /// `syncPresentationIfNeeded()` once they clear.
    public internal(set) var presentedMode: RightKitPillMode?
    @Published public var singleClipEditing = false
    @Published public var hint: Int32 = RightKitPillModel.hiddenHint
    @Published public var barLevels: [Double]
    @Published public var recordingElapsed = 0
    @Published public var revealProgress = false
    @Published public var introBloom = 0.0
    @Published public var presentationCapsuleSize: CGSize
    public internal(set) var level = 0.0
    var recordingStarted: Date?
    public internal(set) var closeAfterCopy = false
    /// Changes every time a clip card is set (`setClipboard`) or dismissed (`dismissClipboard`). A
    /// copy result or a timer from an earlier card compares it before it acts, so it can never mark
    /// or close a different card. Not published: a view reads it only inside a completion.
    public internal(set) var clipboardEpoch: UInt64 = 0
    private var introDidRun = false
    private var introTimer: Timer?
    private var suppressNextStopSound = false

    private var hoverExit: DispatchWorkItem?
    private var hoverCollapseFinish: DispatchWorkItem?
    private var statusDismiss: DispatchWorkItem?
    private var auxiliaryRetire: DispatchWorkItem?
    private var processingIndicatorWork: DispatchWorkItem?
    private var copyLastReset: DispatchWorkItem?
    private var hideOrderOut: DispatchWorkItem?

    public var showsProcessingWaveform: Bool { mode == .starting || mode == .processing }
    public var auxiliaryRetiring: Bool { clipboardRetiring || retiringHint != Self.hiddenHint }
    /// True when the window frame tracks the capsule; false while a hint or clip owns the window
    /// size (open, or still playing its exit).
    public var windowFollowsCapsule: Bool { !clipboardVisible && hint == Self.hiddenHint && !auxiliaryRetiring }
    private var reduceMotion: Bool { NSWorkspace.shared.accessibilityDisplayShouldReduceMotion }
    private var handoffBlocks: Bool { controller?.launchHandoff.blocksNormalPillMutation == true }

    init(
        configuration: RightKitPillConfiguration, callbacks: RightKitPillCallbackQueue,
        views: RightKitPillViews, sounds: RightKitPillActionSounds?
    ) {
        self.configuration = configuration
        self.callbacks = callbacks
        self.views = views
        self.sounds = sounds
        self.statusLabel = configuration.labels.done
        self.barLevels = Array(repeating: 0.0, count: configuration.barCount)
        self.presentationCapsuleSize = configuration.geometry.initialCapsuleSize
    }

    /// Stops every timer the model owns. Called when the controller shuts down.
    func shutdown() {
        hoverExit?.cancel(); hoverExit = nil
        hoverCollapseFinish?.cancel(); hoverCollapseFinish = nil
        statusDismiss?.cancel(); statusDismiss = nil
        auxiliaryRetire?.cancel(); auxiliaryRetire = nil
        processingIndicatorWork?.cancel(); processingIndicatorWork = nil
        copyLastReset?.cancel(); copyLastReset = nil
        hideOrderOut?.cancel(); hideOrderOut = nil
        introTimer?.invalidate(); introTimer = nil
        controller = nil
    }

    // MARK: Engine events

    public func push(_ event: RightKitPillEvent, cancellable: Bool) {
        let labels = configuration.labels
        let next: RightKitPillMode?
        switch event {
        case .hide:
            setVisible(false)
            return
        case .level(let value):
            level = value.isFinite ? max(0, min(1, value)) : 0
            return
        case .idle: next = .idle
        case .recording: next = .recording
        case .processing: next = .processing
        case .statusDone:
            statusLabel = labels.done
            next = .status
        case .status(let label):
            let resolved = RightKitPillLimits.text(label ?? labels.saved, characters: RightKitPillLimits.labelCharacters)
            statusLabel = labels.cancelledAliases.contains(resolved.lowercased()) ? labels.cancelled : resolved
            next = .status
        case .error(let label):
            statusLabel = RightKitPillLimits.text(label ?? labels.error, characters: RightKitPillLimits.labelCharacters)
            next = .error
        }
        guard let next else { return }
        let pushStarted = CACurrentMediaTime()
        if next == .recording, let controller, controller.launchHandoff.blocksNormalPillMutation {
            controller.launchHandoff.recorder.record(
                "recording-event",
                at: 0,
                pillSize: configuration.geometry.restingSize,
                phase: controller.launchHandoff.phase
            )
        }
        let previous = mode
        let entering = next != mode
        let actionSound: RightKitPillActionSound?
        if entering && next == .recording {
            suppressNextStopSound = false
            actionSound = .start
        } else if previous == .recording && next == .processing {
            if suppressNextStopSound {
                suppressNextStopSound = false
                actionSound = nil
            } else {
                actionSound = .stop
            }
        } else {
            actionSound = nil
        }
        mode = next
        if next == .processing {
            if entering { scheduleProcessingIndicator() }
        } else {
            cancelProcessingIndicator()
        }
        if !showsProcessingWaveform && next != .recording {
            barLevels = Array(repeating: 0, count: configuration.barCount)
        }
        self.cancellable = cancellable
        if entering {
            // A hover-exit (or its collapse completion) scheduled before this mode change would
            // otherwise start a capsule resize that cancels the mode resize, or collapse it
            // immediately after it lands. Drop them, then take the hover state that is actually on
            // screen so the mode resize below is computed from it rather than from a stale `isHovered`.
            cancelPendingHoverCallbacks()
            controller?.reconcileHoverFromPointer()
        }
        let transitionNeeded = Self.shouldTransitionModePresentation(from: presentedMode, to: next)
        var transitionApplied = false
        if transitionNeeded {
            transitionApplied = controller?.transitionModePresentation() == true
            if transitionApplied { presentedMode = next }
        }
        if next == .recording {
            if entering || recordingStarted == nil {
                recordingStarted = Date()
                recordingElapsed = 0
            }
        } else {
            recordingStarted = nil
            recordingElapsed = 0
            self.level = 0
        }
        scheduleStatusDismiss(for: next)
        if let actionSound { sounds?.play(actionSound) }
        callbacks.log(
            "pill_model_push",
            "event=\(event.tag) previous=\(previous) next=\(next) entering=\(entering) transition_needed=\(transitionNeeded) transition_applied=\(transitionApplied) elapsed_ms=\(Int((CACurrentMediaTime() - pushStarted) * 1000))"
        )
    }

    /// Compare against the mode whose size was actually PRESENTED, never the previously
    /// *requested* mode.
    ///
    /// Start publishes optimistic + authoritative Recording events within one frame, and
    /// restarting the same size animation for each creates a visible hitch, so identical modes are
    /// still collapsed. Status and error labels may change width while remaining in the same mode,
    /// so those always refresh.
    ///
    /// The `presented` (not `previous`) comparison is what makes a refused transition self-heal:
    /// `transitionModePresentation()` declines while the launch handoff blocks mutation, while the
    /// clipboard card is open, and while a hint is showing. Keying off `previous` would let the
    /// follow-up event look like recording to recording and get suppressed, so the capsule would
    /// never grow.
    static func shouldTransitionModePresentation(from presented: RightKitPillMode?, to next: RightKitPillMode) -> Bool {
        presented != next || next == .status || next == .error
    }

    /// Re-apply the capsule size when a transition was refused earlier. Called once the conditions
    /// that refuse it clear (hint dismissed, clipboard closed, splash handoff finished).
    public func syncPresentationIfNeeded() {
        guard presentedMode != mode else { return }
        if controller?.transitionModePresentation() == true {
            presentedMode = mode
        }
    }

    // MARK: Visibility and launch

    public func setVisible(_ visible: Bool) {
        if handoffBlocks { return }
        if visible {
            hideOrderOut?.cancel(); hideOrderOut = nil
            revealProgress = true
            isVisible = true
            controller?.showPanel()
        } else {
            withAnimation(views.hideAnimation) {
                revealProgress = false
                isVisible = false
            }
            hideOrderOut?.cancel()
            let item = pillWorkItem { [weak self] in
                guard let self, self.isVisible == false else { return }
                self.hideOrderOut = nil
                self.controller?.hidePanel()
            }
            hideOrderOut = item
            DispatchQueue.main.asyncAfter(deadline: .now() + configuration.timings.hideOrderOutDelay, execute: item)
        }
    }

    public func reveal() {
        if handoffBlocks { return }
        setVisible(true)
        revealProgress = false
        withAnimation(views.revealAnimation) { revealProgress = true }
        guard !introDidRun else { return }
        introDidRun = true
        startIntroBloom()
        views.introBloomStarted?()
    }

    @discardableResult
    func prepareForSplashHandoff() -> Bool {
        if let controller, !controller.launchHandoff.prepare() { return false }
        controller?.endSingleClipEditing()
        cancelAuxiliaryRetire()
        clipboardVisible = false
        singleClipEditing = false
        hint = Self.hiddenHint
        introDidRun = false
        introTimer?.invalidate()
        introTimer = nil
        introBloom = 0
        launchExpanded = false
        isVisible = true
        revealProgress = true
        controller?.prepareSplashDestination()
        return true
    }

    func completeSplashHandoff() {
        guard controller?.launchHandoff.phase == .handedOff else { return }
        isVisible = true
        revealProgress = true
        launchExpanded = false
        // Opacity is swapped by the splash controller; this method only owns the model's one-time
        // bloom after the atomic native handoff.
        guard !introDidRun else { return }
        introDidRun = true
        startIntroBloom()
        views.introBloomStarted?()
    }

    // MARK: Hover

    public func setHover(_ hovered: Bool) {
        guard controller?.launchHandoff.blocksNormalPillMutation != true,
              controller?.launchHandoff.requiresPointerMovement != true else { return }
        hoverExit?.cancel()
        hoverCollapseFinish?.cancel()
        if hovered {
            isHoverCollapsing = false
            isHovered = true
            if mode == .idle || mode == .recording {
                controller?.transitionHoverPresentation(expanded: true)
            }
        } else {
            let item = pillWorkItem { [weak self] in
                guard let self else { return }
                let reduced = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
                let canCompress = !reduced && !self.clipboardVisible && (self.mode == .idle || self.mode == .recording)
                guard canCompress else {
                    self.completeHoverExit()
                    return
                }
                self.isHoverCollapsing = true
                self.isHovered = false
                let finish = pillWorkItem { [weak self] in
                    guard let self, !self.isHovered else { return }
                    self.isHoverCollapsing = false
                    if self.mode == .error { self.scheduleStatusDismiss(for: .error) }
                }
                self.hoverCollapseFinish = finish
                if let controller = self.controller {
                    controller.transitionHoverPresentation(expanded: false, completion: finish.perform)
                } else {
                    DispatchQueue.main.asyncAfter(
                        deadline: .now() + self.configuration.timings.hoverExitFallbackDuration, execute: finish
                    )
                }
            }
            hoverExit = item
            let delay = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion ? 0 : configuration.timings.hoverExitDelay
            DispatchQueue.main.asyncAfter(deadline: .now() + delay, execute: item)
        }
    }

    func acknowledgePointerMovement() -> Bool {
        controller?.launchHandoff.acknowledgePointerMovement() ?? false
    }

    /// Cancel the delayed hover-exit and its collapse completion, so a hover callback queued before
    /// a mode change cannot interrupt the mode resize or undo it one frame later. The hover state
    /// is re-read from the pointer immediately afterwards (see `reconcileHoverFromPointer`).
    private func cancelPendingHoverCallbacks() {
        hoverExit?.cancel()
        hoverExit = nil
        hoverCollapseFinish?.cancel()
        hoverCollapseFinish = nil
        isHoverCollapsing = false
    }

    private func completeHoverExit() {
        isHovered = false
        isHoverCollapsing = false
        if !clipboardVisible && (mode == .idle || mode == .recording) {
            controller?.transitionHoverPresentation(expanded: false)
        }
        if mode == .error { scheduleStatusDismiss(for: .error) }
    }

    // MARK: Clipboard card and hints

    /// `json` is the engine's item list. Text over 4 MiB shows an empty card, and at most 64 items
    /// are read; the engine bounds both further. `mode` other than 0 or 1 is the "recent" card.
    public func setClipboard(json: String, mode: Int32, loaded: Bool) {
        guard !handoffBlocks else { return }
        clipboardEpoch &+= 1
        let resolvedMode = RightKitPillClipboardMode(rawValue: mode) ?? .recent
        var decodedItems: [RightKitPillClipboardItem] = []
        if json.utf8.count <= Self.maxClipboardJSONBytes {
            decodedItems = (try? JSONDecoder().decode([RightKitPillClipboardItem].self, from: Data(json.utf8))) ?? []
            if decodedItems.count > Self.maxClipboardItems { decodedItems = Array(decodedItems.prefix(Self.maxClipboardItems)) }
        }
        cancelAuxiliaryRetire()
        clipboardMode = resolvedMode
        clipboardLoaded = loaded
        singleClipEditing = false
        clipboardItems = decodedItems
        closeAfterCopy = resolvedMode == .single
            ? configuration.closeSingleClipboardAfterCopy
            : configuration.closeRecentClipboardAfterCopy
        hint = Self.hiddenHint
        controller?.prepareClipboardPresentation(resolvedMode, items: decodedItems)
        // Visibility is last: the first rendered card frame already contains every row and lands
        // inside a center-fixed, final-size window.
        clipboardVisible = true
    }

    /// `id` is a host hint id, 0 hides. Ids outside 0...4096 hide.
    public func setHint(_ id: Int32) {
        guard !handoffBlocks else { return }
        let mode = RightKitPillLimits.hintIDRange.contains(id) ? id : Self.hiddenHint
        if mode != Self.hiddenHint {
            cancelAuxiliaryRetire()
            cancelPendingHoverCallbacks()
            // A visible hint suppresses idle controls, so an idle hover must not leave an empty
            // expanded capsule behind the hint. Recording controls intentionally remain available.
            if self.mode == .idle { isHovered = false }
            controller?.prepareHintPresentation()
        }
        let previousHint = hint
        let wasVisible = previousHint != Self.hiddenHint
        hint = mode
        // While a hint is up the WINDOW is sized from the content (hint + capsule), and that path
        // stops driving the window the moment the hint hides. Put the window back to the capsule
        // of the current mode, or a transparent hint-sized window stays over whatever is underneath
        // and swallows clicks. The clipboard card owns the window when it is open. The hint plays
        // its exit first; the window shrinks when it is gone.
        if mode == Self.hiddenHint, wasVisible, !clipboardVisible {
            retireAuxiliary(hint: previousHint, clipboard: false)
        } else if mode == Self.hiddenHint {
            syncPresentationIfNeeded()
        }
    }

    public func dismissClipboard(notify: Bool = true) {
        guard !handoffBlocks else { return }
        clipboardEpoch &+= 1
        controller?.endSingleClipEditing()
        singleClipEditing = false
        let clipWasOpen = clipboardVisible
        let linked = configuration.clipboardLinkedHint
        let leavingHint: Int32 = (linked != nil && hint == linked) ? hint : Self.hiddenHint
        clipboardVisible = false
        if leavingHint != Self.hiddenHint { hint = Self.hiddenHint }
        if clipWasOpen || leavingHint != Self.hiddenHint {
            retireAuxiliary(hint: leavingHint, clipboard: clipWasOpen)
        } else if !auxiliaryRetiring {
            controller?.restoreCapsulePresentation()
        }
        if notify { callbacks.dismissClipboard() }
    }

    public func beginSingleClipEditing() { controller?.beginSingleClipEditing() }
    public func endSingleClipEditing() { controller?.endSingleClipEditing() }

    /// Hint and clip cards leave with the same spring they arrive on. They stay mounted (and the
    /// window keeps their size) for `cardRetireDelay`, while the capsule settles to its resting
    /// size inside the window; then the window snaps to the capsule.
    private func retireAuxiliary(hint retiredHint: Int32, clipboard: Bool) {
        guard !reduceMotion else {
            controller?.restoreCapsulePresentation()
            return
        }
        auxiliaryRetire?.cancel()
        if retiredHint != Self.hiddenHint { retiringHint = retiredHint }
        if clipboard { clipboardRetiring = true }
        controller?.settleCapsuleWhileAuxiliaryRetires()
        let item = pillWorkItem { [weak self] in self?.finishAuxiliaryRetire() }
        auxiliaryRetire = item
        DispatchQueue.main.asyncAfter(deadline: .now() + configuration.timings.cardRetireDelay, execute: item)
    }

    private func finishAuxiliaryRetire() {
        auxiliaryRetire = nil
        guard auxiliaryRetiring else { return }
        clipboardRetiring = false
        retiringHint = Self.hiddenHint
        guard !clipboardVisible, hint == Self.hiddenHint else { return }
        controller?.restoreCapsulePresentation()
    }

    /// A new card (or a splash handoff) takes over: drop any exit in progress.
    private func cancelAuxiliaryRetire() {
        auxiliaryRetire?.cancel()
        auxiliaryRetire = nil
        clipboardRetiring = false
        retiringHint = Self.hiddenHint
    }

    private func scheduleProcessingIndicator() {
        processingIndicatorWork?.cancel()
        processingAnimationVisible = false
        let work = pillWorkItem { [weak self] in
            guard let self, self.mode == .processing else { return }
            self.processingAnimationVisible = true
        }
        processingIndicatorWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + configuration.timings.processingIndicatorDelay, execute: work)
    }

    private func cancelProcessingIndicator() {
        processingIndicatorWork?.cancel()
        processingIndicatorWork = nil
        if processingAnimationVisible { processingAnimationVisible = false }
    }

    // MARK: Actions

    /// Idle pill's one-click copy of the newest transcript. The button flashes a result for a
    /// moment; an empty history flashes a disabled look and copies nothing. The transcript text
    /// never passes through here, and "copied" shows only after the host reported success.
    public func copyLastTranscript() {
        guard copyLastState == .ready else { return }
        callbacks.log("pill_native_action", "action=copy_last phase=clicked")
        callbacks.copyLast { [weak self] result in
            guard let self else { return }
            let next: RightKitPillCopyLastState
            let hold: Double
            let timings = self.configuration.timings
            switch result {
            case .copied: next = .copied; hold = timings.copyLastCopiedHold
            case .empty: next = .empty; hold = timings.copyLastEmptyHold
            case .failed: next = .failed; hold = timings.copyLastFailedHold
            }
            self.copyLastState = next
            self.copyLastReset?.cancel()
            let reset = pillWorkItem { [weak self] in
                self?.copyLastState = .ready
                self?.copyLastReset = nil
            }
            self.copyLastReset = reset
            DispatchQueue.main.asyncAfter(deadline: .now() + hold, execute: reset)
        }
    }

    public func cancelRecording() {
        callbacks.log("cancel_input_candidate", "source=pill_cancel_button phase=clicked")
        sounds?.play(.cancel)
        callbacks.cancelProcessing()
    }

    public func stopAndSend() {
        suppressNextStopSound = true
        sounds?.play(.stopAndSend)
        callbacks.stopAndSend()
    }

    // MARK: Levels and intro

    /// One frame of the level pump. The bars' shape is the host's (`RightKitPillViews.barLevels`);
    /// the elapsed counter and the zeroing outside recording and processing are state, and stay here.
    func integrateLevels(now: CFTimeInterval) {
        let count = configuration.barCount
        func fitted(_ bars: [Double]) -> [Double] {
            if bars.count == count { return bars }
            return Array(bars.prefix(count)) + Array(repeating: 0, count: max(0, count - bars.count))
        }
        if mode == .processing {
            barLevels = fitted(views.barLevels(.processing, level, now, barLevels, reduceMotion))
            return
        }
        guard mode == .recording else {
            if barLevels.contains(where: { $0 != 0 }) {
                barLevels = Array(repeating: 0, count: count)
            }
            return
        }
        if let recordingStarted {
            recordingElapsed = max(0, Int(Date().timeIntervalSince(recordingStarted)))
        }
        barLevels = fitted(views.barLevels(.recording, level, now, barLevels, reduceMotion))
    }

    /// The host's intro curve, made safe: progress is clamped to 0...1 going in; a non-finite result is
    /// 0 (no bloom) and a finite one is clamped to 0...1 coming out.
    static func clampedIntroIntensity(_ curve: (Double) -> Double, progress: Double) -> Double {
        let input = progress.isFinite ? max(0, min(1, progress)) : 0
        let value = curve(input)
        return value.isFinite ? max(0, min(1, value)) : 0
    }

    private func startIntroBloom() {
        introTimer?.invalidate()
        introBloom = 0
        let started = ProcessInfo.processInfo.systemUptime
        let duration = configuration.timings.introDuration
        let timer = Timer(timeInterval: 1 / 60, repeats: true) { [weak self] timer in
            MainActor.assumeIsolated {
                guard let self else { timer.invalidate(); return }
                let progress = min(1, (ProcessInfo.processInfo.systemUptime - started) / duration)
                self.introBloom = Self.clampedIntroIntensity(self.views.introIntensity, progress: progress)
                if progress >= 1 {
                    timer.invalidate()
                    self.introTimer = nil
                    self.introBloom = 0
                }
            }
        }
        timer.tolerance = 0.002
        introTimer = timer
        RunLoop.main.add(timer, forMode: .common)
    }

    // MARK: Status expiry

    private func scheduleStatusDismiss(for mode: RightKitPillMode) {
        statusDismiss?.cancel()
        guard mode == .status || mode == .error else { return }
        let timings = configuration.timings
        let delay = mode == .status ? timings.statusDismissDelay : timings.errorDismissDelay
        // Bind the error expiry to THIS error's generation (-1 = unbound).
        let errorGeneration: Int64 = mode == .error ? callbacks.terminalErrorGeneration() : -1
        let item = pillWorkItem { [weak self] in
            guard let self, !(self.mode == .error && self.isHovered) else { return }
            let recoverBackend = self.mode == .error
            self.push(.idle, cancellable: false)
            if recoverBackend { self.callbacks.recoverTerminalError(generation: errorGeneration) }
        }
        statusDismiss = item
        DispatchQueue.main.asyncAfter(deadline: .now() + delay, execute: item)
    }
}
#endif
