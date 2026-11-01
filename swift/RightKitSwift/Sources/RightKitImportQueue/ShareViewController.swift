#if os(iOS)
import RightKitImportQueue
import Social
import UIKit

/// Headless share extension: queues shared URLs and files in the App
/// Group share-intake queue and completes immediately. The main app drains the inbox
/// into the queue on next launch/foreground. No UI beyond a brief spinner —
/// the queue lives in the app.
final class ShareViewController: UIViewController {
    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .clear
        let spinner = UIActivityIndicatorView(style: .medium)
        spinner.startAnimating()
        spinner.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(spinner)
        NSLayoutConstraint.activate([
            spinner.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            spinner.centerYAnchor.constraint(equalTo: view.centerYAnchor),
        ])
        ingestAttachments()
    }

    private func ingestAttachments() {
        let items = extensionContext?.inputItems as? [NSExtensionItem] ?? []
        Task { @MainActor in
            let extractor = ShareItemExtractor(
                configuration: ShareIntake.configuration, accepted: ShareIntake.accepted)
            let queue = ShareIntake.queue
            for payload in await extractor.extractAll(from: items) {
                try? queue.enqueue(payload)
            }
            finish()
        }
    }

    private func finish() {
        extensionContext?.completeRequest(returningItems: nil)
    }
}
#endif
