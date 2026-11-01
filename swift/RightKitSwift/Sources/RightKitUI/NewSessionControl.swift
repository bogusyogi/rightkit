import AppIntents
import SwiftUI
import WidgetKit

@available(iOS 18.0, *)
struct NewSessionControl: ControlWidget {
    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(kind: "CodeRightNewSessionControl") {
            ControlWidgetButton(action: NewSessionIntent()) {
                Label("New Session", systemImage: "plus.bubble")
            }
        }
        .displayName("New the app Session")
        .description("Open the app's new-session form.")
    }
}

@available(iOS 18.0, *)
struct NewSessionIntent: AppIntent {
    static let title: LocalizedStringResource = "New the app Session"
    static let openAppWhenRun = true
    static let isDiscoverable = true

    @MainActor
    func perform() async throws -> some IntentResult & OpensIntent {
        .result(opensIntent: OpenURLIntent(URL(string: "coderight://new-session")!))
    }
}
