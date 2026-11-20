#if os(macOS)
import AppKit
import Foundation
import RightKitOverlay
import SwiftUI

// Everything about the pill that a product decides. Nothing here has a default that names a
// product, a size, a time, a colour or a word: the host passes every value, and `validated()`
// clamps each one to the documented range so no host number can make a window, a timer or an
// allocation unbounded.

/// Ranges applied by every `validated()` in this file. They bound host input; they are not
/// product values.
enum RightKitPillLimits {
    static let sizeRange: ClosedRange<CGFloat> = 1...4096
    static let insetRange: ClosedRange<CGFloat> = 0...256
    static let secondsRange: ClosedRange<Double> = 0...120
    static let responseRange: ClosedRange<Double> = 0.01...5
    static let dampingRange: ClosedRange<Double> = 0.05...5
    static let labelCharacters = 128
    static let messageCharacters = 256
    static let aliasCount = 8
    static let hintIDRange: ClosedRange<Int32> = 0...4096

    static func clamp<T: Comparable>(_ value: T, _ range: ClosedRange<T>) -> T {
        min(max(value, range.lowerBound), range.upperBound)
    }

    static func finite(_ value: Double, fallback: Double) -> Double { value.isFinite ? value : fallback }

    static func size(_ value: CGSize) -> CGSize {
        CGSize(
            width: clamp(CGFloat(finite(Double(value.width), fallback: 1)), sizeRange),
            height: clamp(CGFloat(finite(Double(value.height), fallback: 1)), sizeRange)
        )
    }

    static func points(_ value: CGFloat, _ range: ClosedRange<CGFloat>) -> CGFloat {
        clamp(CGFloat(finite(Double(value), fallback: Double(range.lowerBound))), range)
    }

    static func seconds(_ value: Double) -> Double { clamp(finite(value, fallback: 0), secondsRange) }

    static func text(_ value: String, characters: Int) -> String {
        value.count <= characters ? value : String(value.prefix(characters))
    }
}

/// Capsule and window sizes. Every size is a host input.
public struct RightKitPillGeometry: Sendable, Equatable {
    /// The resting capsule (idle, not hovered), and the smallest size the window may take.
    public var restingSize: CGSize
    /// The capsule size the model reports before any mode has been presented.
    public var initialCapsuleSize: CGSize
    /// The idle capsule while hovered, or while a clipboard card is open.
    public var idleExpandedSize: CGSize
    public var recordingCompactSize: CGSize
    public var recordingExpandedSize: CGSize
    /// The capsule for `.starting` and `.processing`.
    public var processingSize: CGSize
    /// The capsule for `.status` and `.error` when the host does not measure the label
    /// (see ``RightKitPillViews/statusCapsuleSize``).
    public var statusFallbackSize: CGSize
    /// The expanded controls shown for a moment during the launch handoff.
    public var launchExpandedSize: CGSize
    /// The capsule while a clipboard card is open.
    public var clipboardCapsuleSize: CGSize
    /// The window for the "recent" clipboard card before wrapped rows are added.
    public var recentClipboardSize: CGSize
    public var recentClipboardWrappedRowHeight: CGFloat
    /// A transcript longer than this many characters, or holding a newline, counts as a wrapped row.
    public var recentClipboardWrapCharacterCount: Int
    /// Rows the "recent" card shows. 1...16.
    public var recentClipboardLimit: Int
    /// The widest window the content may ask for.
    public var maxContentWidth: CGFloat
    public var maxContentHeight: CGFloat
    /// Points between the bottom of the visible screen frame and the window.
    public var bottomInset: CGFloat

    public init(
        restingSize: CGSize, initialCapsuleSize: CGSize, idleExpandedSize: CGSize, recordingCompactSize: CGSize,
        recordingExpandedSize: CGSize, processingSize: CGSize, statusFallbackSize: CGSize,
        launchExpandedSize: CGSize, clipboardCapsuleSize: CGSize, recentClipboardSize: CGSize,
        recentClipboardWrappedRowHeight: CGFloat, recentClipboardWrapCharacterCount: Int,
        recentClipboardLimit: Int, maxContentWidth: CGFloat, maxContentHeight: CGFloat,
        bottomInset: CGFloat
    ) {
        self.restingSize = restingSize
        self.initialCapsuleSize = initialCapsuleSize
        self.idleExpandedSize = idleExpandedSize
        self.recordingCompactSize = recordingCompactSize
        self.recordingExpandedSize = recordingExpandedSize
        self.processingSize = processingSize
        self.statusFallbackSize = statusFallbackSize
        self.launchExpandedSize = launchExpandedSize
        self.clipboardCapsuleSize = clipboardCapsuleSize
        self.recentClipboardSize = recentClipboardSize
        self.recentClipboardWrappedRowHeight = recentClipboardWrappedRowHeight
        self.recentClipboardWrapCharacterCount = recentClipboardWrapCharacterCount
        self.recentClipboardLimit = recentClipboardLimit
        self.maxContentWidth = maxContentWidth
        self.maxContentHeight = maxContentHeight
        self.bottomInset = bottomInset
    }

    /// Sizes 1...4096 points, insets 0...256, `recentClipboardLimit` 1...16,
    /// `recentClipboardWrapCharacterCount` 1...4096, and the content maximum never below the
    /// resting size.
    public func validated() -> RightKitPillGeometry {
        typealias L = RightKitPillLimits
        var out = self
        out.restingSize = L.size(restingSize)
        out.initialCapsuleSize = L.size(initialCapsuleSize)
        out.idleExpandedSize = L.size(idleExpandedSize)
        out.recordingCompactSize = L.size(recordingCompactSize)
        out.recordingExpandedSize = L.size(recordingExpandedSize)
        out.processingSize = L.size(processingSize)
        out.statusFallbackSize = L.size(statusFallbackSize)
        out.launchExpandedSize = L.size(launchExpandedSize)
        out.clipboardCapsuleSize = L.size(clipboardCapsuleSize)
        out.recentClipboardSize = L.size(recentClipboardSize)
        out.recentClipboardWrappedRowHeight = L.points(recentClipboardWrappedRowHeight, L.insetRange)
        out.recentClipboardWrapCharacterCount = L.clamp(recentClipboardWrapCharacterCount, 1...4096)
        out.recentClipboardLimit = L.clamp(recentClipboardLimit, 1...16)
        out.maxContentWidth = max(out.restingSize.width, L.points(maxContentWidth, L.sizeRange))
        out.maxContentHeight = max(out.restingSize.height, L.points(maxContentHeight, L.sizeRange))
        out.bottomInset = L.points(bottomInset, L.insetRange)
        return out
    }
}

/// Every deadline, delay and spring the pill's state machines use. Seconds, 0...120 unless noted.
public struct RightKitPillTimings: Sendable, Equatable {
    public var statusDismissDelay: Double
    public var errorDismissDelay: Double
    /// How long the window stays after the pill fades out before it is ordered out.
    public var hideOrderOutDelay: Double
    public var hoverExitDelay: Double
    /// The wait for a hover collapse that has no panel to animate.
    public var hoverExitFallbackDuration: Double
    /// How long a hint or clipboard card needs to finish leaving before the window may shrink.
    public var cardRetireDelay: Double
    public var processingIndicatorDelay: Double
    public var copyLastCopiedHold: Double
    public var copyLastEmptyHold: Double
    public var copyLastFailedHold: Double
    /// Length of the intro bloom. The curve that shapes it is `RightKitPillViews.introIntensity`.
    public var introDuration: Double
    /// Capsule spring: response 0.01...5 s, damping ratio 0.05...5.
    public var capsuleResponse: Double
    public var capsuleDamping: Double
    /// Poll interval of the hover watchdog while the pointer is over the pill, 0.01...1 s.
    public var hoverWatchdogInterval: Double

    public init(
        statusDismissDelay: Double, errorDismissDelay: Double, hideOrderOutDelay: Double,
        hoverExitDelay: Double, hoverExitFallbackDuration: Double, cardRetireDelay: Double,
        processingIndicatorDelay: Double, copyLastCopiedHold: Double, copyLastEmptyHold: Double,
        copyLastFailedHold: Double, introDuration: Double,
        capsuleResponse: Double, capsuleDamping: Double, hoverWatchdogInterval: Double
    ) {
        self.statusDismissDelay = statusDismissDelay
        self.errorDismissDelay = errorDismissDelay
        self.hideOrderOutDelay = hideOrderOutDelay
        self.hoverExitDelay = hoverExitDelay
        self.hoverExitFallbackDuration = hoverExitFallbackDuration
        self.cardRetireDelay = cardRetireDelay
        self.processingIndicatorDelay = processingIndicatorDelay
        self.copyLastCopiedHold = copyLastCopiedHold
        self.copyLastEmptyHold = copyLastEmptyHold
        self.copyLastFailedHold = copyLastFailedHold
        self.introDuration = introDuration
        self.capsuleResponse = capsuleResponse
        self.capsuleDamping = capsuleDamping
        self.hoverWatchdogInterval = hoverWatchdogInterval
    }

    public func validated() -> RightKitPillTimings {
        typealias L = RightKitPillLimits
        var out = self
        out.statusDismissDelay = L.seconds(statusDismissDelay)
        out.errorDismissDelay = L.seconds(errorDismissDelay)
        out.hideOrderOutDelay = L.seconds(hideOrderOutDelay)
        out.hoverExitDelay = L.seconds(hoverExitDelay)
        out.hoverExitFallbackDuration = L.seconds(hoverExitFallbackDuration)
        out.cardRetireDelay = L.seconds(cardRetireDelay)
        out.processingIndicatorDelay = L.seconds(processingIndicatorDelay)
        out.copyLastCopiedHold = L.seconds(copyLastCopiedHold)
        out.copyLastEmptyHold = L.seconds(copyLastEmptyHold)
        out.copyLastFailedHold = L.seconds(copyLastFailedHold)
        out.introDuration = max(0.01, L.seconds(introDuration))
        out.capsuleResponse = L.clamp(L.finite(capsuleResponse, fallback: 0.18), L.responseRange)
        out.capsuleDamping = L.clamp(L.finite(capsuleDamping, fallback: 1), L.dampingRange)
        out.hoverWatchdogInterval = L.clamp(L.finite(hoverWatchdogInterval, fallback: 0.05), 0.01...1)
        return out
    }
}

/// Status wording. The package never writes a word of its own.
public struct RightKitPillLabels: Sendable, Equatable {
    /// Shown for a plain "done" status event.
    public var done: String
    /// Shown for a status event that carries no label.
    public var saved: String
    /// Shown when a status label matches one of `cancelledAliases` (compared lowercased).
    public var cancelled: String
    /// Shown for an error event that carries no label.
    public var error: String
    public var cancelledAliases: [String]

    public init(done: String, saved: String, cancelled: String, error: String, cancelledAliases: [String]) {
        self.done = done
        self.saved = saved
        self.cancelled = cancelled
        self.error = error
        self.cancelledAliases = cancelledAliases
    }

    /// Each label at most 128 characters, at most 8 aliases of at most 64 characters, stored lowercased.
    public func validated() -> RightKitPillLabels {
        typealias L = RightKitPillLimits
        return RightKitPillLabels(
            done: L.text(done, characters: L.labelCharacters),
            saved: L.text(saved, characters: L.labelCharacters),
            cancelled: L.text(cancelled, characters: L.labelCharacters),
            error: L.text(error, characters: L.labelCharacters),
            cancelledAliases: cancelledAliases.prefix(L.aliasCount).map { L.text($0, characters: 64).lowercased() }
        )
    }
}

/// Window levels and Space behaviour. Policy, so the host chooses each.
public struct RightKitPillWindows: Sendable {
    public var pillLevel: NSWindow.Level
    public var pillCollectionBehavior: NSWindow.CollectionBehavior
    public var hubTransitionLevel: NSWindow.Level
    public var hubTransitionCollectionBehavior: NSWindow.CollectionBehavior
    public var splashLevel: NSWindow.Level
    public var splashCollectionBehavior: NSWindow.CollectionBehavior

    public init(
        pillLevel: NSWindow.Level, pillCollectionBehavior: NSWindow.CollectionBehavior,
        hubTransitionLevel: NSWindow.Level, hubTransitionCollectionBehavior: NSWindow.CollectionBehavior,
        splashLevel: NSWindow.Level, splashCollectionBehavior: NSWindow.CollectionBehavior
    ) {
        self.pillLevel = pillLevel
        self.pillCollectionBehavior = pillCollectionBehavior
        self.hubTransitionLevel = hubTransitionLevel
        self.hubTransitionCollectionBehavior = hubTransitionCollectionBehavior
        self.splashLevel = splashLevel
        self.splashCollectionBehavior = splashCollectionBehavior
    }
}

/// The launch splash and the hub transition: sizes, deadlines and the message ladder.
public struct RightKitPillSplashConfiguration: Sendable, Equatable {
    public struct ProgressMessage: Sendable, Equatable {
        public var afterSeconds: Double
        public var text: String
        public init(afterSeconds: Double, text: String) {
            self.afterSeconds = afterSeconds
            self.text = text
        }
    }

    public var size: CGSize
    /// Text shown when the splash starts, and when the engine sends no message.
    public var initialMessage: String
    /// Later messages, applied while the splash waits. At most 8, each `afterSeconds` 0...120.
    public var progressMessages: [ProgressMessage]
    /// Loader steps, 1...32.
    public var stepCount: Int
    public var tickInterval: Double
    public var minimumVisibleDuration: Double
    public var contractionDuration: Double
    /// Extra time after the contraction before the watchdog forces the handoff.
    public var contractionWatchdogSlack: Double
    /// Wall-clock limit after which the handoff counts as stalled and is forced.
    public var handoffDeadline: Double
    /// Seconds between two asks of the host whether startup has stalled.
    public var stallPollInterval: Double
    /// Contraction timing function control points (x1, y1, x2, y2), each 0...1.
    public var contractionTimingControlPoints: [Float]
    public var hubExpansionDuration: Double
    public var hubPillClearance: CGFloat

    public init(
        size: CGSize, initialMessage: String, progressMessages: [ProgressMessage], stepCount: Int,
        tickInterval: Double, minimumVisibleDuration: Double, contractionDuration: Double,
        contractionWatchdogSlack: Double, handoffDeadline: Double, stallPollInterval: Double,
        contractionTimingControlPoints: [Float], hubExpansionDuration: Double, hubPillClearance: CGFloat
    ) {
        self.size = size
        self.initialMessage = initialMessage
        self.progressMessages = progressMessages
        self.stepCount = stepCount
        self.tickInterval = tickInterval
        self.minimumVisibleDuration = minimumVisibleDuration
        self.contractionDuration = contractionDuration
        self.contractionWatchdogSlack = contractionWatchdogSlack
        self.handoffDeadline = handoffDeadline
        self.stallPollInterval = stallPollInterval
        self.contractionTimingControlPoints = contractionTimingControlPoints
        self.hubExpansionDuration = hubExpansionDuration
        self.hubPillClearance = hubPillClearance
    }

    public func validated() -> RightKitPillSplashConfiguration {
        typealias L = RightKitPillLimits
        var out = self
        out.size = L.size(size)
        out.initialMessage = L.text(initialMessage, characters: L.messageCharacters)
        out.progressMessages = progressMessages.prefix(8).map {
            ProgressMessage(
                afterSeconds: L.seconds($0.afterSeconds),
                text: L.text($0.text, characters: L.messageCharacters)
            )
        }
        out.stepCount = L.clamp(stepCount, 1...32)
        out.tickInterval = L.clamp(L.finite(tickInterval, fallback: 0.26), 0.02...5)
        out.minimumVisibleDuration = L.seconds(minimumVisibleDuration)
        out.contractionDuration = L.seconds(contractionDuration)
        out.contractionWatchdogSlack = L.seconds(contractionWatchdogSlack)
        out.handoffDeadline = max(0.1, L.seconds(handoffDeadline))
        out.stallPollInterval = max(0.1, L.seconds(stallPollInterval))
        let points = contractionTimingControlPoints
        out.contractionTimingControlPoints = points.count == 4
            ? points.map { $0.isFinite ? min(max($0, 0), 1) : 0 }
            : [0, 0, 1, 1]
        out.hubExpansionDuration = max(0.01, L.seconds(hubExpansionDuration))
        out.hubPillClearance = L.points(hubPillClearance, L.insetRange)
        return out
    }
}

public struct RightKitPillConfiguration: Sendable {
    public var geometry: RightKitPillGeometry
    public var timings: RightKitPillTimings
    public var labels: RightKitPillLabels
    public var windows: RightKitPillWindows
    public var splash: RightKitPillSplashConfiguration
    /// Number of bars in ``RightKitPillModel/barLevels``, 1...128.
    public var barCount: Int
    /// The hint id that belongs to the clipboard card: dismissing the card also retires it. nil for none.
    public var clipboardLinkedHint: Int32?
    /// Policy: whether a successful copy closes the card, per card kind.
    public var closeSingleClipboardAfterCopy: Bool
    public var closeRecentClipboardAfterCopy: Bool
    /// Seconds before the clipboard card's copied mark, failed mark and delayed close act (0...120).
    public var clipboardCopiedMarkDuration: Double
    public var clipboardFailedMarkDuration: Double
    public var clipboardCloseAfterCopyDelay: Double

    public init(
        geometry: RightKitPillGeometry, timings: RightKitPillTimings, labels: RightKitPillLabels,
        windows: RightKitPillWindows, splash: RightKitPillSplashConfiguration, barCount: Int,
        clipboardLinkedHint: Int32?, closeSingleClipboardAfterCopy: Bool,
        closeRecentClipboardAfterCopy: Bool, clipboardCopiedMarkDuration: Double,
        clipboardFailedMarkDuration: Double, clipboardCloseAfterCopyDelay: Double
    ) {
        self.geometry = geometry
        self.timings = timings
        self.labels = labels
        self.windows = windows
        self.splash = splash
        self.barCount = barCount
        self.clipboardLinkedHint = clipboardLinkedHint
        self.closeSingleClipboardAfterCopy = closeSingleClipboardAfterCopy
        self.closeRecentClipboardAfterCopy = closeRecentClipboardAfterCopy
        self.clipboardCopiedMarkDuration = clipboardCopiedMarkDuration
        self.clipboardFailedMarkDuration = clipboardFailedMarkDuration
        self.clipboardCloseAfterCopyDelay = clipboardCloseAfterCopyDelay
    }

    public func validated() -> RightKitPillConfiguration {
        typealias L = RightKitPillLimits
        var out = self
        out.geometry = geometry.validated()
        out.timings = timings.validated()
        out.labels = labels.validated()
        out.splash = splash.validated()
        out.barCount = L.clamp(barCount, 1...128)
        out.clipboardLinkedHint = clipboardLinkedHint.map { L.clamp($0, L.hintIDRange) }
        out.clipboardCopiedMarkDuration = L.seconds(clipboardCopiedMarkDuration)
        out.clipboardFailedMarkDuration = L.seconds(clipboardFailedMarkDuration)
        out.clipboardCloseAfterCopyDelay = L.seconds(clipboardCloseAfterCopyDelay)
        return out
    }
}

/// The product's views and measuring, supplied as closures. Everything the pill draws comes from here.
@MainActor
public struct RightKitPillViews {
    /// The pill's root SwiftUI view. `setContentSize` must be called with the size the content
    /// wants whenever it changes (a hint or clipboard card owns the window size).
    public var makeRoot: (_ model: RightKitPillModel, _ setContentSize: @escaping @MainActor (CGSize) -> Void) -> AnyView
    /// Registers fonts and other resources the views need. Called once per install, on the main thread.
    public var registerResources: () -> Void
    /// Warms the pill's first render. Called once when the content is attached, on the main thread.
    public var warmContent: () -> Void
    /// The capsule size for a status or error label (the host measures with its own font).
    public var statusCapsuleSize: (_ label: String) -> CGSize
    /// The splash's root SwiftUI view.
    public var makeSplashRoot: (_ model: RightKitSplashModel) -> AnyView
    /// Primes the splash's text rendering. Called on a background queue before the splash window is built.
    public var warmSplash: @Sendable () -> Void
    /// The splash backdrop: corner radius and fill used while it contracts into the pill.
    public var splashBackdrop: () -> (cornerRadius: CGFloat, color: NSColor)
    /// The note shown after a splash action ran, nil for none.
    public var splashActionNote: (_ action: Int32) -> String?
    /// The shell drawn while the hub window grows out of, or shrinks back into, the pill.
    public var makeHubShell: (_ color: NSColor) -> AnyView
    /// Used when the hub window reports no background colour.
    public var hubShellFallbackColor: () -> NSColor
    /// Progress 0...1 in, eased progress out, for the hub frame transition.
    public var hubEasing: (_ progress: Double) -> Double
    /// The animation the pill fades out with, and the one it appears with. Motion is the host's.
    public var hideAnimation: Animation
    public var revealAnimation: Animation
    /// The intro bloom's curve: progress 0...1 through `timings.introDuration` in (always finite and
    /// clamped to 0...1 before the call), bloom intensity out. The shape (easing, peak position) is the
    /// host's product choice; the model clamps the result to 0...1 and treats a non-finite result as 0.
    public var introIntensity: (_ progress: Double) -> Double
    /// Called once, when the intro bloom starts (a sound, for example). nil for none.
    public var introBloomStarted: (() -> Void)?
    /// One frame of the level bars while recording or processing: previous bars in, next bars out
    /// (`barCount` values; the pill pads or cuts any other count).
    public var barLevels: (_ mode: RightKitPillMode, _ level: Double, _ now: CFTimeInterval,
                           _ previous: [Double], _ reduceMotion: Bool) -> [Double]

    public init(
        makeRoot: @escaping (RightKitPillModel, @escaping @MainActor (CGSize) -> Void) -> AnyView,
        registerResources: @escaping () -> Void,
        warmContent: @escaping () -> Void,
        statusCapsuleSize: @escaping (String) -> CGSize,
        makeSplashRoot: @escaping (RightKitSplashModel) -> AnyView,
        warmSplash: @escaping @Sendable () -> Void,
        splashBackdrop: @escaping () -> (cornerRadius: CGFloat, color: NSColor),
        splashActionNote: @escaping (Int32) -> String?,
        makeHubShell: @escaping (NSColor) -> AnyView,
        hubShellFallbackColor: @escaping () -> NSColor,
        hubEasing: @escaping (Double) -> Double,
        hideAnimation: Animation,
        revealAnimation: Animation,
        introIntensity: @escaping (Double) -> Double,
        introBloomStarted: (() -> Void)?,
        barLevels: @escaping (RightKitPillMode, Double, CFTimeInterval, [Double], Bool) -> [Double]
    ) {
        self.makeRoot = makeRoot
        self.registerResources = registerResources
        self.warmContent = warmContent
        self.statusCapsuleSize = statusCapsuleSize
        self.makeSplashRoot = makeSplashRoot
        self.warmSplash = warmSplash
        self.splashBackdrop = splashBackdrop
        self.splashActionNote = splashActionNote
        self.makeHubShell = makeHubShell
        self.hubShellFallbackColor = hubShellFallbackColor
        self.hubEasing = hubEasing
        self.hideAnimation = hideAnimation
        self.revealAnimation = revealAnimation
        self.introIntensity = introIntensity
        self.introBloomStarted = introBloomStarted
        self.barLevels = barLevels
    }
}
#endif
