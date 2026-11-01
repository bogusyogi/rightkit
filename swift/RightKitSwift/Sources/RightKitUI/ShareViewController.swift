#if os(iOS)
import Foundation
import RightKitImportQueue
import UIKit

/// Inbound Share Sheet bridge. RightKit performs bounded text extraction,
/// file staging, durable queueing, & legacy migration; HeardRight owns only
/// product group binding & completion policy.
final class ShareViewController: UIViewController {
    private var didStart = false

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        processOnce()
    }

    private func processOnce() {
        guard !didStart else { return }
        didStart = true
        Task { @MainActor in
            let configuration = ShareIntakeConfiguration(
                groupID: "group.app.heardright.ios.native",
                legacyQueueKey: "heardright.pending.share.payloads",
                legacyPayloadKey: "heardright.pending.share.payload"
            )
            let extractor = ShareItemExtractor(
                configuration: configuration,
                accepted: .documentsAndMedia
            )
            let payload = await extractor.extractFirst(
                from: (extensionContext?.inputItems as? [NSExtensionItem]) ?? []
            )
            guard let payload else {
                extensionContext?.cancelRequest(
                    withError: NSError(domain: "HeardRightShare", code: 2, userInfo: [
                        NSLocalizedDescriptionKey: "HeardRight could not import this item."
                    ])
                )
                return
            }
            do {
                try RightKitImportQueue.SharedImportQueue(configuration: configuration).enqueue(payload)
                extensionContext?.completeRequest(returningItems: [], completionHandler: nil)
            } catch {
                extensionContext?.cancelRequest(
                    withError: NSError(domain: "HeardRightShare", code: 1, userInfo: [
                        NSLocalizedDescriptionKey: "HeardRight could not queue this import.",
                        NSUnderlyingErrorKey: error,
                    ])
                )
            }
        }
    }
}
#endif
