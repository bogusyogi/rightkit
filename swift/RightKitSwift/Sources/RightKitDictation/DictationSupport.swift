#if os(iOS)
@preconcurrency import AVFoundation
import Foundation
import os
#if canImport(UIKit)
import UIKit
import UniformTypeIdentifiers
#endif
enum RecordingStopReason {
    case manual
    case interruption
    case routeChanged
    case durationLimit
    case retry

    var readyStatusPrefix: String {
        switch self {
        case .manual:
            return ""
        case .interruption:
            return "Recording interrupted; saved what was captured. "
        case .routeChanged:
            return "Input changed; saved what was captured. "
        case .durationLimit:
            return "Recording limit reached; saved transcript. "
        case .retry:
            return "Retry succeeded. "
        }
    }

    var logName: String {
        switch self {
        case .manual:
            return "manual"
        case .interruption:
            return "interruption"
        case .routeChanged:
            return "route_changed"
        case .durationLimit:
            return "duration_limit"
        case .retry:
            return "retry"
        }
    }
}

#if canImport(UIKit)
/// What the pasteboard reported immediately after a write. These fields prove
/// the write landed in the pasteboard server, not just that the call was made.
struct PasteboardWriteReceipt {
    let changeCountBefore: Int
    let changeCountAfter: Int
    let hasStrings: Bool
    let readbackMatches: Bool

    var traceFields: [String: Any] {
        [
            "pb_change_before": changeCountBefore,
            "pb_change_after": changeCountAfter,
            "pb_has_strings": hasStrings,
            "pb_readback_match": readbackMatches,
        ]
    }
}

extension UIPasteboard {
    /// Copies text without Universal Clipboard sync (`localOnly: true`), which keeps the
    /// transcript from propagating to other devices. Deliberately no `expirationDate`: an
    /// expiring pasteboard entry does not just "disappear" — once it expires, iOS falls back
    /// to the next-newest item, which can be a Universal Clipboard item from another device
    /// (e.g. a Terminal command copied on the user's Mac). That produced a real bug: the
    /// transcript from a "zephyr stop" copy silently vanished after `seconds` and a later
    /// paste pulled in unrelated Mac clipboard content instead. `localOnly` alone is already
    /// privacy-correct for not syncing outward, and does not block local pasting.
    ///
    /// Written with `setItems`, never `setObjects`. Apple documents `setObjects` as creating
    /// item providers, while `setItems` supplies a concrete pasteboard value. Background App
    /// Intents have a short process lifetime, so use the direct representation and record an
    /// immediate readback receipt rather than relying on a provider-backed representation.
    @MainActor
    @discardableResult
    func setPrivateText(_ text: String) -> PasteboardWriteReceipt {
        let before = changeCount
        setItems(
            [[UTType.utf8PlainText.identifier: text]],
            options: [.localOnly: true]
        )
        // Reading back content this process just wrote does not raise the iOS paste
        // banner; the banner covers content written by another app.
        return PasteboardWriteReceipt(
            changeCountBefore: before,
            changeCountAfter: changeCount,
            hasStrings: hasStrings,
            readbackMatches: string == text
        )
    }
}
#endif
#endif
