#if os(iOS)
import SwiftUI

struct MicMeter: View {
    let level: Double
    let ember: Color
    let line: Color
    let muted = Color.hrMuted()

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("Mic level")
                    .font(.caption2.weight(.black))
                    .textCase(.uppercase)
                    .foregroundStyle(muted)
                Spacer()
                Text("\(Int((min(max(level, 0), 1) * 100).rounded()))%")
                    .font(.caption2.weight(.bold))
                    .foregroundStyle(muted)
                    .monospacedDigit()
            }
            GeometryReader { proxy in
                ZStack(alignment: .leading) {
                    Capsule().fill(line.opacity(0.55))
                    Capsule()
                        .fill(ember)
                        .frame(width: proxy.size.width * min(max(level, 0), 1))
                }
            }
            .frame(height: 7)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 2)
        .accessibilityLabel("Microphone level")
        .accessibilityValue("\(Int((min(max(level, 0), 1) * 100).rounded())) percent")
    }
}

struct ComposerPreview: View {
    let transcript: String
    let isRecording: Bool
    let isTranscribing: Bool
    let isModelReady: Bool
    let isModelWarming: Bool
    let level: Double
    let status: String
    let ink: Color
    let muted: Color
    let ember: Color
    let line: Color
    let surface: Color
    let onRecordToggle: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            VStack(spacing: 0) {
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Focused app")
                            .font(.caption2.weight(.black))
                            .textCase(.uppercase)
                            .foregroundStyle(muted)
                        Text("Message")
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(ink)
                    }
                    Spacer()
                    Circle()
                        .fill(line)
                        .frame(width: 6, height: 6)
                    Circle()
                        .fill(line)
                        .frame(width: 6, height: 6)
                    Circle()
                        .fill(line)
                        .frame(width: 6, height: 6)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
                Rectangle()
                    .fill(line.opacity(0.75))
                    .frame(height: 1)
                Color.clear
                    .frame(height: 108)
                HStack(alignment: .bottom, spacing: 8) {
                    Text(composerText)
                        .font(.subheadline.weight(hasDraft ? .semibold : .regular))
                        .foregroundStyle(hasDraft ? ink : muted)
                        .lineLimit(4)
                        .fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: .infinity, minHeight: 52, alignment: .topLeading)
                    Image(systemName: "arrow.up.circle.fill")
                        .font(.title2)
                        .foregroundStyle(hasDraft ? ember : muted.opacity(0.45))
                        .accessibilityHidden(true)
                }
                .padding(12)
                .background(surface, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                .overlay(RoundedRectangle(cornerRadius: 18, style: .continuous).stroke(hasDraft ? ember.opacity(0.55) : line, lineWidth: 1))
                .padding(12)
            }
            .frame(height: 278)
            .background(Color.hrInset, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).stroke(line, lineWidth: 1))

            if isRecording {
                MicMeter(level: level, ember: ember, line: line)
            }

            Button(action: onRecordToggle) {
                Label(recordTitle, systemImage: isRecording ? "stop.fill" : "mic.fill")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(HRPrimaryButtonStyle(disabled: isTranscribing || !isModelReady, ink: ink, ember: ember))
            .disabled(isTranscribing || !isModelReady)

            Text(statusText)
                .font(.caption)
                .foregroundStyle(muted)
        }
        .panel(fill: surface, border: line)
    }

    var composerText: String {
        if !transcript.isEmpty { return transcript }
        if !isModelReady { return "Finish the model download first." }
        if isRecording { return "Listening..." }
        if isTranscribing { return "Transcribing..." }
        return "Your transcript appears here."
    }

    var hasDraft: Bool {
        !transcript.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var recordTitle: String {
        if !isModelReady { return "Waiting for model" }
        if isTranscribing { return "Transcribing" }
        return isRecording ? "Stop" : "Start recording"
    }

    var statusText: String {
        if !isModelReady { return "Finish model setup before trying a recording." }
        if isTranscribing, isModelWarming { return "Loading the local model, then transcribing on this iPhone." }
        if isModelWarming { return "Warming up in the background. You can record now." }
        if isTranscribing { return "Transcribing on this iPhone..." }
        return status
    }
}

enum CleanupLevel {
    case clean
    case aggressive
}

struct PrimaryButtonStyle: ButtonStyle {
    let ink: Color
    let ember: Color

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.subheadline.weight(.black))
            .foregroundStyle(ink)
            .padding(.horizontal, 16)
            .frame(minHeight: 40)
            .background(ember.opacity(configuration.isPressed ? 0.82 : 1), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
            .scaleEffect(configuration.isPressed ? 0.98 : 1)
    }
}

struct SecondaryButtonStyle: ButtonStyle {
    let ink: Color
    let line: Color
    let surface: Color

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.subheadline.weight(.black))
            .foregroundStyle(ink)
            .padding(.horizontal, 16)
            .frame(minHeight: 40)
            .background(surface.opacity(configuration.isPressed ? 0.7 : 1), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).stroke(line, lineWidth: 1))
            .scaleEffect(configuration.isPressed ? 0.98 : 1)
    }
}

extension View {
    func panel(fill: Color, border: Color) -> some View {
        frame(maxWidth: 360)
            .padding(14)
            .background(fill, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous).stroke(border, lineWidth: 1))
    }
}

struct FlowLayout: Layout {
    var spacing: CGFloat = 8

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        rows(for: subviews, width: proposal.width ?? 0).size
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        for row in rows(for: subviews, width: bounds.width).items {
            for item in row.items {
                item.subview.place(
                    at: CGPoint(x: bounds.minX + item.x, y: bounds.minY + row.y),
                    proposal: ProposedViewSize(item.size)
                )
            }
        }
    }

    func rows(for subviews: Subviews, width: CGFloat) -> (items: [(y: CGFloat, items: [(subview: LayoutSubview, x: CGFloat, size: CGSize)])], size: CGSize) {
        var rows: [(y: CGFloat, items: [(subview: LayoutSubview, x: CGFloat, size: CGSize)])] = []
        var current: [(subview: LayoutSubview, x: CGFloat, size: CGSize)] = []
        var x: CGFloat = 0
        var y: CGFloat = 0
        var rowHeight: CGFloat = 0
        let maxWidth = max(width, 1)

        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if x > 0, x + size.width > maxWidth {
                rows.append((y, current))
                y += rowHeight + spacing
                current = []
                x = 0
                rowHeight = 0
            }
            current.append((subview, x, size))
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }

        if !current.isEmpty {
            rows.append((y, current))
            y += rowHeight
        }

        return (rows, CGSize(width: maxWidth, height: y))
    }
}
#endif
