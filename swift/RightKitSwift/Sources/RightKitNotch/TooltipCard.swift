import SwiftUI

/// The regular Liquid Glass material follows the desktop behind it. A dark
/// system appearance is not a guarantee of a dark result: a light wallpaper
/// can still make the card pale enough to erase secondary copy. Keep that
/// adaptive material, but give reading surfaces a stable dark base in this
/// one case.
enum TooltipGlassContrast {
    static func needsReadableDim(surfaceStyle: NotchSurfaceStyle,
                                 colorScheme: ColorScheme,
                                 reduceTransparency: Bool = false) -> Bool {
        surfaceStyle.effective == .glass && colorScheme == .dark && !reduceTransparency
    }

    static func dim(surfaceStyle: NotchSurfaceStyle, colorScheme: ColorScheme,
                    reduceTransparency: Bool = false) -> Color? {
        if surfaceStyle.effective == .darkGlass {
            return Palette.darkGlassTooltipDim
        }
        return needsReadableDim(surfaceStyle: surfaceStyle, colorScheme: colorScheme,
                                reduceTransparency: reduceTransparency)
            ? Palette.liquidGlassTooltipDim : nil
    }

    static func secondaryInk(surfaceStyle: NotchSurfaceStyle, colorScheme: ColorScheme,
                             reduceTransparency: Bool = false) -> Color {
        needsReadableDim(surfaceStyle: surfaceStyle, colorScheme: colorScheme,
                         reduceTransparency: reduceTransparency)
            ? Palette.readableTooltipTextSecondary : Palette.textSecondary
    }
}

/// The card chrome every tooltip shares: fixed width, the frame's padding and a
/// clip to the card's corner. What is behind it — the silhouette with its tail,
/// the glass, the way in and out of the notch — is `NoticeStage`'s: the hover
/// card is the toast's own object on shorter clocks (liquid-motion pass §2).
private struct TooltipShell<Content: View>: View {
    /// Given explicitly rather than left to the contents.
    ///
    /// Sized by its contents, the card's height changes the instant they do —
    /// and the tail, centred on that height, jumps with it while the card's
    /// position is still gliding. The two halves then visibly come apart.
    let height: CGFloat
    @ViewBuilder let content: Content

    var body: some View {
        // The same arrangement that makes the notch fold work: the contents
        // are laid out once at their natural size and never move, and it is
        // the *mask* that changes size over them.
        //
        // The obvious alternative — putting the contents inside a frame of
        // the animating height — makes SwiftUI re-align them on every frame
        // of the animation, so the rows drift vertically inside the card and
        // the top ones slide out under the clip. Nothing should move here
        // except the boundary.
        ZStack(alignment: .top) {
            content
                .padding(NotchLayout.cardPadding)
                .frame(width: NotchLayout.cardWidth, alignment: .topLeading)
        }
        .frame(width: NotchLayout.cardWidth, height: height, alignment: .top)
        .clipShape(RoundedRectangle(cornerRadius: NoticeMetrics.corner, style: .circular))
    }
}

private struct TooltipHeader<Mark: View>: View {
    let title: String
    /// The account's named tier, when the provider publishes one.
    var subtitle: String?
    /// Pulse fork: under the subtitle, in the same quiet style, only when the
    /// numbers are out of date or the last read failed. See
    /// `ProviderSnapshot.freshnessLine`.
    var freshness: String?
    /// Sits on the header's own line, so saying when a reading was taken costs
    /// the card no extra height.
    var note: String?
    /// Pulse fork: the Claude card's "Sync chats to your other accounts" button, at the far right.
    var showsClaudeSync = false
    /// The account this ring belongs to (#476): its chip is the ticked one.
    var accountID: String?
    @ViewBuilder let mark: Mark
    @Environment(\.tooltipSecondaryInk) private var secondaryInk

    var body: some View {
        HStack(alignment: .center, spacing: NotchLayout.headerGap) {
            mark
            VStack(alignment: .leading, spacing: 0) {
                HStack(spacing: 0) {
                    Text(title)
                        .font(Typography.cardTitle)
                        .foregroundStyle(Palette.textPrimary)
                        // The title names the model; a long note yields before it does.
                        .layoutPriority(1)
                    if let note {
                        Spacer(minLength: Design.px(20))
                        Text(note)
                            .font(Typography.cardBody)
                            .foregroundStyle(secondaryInk)
                            .lineLimit(1)
                    }
                }
                if let subtitle {
                    Text(subtitle)
                        .font(Typography.cardBody)
                        .foregroundStyle(secondaryInk)
                        .lineLimit(1)
                }
                if let freshness {
                    Text(freshness)
                        .font(Typography.cardBody)
                        .foregroundStyle(secondaryInk)
                        .lineLimit(1)
                }
            }
            if showsClaudeSync {
                Spacer(minLength: 0)
                ClaudeAccountSwitcherRow(accountID: accountID)
                ClaudeChatSyncButton()
            }
        }
    }
}

/// A label on the left and a quieter value on the right — the row shape the
/// design frame uses throughout.
struct SplitRow<Accessory: View>: View {
    let leading: String
    let trailing: String
    var leadingColor: Color = Palette.textPrimary
    var trailingColor: Color? = nil
    /// Sits immediately before the trailing text, inside the same group, so it
    /// travels with the word instead of drifting to the middle of the row.
    @ViewBuilder var accessory: () -> Accessory
    @Environment(\.tooltipSecondaryInk) private var secondaryInk

    var body: some View {
        HStack(spacing: Design.px(20)) {
            Text(leading).foregroundStyle(leadingColor)
            Spacer(minLength: 0)
            HStack(spacing: NotchLayout.statusDotGap) {
                accessory()
                Text(trailing).foregroundStyle(trailingColor ?? secondaryInk)
            }
        }
        .font(Typography.cardBody)
        .lineLimit(1)
    }
}

extension SplitRow where Accessory == EmptyView {
    init(leading: String,
         trailing: String,
         leadingColor: Color = Palette.textPrimary,
         trailingColor: Color? = nil) {
        self.init(leading: leading, trailing: trailing,
                  leadingColor: leadingColor, trailingColor: trailingColor,
                  accessory: { EmptyView() })
    }
}

/// The ring beside a session's status.
///
/// Turning while the agent is working, still when it is not — so the row says
/// what is happening before the word is read.
private struct StatusRing: View {
    let state: AgentSession.State
    let color: Color

    /// Pulse always animates; the system Reduce Motion setting is ignored (Adrian, 2026-10-11).
    private let reduceMotion = false

    /// One turn, in seconds. Slow enough to read as deliberate rather than as
    /// something struggling.
    private static let period: Double = 1.4

    var body: some View {
        Group {
            switch state {
            case .busy:
                if reduceMotion {
                    // Still, but still three-quarters: the gap alone says
                    // "in progress" without anything moving.
                    ring(trim: 0.75)
                } else {
                    // A timeline rather than `repeatForever`. An endless
                    // animation has to be cancelled to stop, and setting the
                    // value it is already heading towards does not cancel it —
                    // which is exactly how the refresh ring here once span for
                    // ever. Derived from the clock, it simply stops being drawn.
                    TimelineView(.animation) { context in
                        ring(trim: 0.75)
                            .rotationEffect(.degrees(angle(at: context.date)))
                    }
                }
            case .waiting, .success:
                // Half a ring, held still: blocked, not progressing. (Or a full ring for success).
                // Wait, if we want success to be a full ring, we can use 1.0 trim for success.
                ring(trim: state == .success ? 1.0 : 0.5)
            case .idle:
                ring(trim: 1)
            }
        }
        .frame(width: NotchLayout.statusDot, height: NotchLayout.statusDot)
    }

    private func ring(trim: CGFloat) -> some View {
        Circle()
            .trim(from: 0, to: trim)
            .stroke(color,
                    style: StrokeStyle(lineWidth: NotchLayout.statusDotStroke, lineCap: .round))
            // Start the gap at the top, where the eye lands first.
            .rotationEffect(.degrees(-90))
    }

    private func angle(at date: Date) -> Double {
        let turns = date.timeIntervalSinceReferenceDate / Self.period
        return turns.truncatingRemainder(dividingBy: 1) * 360
    }
}

// MARK: - Providers

/// One metered window: label and reset copy on a line, a track bar, then the
/// percentage burned.
private struct LimitWindowRow: View {
    let window: LimitWindow
    var inset: CGFloat = 0
    let fidelity: Fidelity
    let now: Date
    let resetTimeFormat: ResetTimeFormat
    let showsUsagePace: Bool
    @Environment(\.codenotchAccentColor) private var accentColor
    @Environment(\.usageWatchLimit) private var watchLimit
    @Environment(\.usageCriticalLimit) private var criticalLimit
    @Environment(\.colorTransitionStyle) private var colorTransitionStyle
    @Environment(\.tooltipSecondaryInk) private var secondaryInk
    @Environment(\.viewShotStill) private var still

    /// A caption that fits keeps its size. `ImageRenderer` lays a view out once and drew some of
    /// these at the 0.85 floor though the line is half the column (the same text full size in a
    /// sibling row), so a still never scales; the live card keeps the floor for a line that does
    /// not fit.
    private var captionScale: CGFloat { still ? 1 : 0.85 }

    private var band: UsageBand {
        if let override = window.bandOverride { return override }
        return UsageBand.band(for: window.usedFraction ?? 0, watchLimit: watchLimit, criticalLimit: criticalLimit)
    }
    /// Continuous when that style is chosen; a `bandOverride` is a deliberate discrete choice
    /// from the caller regardless of style, so it stays exactly as `band.color(accent:)` renders it.
    private var barColor: Color {
        guard window.bandOverride == nil, colorTransitionStyle == .ramp else {
            return band.color(accent: accentColor)
        }
        return UsageBand.rampColor(for: window.usedFraction ?? 0, watchLimit: watchLimit, accent: accentColor)
    }
    private var trackWidth: CGFloat { NotchLayout.cardWidth - 2 * NotchLayout.cardPadding - inset }
    private var fillWidth: CGFloat {
        let fraction = CGFloat(min(max(window.usedFraction ?? 0, 0), 1))
        return max(NotchLayout.barHeight, trackWidth * fraction)
    }

    private var paceText: Text {
        guard showsUsagePace, let pace = window.usagePace(now: now) else {
            return Text("")
        }
        return Text(" · \(pace.summary)")
            .foregroundColor(pace.isDeficit ? .orange : secondaryInk)
    }

    /// Blank rather than invented: some providers never say when the window rolls.
    private var resetText: String {
        window.resetsAt.map { ResetCopy.text(for: $0, now: now, format: resetTimeFormat) } ?? ""
    }

    /// A count-only row (no fraction, no reset) — like Ollama's per-model request
    /// counts — renders as a single table line: name left, count right.
    private var isCountRow: Bool {
        window.usedFraction == nil && (window.used != nil || window.detail != nil)
    }

    /// Pulse fork: a nearby device in the Send cell's card; a click makes it the target.
    private var nearbyDevice: String? {
        window.id.hasPrefix("nearby:") ? String(window.id.dropFirst("nearby:".count)) : nil
    }

    var body: some View {
        if window.id == NearbySharing.boxesRowID {
            // Pulse fork (#471): "Paste and send to", one box per computer.
            SendBoxGrid(signature: window.detail ?? "")
        } else if window.id == NearbySharing.tilesRowID {
            // Pulse fork (#471): "Received", the last 6 items, 3 per row.
            SendTileGrid(signature: window.detail ?? "")
        } else if window.id.hasPrefix("tool:") {
            // Pulse fork: the Tools cell's card. The first tool row draws the whole grid of
            // equal buttons from the shared action list; the other tool rows draw nothing.
            if window.id == "tool:" + (ToolKit.shared.tools.first?.id ?? "") {
                let columns = [GridItem(.flexible(), spacing: 8), GridItem(.flexible(), spacing: 8)]
                LazyVGrid(columns: columns, spacing: 8) {
                    ForEach(ToolKit.shared.tools, id: \.id) { tool in
                        sendBarButton(icon: tool.symbol, title: tool.title,
                                      enabled: tool.enabled(), help: tool.help) {
                            ToolKit.shared.run(tool.id)
                        }
                    }
                }
            } else {
                EmptyView()
            }
        } else if window.id == NearbySharing.copyLastRowID {
            Button {
                MainActor.assumeIsolated { NearbySharing.shared.copyLast() }
            } label: {
                rows.contentShape(Rectangle())
            }
            .buttonStyle(CardButtonStyle())
        } else if window.id == NearbySharing.pasteRowID {
            Button {
                MainActor.assumeIsolated { NearbySharing.shared.pasteClipboard() }
            } label: {
                rows.contentShape(Rectangle())
            }
            .buttonStyle(CardButtonStyle())
        } else {
            rows
                .contentShape(Rectangle())
                .cardRowHover(enabled: nearbyDevice != nil)
                .onTapGesture {
                    if let device = nearbyDevice {
                        MainActor.assumeIsolated { NearbySharing.shared.select(device) }
                    }
                }
        }
    }

    /// One of the Send bar's equal buttons.
    private func sendBarButton(icon: String, title: String, enabled: Bool, help: String,
                               action: @escaping @MainActor () -> Void) -> some View {
        Button {
            if enabled { MainActor.assumeIsolated { action() } }
        } label: {
            HStack(spacing: 6) {
                Image(systemName: icon)
                Text(title).lineLimit(1)
            }
            .font(Typography.cardBody)
            .foregroundStyle(Palette.textPrimary)
            .padding(.vertical, 4)
            .padding(.horizontal, 8)
            .frame(maxWidth: .infinity)
            .background(RoundedRectangle(cornerRadius: 6).fill(Palette.barTrack))
            .opacity(enabled ? 1 : 0.4)
            .contentShape(Rectangle())
        }
        .buttonStyle(CardButtonStyle())
        .disabled(!enabled)
        .help(help)
        .accessibilityLabel(title)
    }

    @ViewBuilder private var rows: some View {
        if window.id == NearbySharing.copyLastRowID {
            // Pulse fork: the Send card's "Copy last" button.
            HStack(spacing: 6) {
                Image(systemName: "doc.on.clipboard")
                Text(window.label).lineLimit(1)
                Spacer(minLength: 4)
                Text(window.detail ?? "").foregroundStyle(secondaryInk).lineLimit(1)
            }
            .font(Typography.cardBody)
            .foregroundStyle(Palette.textPrimary)
            .padding(.vertical, 4)
            .padding(.horizontal, 8)
            .frame(maxWidth: .infinity)
            .background(RoundedRectangle(cornerRadius: 6).fill(Palette.barTrack))
        } else if window.id == NearbySharing.pasteRowID {
            // Pulse fork: the Send card's paste button.
            HStack(spacing: 6) {
                Image(systemName: "doc.on.clipboard")
                Text(window.label)
            }
            .font(Typography.cardBody)
            .foregroundStyle(Palette.textPrimary)
            .padding(.vertical, 4)
            .frame(maxWidth: .infinity)
            .background(RoundedRectangle(cornerRadius: 6).fill(Palette.barTrack))
        } else if let money = window.money {
            MoneyBreakdownView(title: window.label, money: money, fidelity: fidelity)
        } else if isCountRow {
            if window.label.isEmpty, let line = window.detail {
                // A plain one-line note (drive health): left-aligned, no label column.
                Text(line)
                    .font(Typography.cardBody)
                    .foregroundStyle(Palette.textPrimary)
                    .lineLimit(1)
                    .minimumScaleFactor(captionScale)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                SplitRow(leading: window.label, trailing: window.detail ?? window.usedText ?? "\(window.used ?? 0)")
            }
        } else {
            VStack(alignment: .leading, spacing: 0) {
                SplitRow(leading: window.label, trailing: window.trailingText ?? resetText)

                // No bar without a denominator — an empty track would read as "none
                // used", which is not what "we do not know the limit" means.
                if window.usedFraction != nil {
                    ZStack(alignment: .leading) {
                        Capsule().fill(Palette.barTrack)
                        Capsule().fill(barColor).frame(width: fillWidth)
                    }
                    .frame(width: trackWidth, height: NotchLayout.barHeight)
                    .padding(.top, NotchLayout.labelToBar)
                }

                Text("\(window.usedFraction == nil ? "" : fidelity.qualifier)\(window.detail ?? window.summary)\(paceText)")
                    .font(Typography.cardBody)
                    .foregroundStyle(Palette.textPrimary)
                    .lineLimit(1)
                    .minimumScaleFactor(captionScale)
                    .padding(.top, NotchLayout.barToUsed)
            }
        }
    }
}

private struct MoneyBreakdownView: View {
    let title: String
    let money: UsageMoneyBreakdown
    let fidelity: Fidelity
    @Environment(\.codenotchAccentColor) private var accentColor
    @Environment(\.usageWatchLimit) private var watchLimit
    @Environment(\.usageCriticalLimit) private var criticalLimit
    @Environment(\.colorTransitionStyle) private var colorTransitionStyle

    private var barColor: Color {
        guard colorTransitionStyle == .ramp else {
            return UsageBand.band(for: money.spentFraction, watchLimit: watchLimit, criticalLimit: criticalLimit)
                .color(accent: accentColor)
        }
        return UsageBand.rampColor(for: money.spentFraction, watchLimit: watchLimit, accent: accentColor)
    }

    private var symbol: String {
        switch money.currency.uppercased() {
        case "CNY", "RMB", "JPY": return "¥"
        case "USD": return "$"
        case "EUR": return "€"
        default: return "\(money.currency) "
        }
    }

    private func amount(_ value: Double) -> String {
        "\(symbol)\(String(format: "%.2f", locale: Locale(identifier: "en_US_POSIX"), value))"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            SplitRow(leading: title,
                     trailing: "\(fidelity.qualifier)\(Percent.text(for: money.spentFraction))% used")
            GeometryReader { proxy in
                HStack(spacing: 0) {
                    Rectangle()
                        .fill(barColor)
                        .frame(width: proxy.size.width * CGFloat(money.spentFraction))
                    Rectangle().fill(Palette.barTrack)
                }
            }
            .frame(width: NotchLayout.cardTextWidth, height: NotchLayout.moneyBarHeight)
            .clipShape(Capsule())
            .padding(.top, NotchLayout.labelToBar)

            HStack(spacing: NotchLayout.blockSpacing) {
                MoneyStat(label: L10n.t("Spent"), value: amount(money.spent), color: accentColor)
                MoneyStat(label: L10n.t("Remaining"), value: amount(money.remaining))
                MoneyStat(label: L10n.t("Funded"), value: amount(money.funded), color: Palette.textPrimary)
            }
            .frame(width: NotchLayout.cardTextWidth)
            .padding(.top, NotchLayout.moneyBarToStats)
        }
    }
}

private struct MoneyStat: View {
    let label: String
    let value: String
    var color: Color? = nil
    @Environment(\.tooltipSecondaryInk) private var secondaryInk

    var body: some View {
        VStack(alignment: .leading, spacing: NotchLayout.moneyStatGap) {
            Text(label).foregroundStyle(secondaryInk).lineLimit(1)
            Text(value).foregroundStyle(color ?? secondaryInk).monospacedDigit().lineLimit(1)
        }
        .font(Typography.cardBody)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct ProviderTooltip: View {
    /// What a local model is doing right now, for the header's note.
    var activityNote: String?
    let snapshot: ProviderSnapshot
    let now: Date
    let resetTimeFormat: ResetTimeFormat
    let showUsagePace: Bool
    @Environment(\.tooltipSecondaryInk) private var secondaryInk

    /// Only worth saying when the numbers are not current. A remembered reading
    /// has to be dated, or it quietly passes itself off as live.
    private var readingAge: String? {
        guard snapshot.hasReading, let since = snapshot.status.staleSince,
              since != .distantPast
        else { return nil }
        return ElapsedCopy.ago(since: since, now: now)
    }

    /// Pulse fork: how current the numbers are, when that is worth saying. It
    /// says the age itself, so the header's age note is dropped while it shows.
    private var freshness: String? { snapshot.freshnessLine(now: now) }

    private struct WindowGroup: Identifiable {
        let id: String
        let title: String?
        var windows: [LimitWindow]
    }

    private var groupedWindows: [WindowGroup] {
        var result: [WindowGroup] = []
        for window in snapshot.windows {
            if let last = result.last, last.title == window.group {
                result[result.count - 1].windows.append(window)
            } else {
                result.append(WindowGroup(id: window.group ?? window.id, title: window.group, windows: [window]))
            }
        }
        return result
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            TooltipHeader(title: snapshot.glyph == .send || snapshot.glyph == .tools
                              ? snapshot.displayName : L10n.t("\(snapshot.displayName) Usage"),
                          subtitle: snapshot.subtitle,
                          freshness: freshness,
                          note: snapshot.headerAccessory ?? activityNote ?? (freshness == nil ? readingAge : nil),
                          showsClaudeSync: snapshot.glyph == .claude,
                          accountID: snapshot.accountID) {
                ProviderGlyphView(glyph: snapshot.glyph)
                    .foregroundStyle(Palette.textPrimary)
            }

            if let block = snapshot.block {
                BlockedRow(text: NoticeText.bound(block.summary(now: now), NoticeText.detailLimit))
                    .padding(.top, NotchLayout.headerToBlock)
            }

            if let message = snapshot.statusMessage {
                Text(NoticeText.bound(message, NoticeText.detailLimit))
                    .font(Typography.cardBody)
                    .foregroundStyle(secondaryInk)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.top, NotchLayout.headerToBlock)
            } else {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(groupedWindows.enumerated()), id: \.element.id) { groupIndex, group in
                        if let title = group.title {
                            VStack(alignment: .leading, spacing: Design.px(12)) {
                                Text(title)
                                    .font(Typography.cardBody)
                                    .fontWeight(.semibold)
                                    .foregroundStyle(Palette.textPrimary)
                                    .padding(.leading, Design.px(4))

                                VStack(alignment: .leading, spacing: NotchLayout.blockSpacing) {
                                    ForEach(Array(group.windows.enumerated()), id: \.element.id) { windowIndex, window in
                                        LimitWindowRow(window: window, inset: 2 * Design.px(16), fidelity: snapshot.fidelity, now: now, resetTimeFormat: resetTimeFormat, showsUsagePace: showUsagePace)
                                            .padding(.top, windowIndex == 0 ? 0 : NotchLayout.blockSpacing)
                                    }
                                }
                                .padding(Design.px(16))
                                .overlay(
                                    RoundedRectangle(cornerRadius: Design.px(20))
                                        .stroke(Color.white.opacity(0.25), lineWidth: Design.px(1.5))
                                )
                            }
                            .padding(.top, groupIndex == 0 ? NotchLayout.headerToBlock : Design.px(28))
                        } else {
                            ForEach(Array(group.windows.enumerated()), id: \.element.id) { windowIndex, window in
                                LimitWindowRow(window: window, fidelity: snapshot.fidelity, now: now, resetTimeFormat: resetTimeFormat, showsUsagePace: showUsagePace)
                                    .padding(.top, (groupIndex == 0 && windowIndex == 0) ? NotchLayout.headerToBlock : NotchLayout.blockSpacing)
                            }
                        }
                    }
                }
                .padding(.bottom, groupedWindows.contains(where: { $0.title != nil }) ? Design.px(8) : 0)
            }
        }
    }
}

enum UsageFormat {
    static func tokens(_ value: Int?) -> String {
        guard let value else { return "—" }
        switch value {
        case 1_000_000_000...:
            return String(format: "%.2fB", locale: Locale(identifier: "en_US_POSIX"),
                          Double(value) / 1_000_000_000)
        case 1_000_000...:
            return String(format: "%.1fM", locale: Locale(identifier: "en_US_POSIX"),
                          Double(value) / 1_000_000)
        case 1_000...:
            return String(format: "%.0fK", locale: Locale(identifier: "en_US_POSIX"),
                          Double(value) / 1_000)
        default:
            return "\(value)"
        }
    }

    static func duration(seconds: Double?) -> String {
        guard let seconds, seconds.isFinite, seconds > 0 else { return "—" }
        let minutes = max(1, Int((seconds / 60).rounded()))
        let hours = minutes / 60
        let remainder = minutes % 60
        if hours > 0 {
            return remainder == 0 ? "\(hours)h" : "\(hours)h \(remainder)m"
        }
        return "\(minutes)m"
    }

    static func days(_ value: Int?) -> String {
        guard let value else { return "—" }
        return "\(value)d"
    }
}

/// Unused rate-limit resets on this account.
private struct UsageResetCreditsSection: View {
    let credits: UsageResetCredits
    let now: Date
    @Environment(\.tooltipSecondaryInk) private var secondaryInk

    private var countText: String {
        switch credits.availableCount {
        case 0: return L10n.t("No unused resets")
        case 1: return L10n.t("1 unused reset")
        case let n: return L10n.t("\(n) unused resets")
        }
    }

    private var observedCountText: String {
        guard let checkedAt = credits.checkedAt else { return countText }
        return L10n.t("\(countText) · \(ElapsedCopy.ago(since: checkedAt, now: now))")
    }

    private var expiryText: String? {
        guard credits.availableCount > 0, let date = credits.nextExpiry, date > now else {
            return nil
        }
        let stamp = Self.stamp(for: date, now: now)
        return credits.availableCount > 1
            ? L10n.t("Next expires \(stamp)")
            : L10n.t("Expires \(stamp)")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Rectangle()
                .fill(Palette.ringTrack)
                .frame(height: NotchLayout.hairline)
                .padding(.top, NotchLayout.codexUsageTop)

            VStack(alignment: .leading, spacing: 0) {
                Text(credits.checkedAt == nil
                     ? L10n.t("Unused resets") : L10n.t("Unused resets (cached)"))
                    .font(Typography.cardBody)
                    .fontWeight(.semibold)
                    .foregroundStyle(Palette.textPrimary)
                    .padding(.top, NotchLayout.blockSpacing)

                Text(observedCountText)
                    .font(Typography.cardBody)
                    .foregroundStyle(Palette.textPrimary)
                    .lineLimit(1)
                    .minimumScaleFactor(0.8)
                    .padding(.top, NotchLayout.codexUsageRowGap)

                if let expiryText {
                    Text(expiryText)
                        .font(Typography.cardBody)
                        .foregroundStyle(secondaryInk)
                        .lineLimit(1)
                        .padding(.top, NotchLayout.codexUsageRowGap)
                }
            }
            .frame(height: NotchLayout.codexResetCreditsHeight, alignment: .top)
        }
    }

    /// Same date templates `ResetCopy` uses past the hour, so this line and
    /// the quota rows agree on what "soon" looks like.
    private static func stamp(for date: Date, now: Date, calendar: Calendar = .current) -> String {
        let formatter = ResetCopy.formatter(for: calendar)
        formatter.locale = L10n.locale
        if ResetCopy.daysApart(from: now, to: date, calendar: calendar) >= 7 {
            formatter.setLocalizedDateFormatFromTemplate("MMM d")
        } else {
            formatter.setLocalizedDateFormatFromTemplate("E j:mm")
        }
        return formatter.string(from: date)
    }
}

/// The line that says you are stopped.
///
/// Deliberately loud where the rest of the card is quiet: it is the one thing
/// here that changes what you can do next, and it can be true while the
/// percentage beside it still reads comfortable.
private struct BlockedRow: View {
    let text: String

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: NotchLayout.statusDotGap) {
            Image(systemName: "pause.circle.fill")
                .font(.system(size: NotchLayout.statusDot))
            Text(text)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .font(Typography.cardBody)
        .foregroundStyle(Palette.critical)
    }
}

// MARK: - Activity

private struct SessionRow: View {
    let session: AgentSession
    let now: Date
    /// Set when rows can be clicked to jump to the session's terminal.
    var onFocus: ((pid_t) -> Void)? = nil
    @Environment(\.codenotchAccentColor) private var accentColor
    @Environment(\.tooltipSecondaryInk) private var secondaryInk

    private var stateColor: Color {
        switch session.state {
        case .busy:    return Palette.textPrimary
        case .waiting: return Palette.watch
        case .success: return Palette.ample
        case .idle:    return secondaryInk
        }
    }

    private var stateWord: String {
        switch session.state {
        case .busy:    return L10n.t("working")
        case .waiting: return L10n.t("waiting")
        case .success: return L10n.t("complete")
        case .idle:    return L10n.t("idle")
        }
    }

    /// While blocked, what it is blocked on matters more than where it lives.
    private var detail: String {
        if session.state == .waiting, let waitingFor = session.waitingFor, !waitingFor.isEmpty {
            return waitingFor
        }
        return session.detail
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            SplitRow(leading: session.name, trailing: stateWord,
                     trailingColor: stateColor) {
                StatusRing(state: session.state, color: stateColor)
            }
            SplitRow(
                leading: detail,
                trailing: ElapsedCopy.text(since: session.since, now: now),
                leadingColor: secondaryInk
            )
            .padding(.top, NotchLayout.sessionRowGap)
        }
        // Sessions that publish a pid can be jumped to; the rest are text,
        // and a gesture on them would promise something it cannot do.
        .contentShape(Rectangle())
        .onTapGesture {
            guard let pid = session.processID else { return }
            onFocus?(pid)
        }
    }
}

/// The live sessions for this provider, under a rule that separates them from
/// the limit windows above — they answer a different question.
private struct SessionList: View {
    let summary: ActivitySummary
    let now: Date
    /// How many rows this screen has room for; the rest are counted.
    let cap: Int
    var onFocus: ((pid_t) -> Void)? = nil
    @Environment(\.tooltipSecondaryInk) private var secondaryInk

    /// Busy sessions first, so what is hidden is what matters least.
    private var ordered: [AgentSession] {
        summary.sessions.sorted { a, b in
            let rank: (AgentSession) -> Int = {
                switch $0.state { case .waiting: 0; case .busy: 1; case .success: 2; case .idle: 3 }
            }
            return rank(a) == rank(b) ? a.since > b.since : rank(a) < rank(b)
        }
    }

    private var shown: [AgentSession] { Array(ordered.prefix(max(0, cap))) }
    private var hidden: Int { max(0, summary.sessions.count - shown.count) }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Rectangle()
                .fill(Palette.ringTrack)
                .frame(height: NotchLayout.hairline)
                .padding(.top, NotchLayout.blockSpacing)

            // Only as many as the card's budgeted height can hold. The rest
            // are counted rather than drawn: the card is clipped, not scrolled,
            // so anything past the budget silently pushes the title off the top.
            ForEach(Array(shown.enumerated()), id: \.element.id) { index, session in
                SessionRow(session: session, now: now, onFocus: onFocus)
                    .padding(.top, NotchLayout.blockSpacing)
            }

            if hidden > 0 {
                Text(L10n.t("and \(hidden) more"))
                    .font(Typography.cardBody)
                    .foregroundStyle(secondaryInk)
                    .padding(.top, NotchLayout.blockSpacing)
            }
        }
    }
}

// MARK: - Entry point

struct TooltipCard: View {
    let snapshot: ProviderSnapshot
    var activity: ActivitySummary?
    let now: Date
    /// Which way the card sits from the notch, which follows from the edge. The
    /// silhouette and tail are `NoticeStage`'s now; the contents do not read it.
    var direction: NotchEdge.TooltipDirection = .leading
    /// How many sessions this screen has room to list. Solved from the display
    /// rather than fixed, so a big screen hides nothing.
    var sessionCap: Int = NotchLayout.defaultSessionCap
    var resetTimeFormat: ResetTimeFormat = .automatic
    /// Likewise: where the tail points is the stage's.
    var tailOffset: CGFloat = 0
    /// A tap on a session row jumps to that session's terminal — nil leaves
    /// the rows as plain text.
    var onFocusSession: ((pid_t) -> Void)? = nil
    @AppStorage(Preferences.showUsagePaceKey) private var showUsagePace = false

    /// The same figure the hover region uses, so what is drawn and what is
    /// reachable can never drift apart.
    var height: CGFloat {
        NotchLayout.cardHeight(
            windowCount: snapshot.windows.count,
            groupCount: snapshot.windowGroupCount,
            moneyWindowCount: snapshot.windows.filter { $0.money != nil }.count,
            sessionCount: activity?.sessions.count ?? 0,
            sessionCap: sessionCap,
            statusMessage: snapshot.statusMessage,
            blockMessage: snapshot.block?.summary(now: now),
            hasTokenUsage: snapshot.tokenUsage != nil,
            hasPlan: snapshot.subtitle != nil,
            hasResetCredits: snapshot.availableResetCredits(at: now) != nil,
            compactRowCount: snapshot.compactRowCount,
            extraHeight: snapshot.sendExtraHeight
        )
    }

    var body: some View {
        TooltipShell(height: height) {
            // Stacked, not replaced in place: during a swap both sets of rows
            // exist for a moment, and in a ZStack they overlap and dissolve
            // instead of shoving each other around. Top-aligned so neither
            // drifts while the card resizes around them.
            ZStack(alignment: .topLeading) {
                VStack(alignment: .leading, spacing: 0) {
                    ProviderTooltip(activityNote: nil, snapshot: snapshot, now: now, resetTimeFormat: resetTimeFormat,
                                    showUsagePace: showUsagePace)
                    if let resetCredits = snapshot.availableResetCredits(at: now) {
                        UsageResetCreditsSection(credits: resetCredits, now: now)
                    }
                    if let activity {
                        SessionList(summary: activity, now: now, cap: sessionCap,
                                    onFocus: onFocusSession)
                    }
                }
                // An identity, so one provider's rows are never interpolated
                // into another's — that is what slid text through positions
                // belonging to neither layout. A crossfade rather than an
                // instant swap, so the change is part of the movement instead
                // of a cut in the middle of it.
                .id(snapshot.id)
                .transition(.opacity.animation(NotchMotion.crossfade))
            }
            .frame(maxWidth: .infinity, alignment: .topLeading)
        }
    }
}
