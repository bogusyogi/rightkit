import Foundation
import RightKitImportQueue
import UniformTypeIdentifiers

/// ScrapeRight's binding of the shared RightKit share-intake queue to its
/// App Group. The extension enqueues; the app drains into the job queue.
enum ShareIntake {
    static let appGroupID = "group.app.scraperight.ios.native"

    static let configuration = ShareIntakeConfiguration(groupID: appGroupID)

    static let accepted = ShareAcceptedTypes(text: false, url: true, files: [
        (.pdf, "pdf"), (.image, "image"), (.movie, "movie"), (.audio, "audio"),
    ])

    static var queue: SharedImportQueue { SharedImportQueue(configuration: configuration) }
}
