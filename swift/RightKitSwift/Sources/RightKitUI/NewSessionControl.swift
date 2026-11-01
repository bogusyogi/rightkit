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
        .displayName("New CodeRight Session")
        .description("Open CodeRight's new-session form.")
    }
}

@available(iOS 18.0, *)
struct NewSessionIntent: AppIntent {
    static let title: LocalizedStringResource = "New CodeRight Session"
    static let openAppWhenRun = true
    static let isDiscoverable = true

    @MainActor
    func perform() async throws -> some IntentResult & OpensIntent {
        .result(opensIntent: OpenURLIntent(URL(string: "coderight://new-session")!))
    }
}
