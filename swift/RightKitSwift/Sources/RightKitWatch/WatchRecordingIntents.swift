#if os(watchOS)
import AppIntents
import Foundation

@available(watchOS 11.0, *)
struct ToggleWatchRecordingIntent: AppIntent {
    static var title: LocalizedStringResource = "HeardRight"
    static var description = IntentDescription("Start or stop a recording on your watch.")
    static var openAppWhenRun = true
    @available(watchOS 26.0, *)
    static var supportedModes: IntentModes { [.foreground(.immediate)] }

    init() {}

    @MainActor
    func perform() async throws -> some IntentResult {
        do {
            try await WatchRecordingController.shared.toggle(source: "watch_action_button")
        } catch {
            WatchTelemetry.record("watch_intent_failed", fields: [
                "source": "watch_action_button",
                "error_type": String(reflecting: type(of: error)),
            ])
            throw error
        }
        return .result()
    }
}

@available(watchOS 11.0, *)
struct WatchShortcuts: AppShortcutsProvider {
    static var shortcutTileColor: ShortcutTileColor = .orange

    static var appShortcuts: [AppShortcut] {
        AppShortcut(
            intent: ToggleWatchRecordingIntent(),
            phrases: [
                "Toggle recording in \(.applicationName)",
                "Record with \(.applicationName)",
            ],
            shortTitle: "HeardRight",
            systemImageName: "waveform.circle.fill"
        )
    }
}
#endif
