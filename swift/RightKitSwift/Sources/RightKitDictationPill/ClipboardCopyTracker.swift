#if os(macOS)
import Foundation
import SwiftUI

extension RightKitPillModel {
    /// Puts the single card into edit mode and lets the pill window take the keyboard for it.
    public func startSingleClipEditing() {
        singleClipEditing = true
        beginSingleClipEditing()
    }
}

/// The copy, mark and close state of one clipboard card. A view shows `copiedID` and `failedID`
/// and calls `copy`, `commitAndCopy` and `finishEditing`; the rules live here.
///
/// The card that is open when a copy starts owns its result. `model.clipboardEpoch` changes every
/// time a card is set or dismissed, so a card that was closed or replaced since (a late answer, or
/// a timer armed by an earlier copy) has a different epoch and is left alone: the answer neither
/// marks the new card nor closes it. A copy is shown as copied only after the host reported
/// success; on failure the card stays open and shows the failed mark.
///
/// Lifecycle: at most one delayed action is pending (the mark reset, or the close after copy). A
/// newer result cancels it, and it is cancelled when the tracker is released.
@MainActor
public final class RightKitPillClipboardCopyTracker: ObservableObject {
    @Published public private(set) var copiedID = ""
    /// The item whose last copy failed. Never set together with `copiedID`.
    @Published public private(set) var failedID = ""
    private unowned let model: RightKitPillModel
    nonisolated(unsafe) private var pending: DispatchWorkItem?

    public init(model: RightKitPillModel) { self.model = model }

    deinit { pending?.cancel() }

    /// Copies `text` and marks `id` when the host reports the clipboard holds it.
    public func copy(id: String, text: String) {
        let epoch = model.clipboardEpoch
        model.callbacks.copyText(text) { [weak self] copied in
            guard let self, self.model.clipboardEpoch == epoch else { return }
            if copied { self.markCopied(id, epoch: epoch) } else { self.markCopyFailed(id, epoch: epoch) }
        }
    }

    /// Saves an edited `draft` to history when it differs from `original`, then copies the draft.
    public func commitAndCopy(id: String, original: String, draft: String) {
        if draft != original { model.callbacks.updateHistoryItem(id, transcript: draft) }
        copy(id: id, text: draft)
    }

    /// Leaves edit mode, saving `draft` to history (and to the card's row) when it changed.
    public func finishEditing(id: String, original: String, draft: String) {
        if draft != original {
            model.callbacks.updateHistoryItem(id, transcript: draft)
            if let index = model.clipboardItems.firstIndex(where: { $0.deliveryID == id }) {
                model.clipboardItems[index].transcript = draft
            }
        }
        model.endSingleClipEditing()
    }

    /// The copy failed: no copied mark and no close; the card stays so the user can try again.
    private func markCopyFailed(_ id: String, epoch: UInt64) {
        copiedID = ""
        failedID = id
        schedule(after: model.configuration.clipboardFailedMarkDuration, epoch: epoch) { [weak self] in
            if self?.failedID == id { self?.failedID = "" }
        }
    }

    /// Called only after the host reported the clipboard write finished, and only for the card
    /// that started the copy (`epoch`): the delayed close and reset check it again.
    private func markCopied(_ id: String, epoch: UInt64) {
        failedID = ""
        copiedID = id
        if model.closeAfterCopy {
            schedule(after: model.configuration.clipboardCloseAfterCopyDelay, epoch: epoch) { [weak self] in
                self?.model.dismissClipboard()
            }
        } else {
            schedule(after: model.configuration.clipboardCopiedMarkDuration, epoch: epoch) { [weak self] in
                if self?.copiedID == id { self?.copiedID = "" }
            }
        }
    }

    private func schedule(after seconds: Double, epoch: UInt64, _ action: @escaping @MainActor () -> Void) {
        pending?.cancel()
        let item = pillWorkItem { [weak self] in
            guard let self, self.model.clipboardEpoch == epoch else { return }
            self.pending = nil
            action()
        }
        pending = item
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds, execute: item)
    }
}
#endif
