#if os(watchOS)
import AppIntents
import Foundation

/// Shared by Watch app & Control Widget extension. Foreground execution makes
/// Watch app establish its control receiver during app initialization;
/// extension-only `openAppWhenRun` cannot provide that lifecycle guarantee.
@available(iOS 26.0, watchOS 26.0, *)
struct WatchControlIntent: AppIntent {
    static var title: LocalizedStringResource { "HeardRight" }
    static var description = IntentDescription("Start or stop the app recording on your watch.")
    static var openAppWhenRun = true
    @available(watchOS 26.0, *)
    static var supportedModes: IntentModes { [.foreground(.immediate)] }

    func perform() async throws -> some IntentResult {
        CFNotificationCenterPostNotification(
            CFNotificationCenterGetDarwinNotifyCenter(),
            CFNotificationName("app.heardright.watch.toggle-recording" as CFString),
            nil,
            nil,
            true
        )
        return .result()
    }
}
#endif
