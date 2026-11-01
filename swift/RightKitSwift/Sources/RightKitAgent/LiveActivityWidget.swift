#if os(iOS)
// Provenance: ActivityKit/WidgetKit portion directly transplanted from Bot commit
// 6951c40156ba0916a10b6e49e5051b7785198b93, ios/Widgets/Widgets.swift.
// The @main WidgetBundle is intentionally omitted; integration adds this widget to the
// existing Widgets bundle after target membership is selected.

import ActivityKit
import SwiftUI
import WidgetKit

/// ActivityKit configuration for the app's existing widget extension.
/// Layout mirrors donor expanded, compact, minimal, and lock-screen surfaces.
struct LiveActivityWidget: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: BotActivityAttributes.self) { context in
            LockScreenView(context: context)
                .activityBackgroundTint(.black)
                .activitySystemActionForegroundColor(.white)
        } dynamicIsland: { context in
            DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    OrbitingFace(context: context, size: 68)
                        .padding(.leading, 2)
                }
                DynamicIslandExpandedRegion(.center) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(context.state.headline)
                            .font(.system(size: 15, weight: .semibold))
                            .foregroundStyle(.white)
                        Text(context.state.line)
                            .font(.system(size: 13))
                            .foregroundStyle(.white.opacity(0.7))
                            .lineLimit(2)
                        // The one clock iOS keeps ticking for us.
                        Text(
                            timerInterval: context.state.since ... context.state.since.addingTimeInterval(86_400),
                            countsDown: false
                        )
                        .font(.system(size: 12, weight: .medium).monospacedDigit())
                        .foregroundStyle(.white.opacity(0.5))
                    }
                    .padding(.top, 2)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    if context.state.kind == "needsYou",
                       let requestId = context.state.requestId,
                       !context.state.options.isEmpty {
                        AnswerButtons(context: context, requestId: requestId)
                            .padding(.top, 4)
                    }
                }
            } compactLeading: {
                LiveActivityFaceStill(
                    color: context.attributes.color,
                    state: context.state.face,
                    size: 24
                )
            } compactTrailing: {
                compactTrailing(context)
            } minimal: {
                LiveActivityFaceStill(
                    color: context.attributes.color,
                    state: context.state.face,
                    size: 22
                )
            }
            .keylineTint(LiveActivityPalette.color(context.attributes.color))
        }
    }

    @ViewBuilder
    private func compactTrailing(
        _ context: ActivityViewContext<BotActivityAttributes>
    ) -> some View {
        switch context.state.kind {
        case "needsYou":
            Image(systemName: "hand.raised.fill")
                .font(.system(size: 13, weight: .bold))
                .foregroundStyle(LiveActivityPalette.color(context.attributes.color))
        case "working":
            Image(systemName: "circle.dotted")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(.white.opacity(0.8))
        default:
            Circle()
                .fill(LiveActivityPalette.color(context.attributes.color))
                .frame(width: 8, height: 8)
        }
    }
}

private struct LockScreenView: View {
    let context: ActivityViewContext<BotActivityAttributes>

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            OrbitingFace(context: context, size: 60)
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 6) {
                    if context.state.kind == "needsYou" {
                        Image(systemName: "hand.raised.fill")
                            .font(.system(size: 12, weight: .bold))
                            .foregroundStyle(LiveActivityPalette.color(context.attributes.color))
                    }
                    Text(context.state.headline)
                        .font(.system(size: 15, weight: .semibold))
                        .foregroundStyle(.white)
                }
                Text(context.state.line)
                    .font(.system(size: 13))
                    .foregroundStyle(.white.opacity(0.7))
                    .lineLimit(2)
                if context.state.kind == "needsYou",
                   let requestId = context.state.requestId,
                   !context.state.options.isEmpty {
                    AnswerButtons(context: context, requestId: requestId)
                        .padding(.top, 4)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(14)
    }
}

/// The card's options, as pills — exactly the options the adapter supplied.
private struct AnswerButtons: View {
    let context: ActivityViewContext<BotActivityAttributes>
    let requestId: String

    var body: some View {
        HStack(spacing: 8) {
            ForEach(context.state.options, id: \.self) { option in
                Button(intent: AnswerApprovalIntent(
                    threadId: context.attributes.threadId,
                    requestId: requestId,
                    choice: option,
                    isPermission: context.state.isPermission
                )) {
                    Text(option)
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundStyle(.white)
                        .frame(maxWidth: .infinity)
                        .frame(height: 34)
                        .background(
                            Capsule().fill(
                                option.caseInsensitiveCompare("Deny") == .orderedSame
                                    ? Color.white.opacity(0.16)
                                    : LiveActivityPalette.color(context.attributes.color)
                            )
                        )
                }
                .buttonStyle(.plain)
            }
        }
    }
}

/// The face/ring is a static Live Activity snapshot plus a timer-driven progress ring.
/// The adapter keeps donor face-state strings intact while this local visual shim avoids
/// coupling the widget target to the donor's CompanionCore mascot module.
private struct OrbitingFace: View {
    let context: ActivityViewContext<BotActivityAttributes>
    let size: CGFloat

    var body: some View {
        ZStack {
            ProgressView(
                timerInterval: context.state.since ... context.state.since.addingTimeInterval(60),
                countsDown: false
            ) { EmptyView() } currentValueLabel: { EmptyView() }
            .progressViewStyle(.circular)
            .tint(AngularGradient(colors: [
                Color(red: 0.659, green: 0.333, blue: 0.969),
                Color(red: 0.220, green: 0.741, blue: 0.969),
                Color(red: 0.204, green: 0.827, blue: 0.596),
                Color(red: 0.980, green: 0.792, blue: 0.082),
                Color(red: 0.984, green: 0.573, blue: 0.235),
                Color(red: 0.957, green: 0.247, blue: 0.369),
                Color(red: 0.659, green: 0.333, blue: 0.969),
            ], center: .center))
            .frame(width: size + 4, height: size + 4)
            LiveActivityFaceStill(
                color: context.attributes.color,
                state: context.state.face,
                size: size,
                comets: true,
                at: Date()
            )
        }
    }
}

/// Resting face for a Live Activity snapshot. Raw face state remains adapter-controlled.
private struct LiveActivityFaceStill: View {
    let color: String
    let state: String
    let size: CGFloat
    var comets = false
    var at = Date()

    private var accent: Color { LiveActivityPalette.color(color) }
    private var normalizedState: String { state.lowercased() }
    private var eyeScale: CGFloat {
        ["sleeping", "drowsy", "sad"].contains(normalizedState) ? 0.45 : 1
    }
    private var mouthWidth: CGFloat {
        ["happy", "excited", "celebrate", "playful"].contains(normalizedState) ? 0.34 : 0.24
    }

    var body: some View {
        ZStack {
            if comets {
                ForEach(0..<4, id: \.self) { index in
                    Capsule()
                        .fill(accent.opacity(0.75))
                        .frame(width: size * 0.16, height: max(1, size * 0.045))
                        .offset(x: size * 0.47)
                        .rotationEffect(.degrees(Double(index) * 90 + phase))
                }
            }
            Circle()
                .fill(accent.opacity(0.20))
            Circle()
                .stroke(accent, lineWidth: max(1, size * 0.055))
            HStack(spacing: size * 0.20) {
                Circle()
                    .fill(.white)
                    .frame(width: size * 0.15, height: size * 0.15 * eyeScale)
                Circle()
                    .fill(.white)
                    .frame(width: size * 0.15, height: size * 0.15 * eyeScale)
            }
            .offset(y: -size * 0.08)
            Capsule()
                .fill(.white.opacity(0.9))
                .frame(width: size * mouthWidth, height: max(1, size * 0.045))
                .offset(y: size * 0.20)
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }

    private var phase: Double {
        at.timeIntervalSinceReferenceDate
            .truncatingRemainder(dividingBy: 360)
    }
}

private enum LiveActivityPalette {
    static func color(_ raw: String) -> Color {
        let value = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if let hex = parseHex(value) { return hex }
        switch value.lowercased() {
        case "red", "accent", "coderight":
            return Color(red: 0.898, green: 0.220, blue: 0.333)
        case "green", "success":
            return Color(red: 0.357, green: 0.745, blue: 0.525)
        case "blue":
            return Color(red: 0.220, green: 0.741, blue: 0.969)
        case "purple":
            return Color(red: 0.659, green: 0.333, blue: 0.969)
        default:
            return Color(red: 0.898, green: 0.220, blue: 0.333)
        }
    }

    private static func parseHex(_ raw: String) -> Color? {
        let hex = raw.hasPrefix("#") ? String(raw.dropFirst()) : raw
        guard hex.count == 6, let value = UInt32(hex, radix: 16) else { return nil }
        return Color(
            red: Double((value >> 16) & 0xFF) / 255,
            green: Double((value >> 8) & 0xFF) / 255,
            blue: Double(value & 0xFF) / 255
        )
    }
}
#endif
