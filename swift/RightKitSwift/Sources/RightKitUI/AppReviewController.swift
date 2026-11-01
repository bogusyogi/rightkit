#if os(iOS)
import Foundation
import StoreKit
import UIKit
import Combine

/// Review requests are deliberately sparse & earned by completed recordings.
/// The policy is local-only; no review or usage history leaves the device.
@MainActor
final class AppReviewController: ObservableObject {
    private enum Keys {
        static let successfulRecordings = "heardright.review.successfulRecordings"
        static let lastPromptAt = "heardright.review.lastPromptAt"
    }

    private let defaults: UserDefaults
    private let now: () -> Date
    private let minimumRecordings = 5
    private let minimumPromptInterval: TimeInterval = 90 * 24 * 60 * 60

    init(defaults: UserDefaults = .standard, now: @escaping () -> Date = Date.init) {
        self.defaults = defaults
        self.now = now
    }

    var successfulRecordingCount: Int {
        defaults.integer(forKey: Keys.successfulRecordings)
    }

    /// Records a successful transcript and returns whether policy permits a prompt.
    @discardableResult
    func recordSuccessfulRecording() -> Bool {
        defaults.set(successfulRecordingCount + 1, forKey: Keys.successfulRecordings)
        return shouldRequestReview
    }

    var shouldRequestReview: Bool {
        guard successfulRecordingCount >= minimumRecordings else { return false }
        guard let last = defaults.object(forKey: Keys.lastPromptAt) as? Date else { return true }
        return now().timeIntervalSince(last) >= minimumPromptInterval
    }

    func requestIfAppropriate() {
        guard shouldRequestReview else { return }
        guard let scene = UIApplication.shared.connectedScenes
            .compactMap({ $0 as? UIWindowScene })
            .first(where: { $0.activationState == .foregroundActive }) else { return }
        defaults.set(now(), forKey: Keys.lastPromptAt)
        SKStoreReviewController.requestReview(in: scene)
    }
}
#endif
