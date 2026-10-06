import ActivityKit
import Foundation

/// Live Activity payload compiled into BOTH the app and the Widgets extension.
struct TemplateActivityAttributes: ActivityAttributes {
    struct ContentState: Codable, Hashable {
        var status: String
    }

    var sessionID: String
}
