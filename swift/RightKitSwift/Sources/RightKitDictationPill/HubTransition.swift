#if os(macOS)
import AppKit
import Foundation
import QuartzCore
import RightKitOverlay
import SwiftUI

/// Grows a shell from the pill into the hub window, and back. The pill stays a separate surface;
/// the transition window sits below it. The shell's look, the easing and the levels are the host's.
///
/// Lifecycle: one click-through transition window, created on the first transition and reused;
/// one frame animator (cancelled by every new transition and by `shutdown()`).
///
/// Destination invariant (expansion only): from the moment `animate(to:)` sets the destination window's
/// alpha to 0 until the transition ends, `destinationWindow` holds it STRONGLY, and every exit (completion,
/// a new transition, `shutdown()`) restores it through `restoreDestination` BEFORE the reference is
/// dropped. What the expansion changes on the destination: `alphaValue` (set to 0), `frame` (moved clear of
/// the pill) and ordering (`orderFrontRegardless`). Restoring means alpha 1, the presented state; the moved
/// frame is the presented frame and stays. The reference is held for one expansion (`hubExpansionDuration`)
/// at most and is released by every exit, so a closed window is not kept alive beyond it.
@MainActor
final class HubTransitionController {
    unowned let runtime: RightKitPillRuntime
    private var transitionPanel: RightKitOverlayPanel?
    private var destinationWindow: NSWindow?
    private let frameAnimator = GenerationGuardedFrameAnimator()

    init(runtime: RightKitPillRuntime) { self.runtime = runtime }

    private var views: RightKitPillViews { runtime.views }
    private var splash: RightKitPillSplashConfiguration { runtime.configuration.splash }

    /// Click-through, below the pill. `hidesOnDeactivate` and `becomesKeyOnlyIfNeeded` keep the
    /// `NSPanel` defaults this panel always ran with.
    private func makePanel(contentRect: NSRect) -> RightKitOverlayPanel {
        let windows = runtime.configuration.windows
        return RightKitOverlayPanel(
            contentRect: contentRect,
            kind: .pill,
            configuration: RightKitOverlayPanelConfiguration(
                level: windows.hubTransitionLevel,
                collectionBehavior: windows.hubTransitionCollectionBehavior,
                canBecomeKey: false,
                becomesKeyOnlyIfNeeded: false,
                hasShadow: false,
                startsClickThrough: true,
                hidesOnDeactivate: true
            )
        )
    }

    func animate(to hubWindow: NSWindow) {
        finishImmediately()
        guard let pillFrame = runtime.pill?.panel?.frame else {
            presentImmediately(hubWindow)
            return
        }
        guard !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion else {
            presentImmediately(hubWindow)
            return
        }

        // Keep the expanded pill visually clear of the hub while the shell emerges from it.
        let destinationFrame = destinationFrame(hubWindow.frame, above: pillFrame)
        hubWindow.setFrame(destinationFrame, display: false)
        let shellColor = hubWindow.backgroundColor ?? views.hubShellFallbackColor()
        let panel = transitionPanel ?? makePanel(contentRect: pillFrame)
        panel.setFrame(pillFrame, display: false)
        panel.alphaValue = 1
        // The shell emerging from the pill's location must stay behind it, so the pill never
        // appears to become the hub (the level is the host's `hubTransitionLevel`).
        panel.setClickThrough(true)
        panel.contentView = NSHostingView(rootView: views.makeHubShell(shellColor))
        transitionPanel = panel
        destinationWindow = hubWindow

        hubWindow.alphaValue = 0
        hubWindow.orderFrontRegardless()
        panel.setIsVisible(true)
        panel.orderFrontRegardless()
        startFrameTransition(panel: panel, from: pillFrame, to: destinationFrame) { [weak self] in
            guard let self else { return }
            // Completion: the destination is restored (alpha 1) in the same transaction that retires the
            // shell; it is activated only while the app still has it on screen.
            let destination = self.destinationWindow
            self.destinationWindow = nil
            if let destination {
                self.swapToHub(destination, retiring: panel)
                if destination.isVisible { self.activate(destination) }
            } else {
                self.retire(panel)
            }
        }
    }

    func animateBack(from hubWindow: NSWindow) {
        finishImmediately()
        guard let pillFrame = runtime.pill?.panel?.frame,
              !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion else {
            hubWindow.alphaValue = 0
            return
        }
        let sourceFrame = hubWindow.frame
        let shellColor = hubWindow.backgroundColor ?? views.hubShellFallbackColor()
        let panel = transitionPanel ?? makePanel(contentRect: sourceFrame)
        panel.setFrame(sourceFrame, display: false)
        panel.alphaValue = 1
        panel.setClickThrough(true)
        panel.contentView = NSHostingView(rootView: views.makeHubShell(shellColor))
        transitionPanel = panel
        hubWindow.alphaValue = 0
        panel.setIsVisible(true)
        panel.orderFrontRegardless()
        startFrameTransition(panel: panel, from: sourceFrame, to: pillFrame) { [weak self] in
            self?.retire(panel)
        }
    }

    /// Cancels the animation and closes the transition window. Called when the runtime shuts down.
    /// The destination is restored first, then the shell is retired, then the reference is dropped, so a
    /// shutdown at any point of an expansion never leaves the destination at alpha 0.
    func shutdown() {
        frameAnimator.cancel()
        restoreDestination()
        if let transitionPanel {
            retire(transitionPanel)
            transitionPanel.close()
        }
        transitionPanel = nil
        destinationWindow = nil
    }

    private func startFrameTransition(
        panel: RightKitOverlayPanel,
        from source: CGRect,
        to destination: CGRect,
        completion: @escaping @MainActor () -> Void
    ) {
        let started = CACurrentMediaTime()
        let duration = splash.hubExpansionDuration
        frameAnimator.start(onTick: { [weak panel, weak self] now in
            guard let panel, let self else { return false }
            let linear = min(1, (now - started) / duration)
            let eased = self.views.hubEasing(linear)
            panel.setFrame(Self.interpolate(from: source, to: destination, progress: eased), display: true)
            return linear >= 1
        }, onComplete: completion)
    }

    static func interpolate(from source: CGRect, to destination: CGRect, progress: Double) -> CGRect {
        CGRect(
            x: source.minX + (destination.minX - source.minX) * progress,
            y: source.minY + (destination.minY - source.minY) * progress,
            width: source.width + (destination.width - source.width) * progress,
            height: source.height + (destination.height - source.height) * progress
        )
    }

    func destinationFrame(_ hub: CGRect, above pill: CGRect) -> CGRect {
        let requiredMinY = pill.maxY + splash.hubPillClearance
        return hub.offsetBy(dx: 0, dy: max(0, requiredMinY - hub.minY))
    }

    /// Ends the current transition, if any, because another one starts. The destination of an expansion
    /// is restored (alpha 1, no activation: the next transition decides what to present) before the
    /// reference is dropped.
    private func finishImmediately() {
        frameAnimator.cancel()
        restoreDestination()
        if let transitionPanel { retire(transitionPanel) }
        destinationWindow = nil
    }

    /// Puts the expansion's destination back to its presented state: alpha 1. Does not order the window
    /// front or activate it, so a window the app closed or ordered out meanwhile stays closed (it only
    /// gets its alpha back, so a later reuse is not invisible). A no-op when no expansion is running.
    private func restoreDestination() {
        guard let destinationWindow else { return }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        destinationWindow.alphaValue = 1
        CATransaction.commit()
    }

    private func retire(_ panel: RightKitOverlayPanel) {
        panel.alphaValue = 0
        panel.setClickThrough(true)
        panel.orderOut(nil)
    }

    private func swapToHub(_ hubWindow: NSWindow, retiring panel: RightKitOverlayPanel) {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        hubWindow.alphaValue = 1
        panel.alphaValue = 0
        panel.orderOut(nil)
        CATransaction.commit()
    }

    private func presentImmediately(_ hubWindow: NSWindow) {
        hubWindow.alphaValue = 1
        hubWindow.orderFrontRegardless()
        activate(hubWindow)
    }

    private func activate(_ hubWindow: NSWindow) {
        NSApp.activate(ignoringOtherApps: true)
        hubWindow.makeKeyAndOrderFront(nil)
        hubWindow.orderFrontRegardless()
    }
}
#endif
