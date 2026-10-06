import ActivityKit
import SwiftUI
import WidgetKit

@main
struct TemplateWidgetBundle: WidgetBundle {
    var body: some Widget {
        TemplateLiveActivity()
    }
}

/// Live Activity + Dynamic Island skeleton. State is driven from the app via LiveActivityHost.
struct TemplateLiveActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: TemplateActivityAttributes.self) { context in
            Text(context.state.status).padding()
        } dynamicIsland: { context in
            DynamicIsland {
                DynamicIslandExpandedRegion(.center) { Text(context.state.status) }
            } compactLeading: {
                Image(systemName: "circle.fill")
            } compactTrailing: {
                EmptyView()
            } minimal: {
                Image(systemName: "circle.fill")
            }
        }
    }
}
