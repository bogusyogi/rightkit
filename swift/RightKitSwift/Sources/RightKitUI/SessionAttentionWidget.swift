import SwiftUI
import WidgetKit

private struct SessionAttentionView: View {
    let entry: WidgetEntry

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label("Attention", systemImage: "exclamationmark.circle.fill")
                .font(.headline)
            Text("\(entry.snapshot.attentionCount)")
                .font(.largeTitle.weight(.bold))
                .contentTransition(.numericText())
            Text(entry.snapshot.attentionCount == 1 ? "session needs you" : "sessions need you")
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(1)
            Spacer(minLength: 0)
            Link(destination: WidgetLink.sessions()) {
                Label("Open sessions", systemImage: "arrow.up.right")
                    .font(.caption.weight(.semibold))
            }
        }
        .widgetURL(WidgetLink.sessions())
        .containerBackground(.fill.tertiary, for: .widget)
    }
}

struct SessionAttentionWidget: Widget {
    let kind = "CodeRightSessionAttentionWidget"

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: kind, provider: TimelineProvider()) { entry in
            SessionAttentionView(entry: entry)
        }
        .configurationDisplayName("the app Attention")
        .description("See which the app sessions need attention.")
        .supportedFamilies([.systemSmall])
    }
}
