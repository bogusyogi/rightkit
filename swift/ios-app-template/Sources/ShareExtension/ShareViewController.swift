import RightKitImportQueue
import UIKit

/// Share extension entry: extract provider items into the App Group queue, then complete. The host
/// app drains the queue on foreground (see ExtensionWiring.drainShareQueue).
final class ShareViewController: UIViewController {
    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground
        let spinner = UIActivityIndicatorView(style: .large)
        spinner.translatesAutoresizingMaskIntoConstraints = false
        spinner.startAnimating()
        view.addSubview(spinner)
        NSLayoutConstraint.activate([
            spinner.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            spinner.centerYAnchor.constraint(equalTo: view.centerYAnchor),
        ])
    }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        let items = extensionContext?.inputItems.compactMap { $0 as? NSExtensionItem } ?? []
        let configuration = TemplateConfig.shareIntake()
        Task { @MainActor in
            let extractor = ShareItemExtractor(configuration: configuration, accepted: .documentsAndMedia)
            let payloads = await extractor.extractAll(from: items)
            let queue = SharedImportQueue(configuration: configuration)
            var failed = payloads.isEmpty
            for payload in payloads {
                do { try queue.enqueue(payload) } catch { failed = true }
            }
            if failed {
                extensionContext?.cancelRequest(withError: CocoaError(.fileWriteUnknown))
            } else {
                extensionContext?.completeRequest(returningItems: nil)
            }
        }
    }
}
