import SwiftUI
import WidgetKit

private struct LockScreenAttentionView: View {
    let entry: WidgetEntry
    @Environment(\.widgetFamily) private var family

    var body: some View {
        Group {
            if family == .accessoryRectangular {
                Link(destination: WidgetLink.sessions()) {
                    HStack(spacing: 8) {
                        Image(systemName: "exclamationmark.circle.fill")
                        VStack(alignment: .leading, spacing: 1) {
                            Text("CodeRight")
                                .font(.caption.weight(.semibold))
                            Text(attentionLabel)
                                .font(.caption2)
                                .lineLimit(1)
                        }
                    }
                }
            } else {
                Link(destination: WidgetLink.sessions()) {
                    VStack(spacing: 1) {
                        Text("\(entry.snapshot.attentionCount)")
                            .font(.title2.weight(.semibold))
                        Image(systemName: "exclamationmark.circle.fill")
                            .font(.caption)
                    }
                }
            }
        }
        .containerBackground(.clear, for: .widget)
    }

    private var attentionLabel: String {
        entry.snapshot.attentionCount == 0
            ? "No attention needed"
            : "\(entry.snapshot.attentionCount) need attention"
    }
}

struct LockScreenAttentionWidget: Widget {
    let kind = "CodeRightLockScreenAttentionWidget"

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: kind, provider: TimelineProvider()) { entry in
            LockScreenAttentionView(entry: entry)
        }
        .configurationDisplayName("the app Attention")
        .description("See pending the app sessions from your Lock Screen.")
        .supportedFamilies([.accessoryCircular, .accessoryRectangular])
    }
}
