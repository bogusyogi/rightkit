#if os(iOS)
import Foundation
import UIKit

/// App delegate adopted for exactly one job: deliver CloudKit silent pushes
/// (the zone subscription RightKitCloudSync creates for the zone) into the
/// coordinator, so a Mac status change reaches the phone while the app sits
/// in the background.
///
/// The SwiftUI lifecycle owns everything else; ScrapeRightApp installs the
/// handler at launch, so a push arriving before the app is wired simply
/// reports `.noData`.
final class AppDelegate: NSObject, UIApplicationDelegate {
    /// Set once by `ScrapeRightApp`. Args: notification payload, completion
    /// to call with the fetch result.
    static var remoteNotificationHandler:
        (([AnyHashable: Any], @escaping (UIBackgroundFetchResult) -> Void) -> Void)?

    func application(
        _ application: UIApplication,
        didReceiveRemoteNotification userInfo: [AnyHashable: Any],
        fetchCompletionHandler completionHandler: @escaping (UIBackgroundFetchResult) -> Void
    ) {
        guard let handler = Self.remoteNotificationHandler else {
            completionHandler(.noData)
            return
        }
        handler(userInfo, completionHandler)
    }
}
#endif
