#if os(macOS)
import SwiftUI

/// The ring around a provider glyph: a grey track with a coloured arc that
/// starts at 12 o'clock and sweeps clockwise by the fraction used.
///
/// When that provider is doing something right now, a second, much thinner arc
/// appears *inside* the ring, in the gap between the glyph and the track. It is
/// deliberately a different radius, a different weight and a neutral colour, so
/// it reads as a separate fact rather than as the usage number moving.
struct ProviderRing: View {
    /// Nil when the provider reports what is left but never says out of what —
    /// there is no arc to draw, and inventing one would be a lie in a shape.
    let usedFraction: Double?
    let glyph: ProviderGlyph
    var isStale: Bool = false
    /// Blocked right now. Shown as spent whatever the arc says, because that is
    /// what it means for you — a ring reading 16% while the account is paused
    /// is technically true and practically a lie.
    var isBlocked: Bool = false
    var activity: ActivitySummary?
    /// A fetch this cell asked for, in flight.
    var isRefreshing: Bool = false
    /// The weekly limit, when the provider has one. Nil is the ordinary case
    /// for a provider with a single window, and draws nothing.
    var weeklyFraction: Double?
    /// Where the user asked for it, if at all.
    var weeklyRing: WeeklyRing = .off
    var bandOverride: UsageBand? = nil
    /// A gauge somewhere is past its warning level (#375): the clear band between the glyph and
    /// the track pulses deep red, slowly, for as long as it lasts. Only the Send ring is given it.
    var warning: Bool = false
    /// The Send ring: the copy burst's dots are drawn around it (#387).
    var burst: Bool = false
    /// One Claude ring per account (#454): the account's short tag ("A", "B"), and whether it is
    /// the signed-in one (live). Nil on every other ring.
    var accountBadge: String?
    var accountLive: Bool = false

    /// Pulse always animates; the system Reduce Motion setting is ignored (Adrian, 2026-10-11).
    private let reduceMotion = false
    @Environment(\.viewShotStill) private var still
    @Environment(\.ringArcRenderer) private var renderer
    @Environment(\.ringBurstOverlay) private var burstOverlay
    @Environment(\.codenotchReduceTransparency) private var reduceTransparency
    @Environment(\.usageWatchLimit) private var watchLimit
    @Environment(\.usageCriticalLimit) private var criticalLimit
    @Environment(\.colorTransitionStyle) private var colorTransitionStyle
    @Environment(\.codenotchAccentColor) private var accentColor
    @Environment(\.weeklyRingDashed) private var weeklyRingDashed
    @State private var spin: Double = 0

    private var band: UsageBand {
        guard !isBlocked else { return .exhausted }
        if let bandOverride { return bandOverride }
        return UsageBand.band(for: usedFraction ?? 0, watchLimit: watchLimit, criticalLimit: criticalLimit)
    }
    private var sweep: CGFloat { CGFloat(min(max(usedFraction ?? 0, 0), 1)) }

    /// The ring's actual stroke colour: a continuous ramp when that style is chosen, falling
    /// back to the discrete `band.color(accent:)` in hard-step mode and everywhere `band` itself
    /// special-cases — blocked (no fraction is meaningful once a limit is spent) and an explicit
    /// override from the caller (a deliberate discrete choice, not a reading to interpolate).
    private var primaryRingColor: Color {
        guard !isBlocked, bandOverride == nil, colorTransitionStyle == .ramp else {
            return band.color(accent: accentColor)
        }
        return UsageBand.rampColor(for: usedFraction ?? 0, watchLimit: watchLimit, accent: accentColor)
    }

    private var weeklyBand: UsageBand {
        isBlocked ? .exhausted : UsageBand.band(for: weeklyFraction ?? 0, watchLimit: watchLimit, criticalLimit: criticalLimit)
    }
    private var weeklySweep: CGFloat { CGFloat(min(max(weeklyFraction ?? 0, 0), 1)) }
    /// Same fallback rule as `primaryRingColor`, minus `bandOverride` — the weekly ring has none.
    private var weeklyRingColor: Color {
        guard !isBlocked, colorTransitionStyle == .ramp else { return weeklyBand.color(accent: accentColor) }
        return UsageBand.rampColor(for: weeklyFraction ?? 0, watchLimit: watchLimit, accent: accentColor)
    }

    var body: some View {
        ZStack {
            // Dimming applies to the usage reading only. Whether Claude is
            // working right now is known first-hand and stays at full strength
            // even when the percentage behind it has gone stale.
            ZStack {
                Circle()
                    .strokeBorder(Palette.ringTrack, lineWidth: NotchLayout.trackStroke)

                if usedFraction != nil {
                    // Refreshing spins the reading itself rather than
                    // overlaying a separate spinner: the thing being
                    // refetched is the thing that should move, and a second
                    // arc on the same track only competes with it.
                    //
                    // A ring that snaps to a new value reads as a glitch; one
                    // that sweeps reads as a measurement being taken. The
                    // sweep, its colour and the spin are all carried out by
                    // Core Animation inside `GaugeArc`, not by SwiftUI: a
                    // SwiftUI animation on a value that changes with every
                    // reading re-runs the whole notch's layout on each frame.
                    GaugeArc(fraction: sweep,
                             color: primaryRingColor,
                             radius: NotchLayout.ringDiameter / 2 - NotchLayout.trackStroke / 2,
                             lineWidth: NotchLayout.progressStroke,
                             spin: spin)
                }

                // The weekly limit, when there is one and it has been asked
                // for. Same start and direction as the headline arc, so the
                // two are read the same way round; thinner and at its own
                // radius, so which is which never has to be worked out.
                //
                // It carries its own band colour rather than borrowing the
                // headline's: a session at 12% beside a week at 91% is exactly
                // the case this exists for, and painting them the same colour
                // would hide it. Held slightly back in opacity so the headline
                // stays the one the eye lands on first.
                // Pulse fork: shown while an agent works too — there is no
                // spinner competing for the inner gap.
                if let radius = weeklyRing.radius, weeklyFraction != nil {
                    let inset = NotchLayout.ringDiameter / 2 - radius

                    // A track of its own, for the same reason the headline has
                    // one: a week nobody has spent yet draws an arc of zero
                    // length, and without something behind it that is
                    // indistinguishable from the feature being broken. Codex
                    // opened its week at 0% and read as missing.
                    Circle()
                        .inset(by: inset)
                        .stroke(Palette.ringTrack,
                                style: StrokeStyle(lineWidth: NotchLayout.weeklyRingStroke,
                                                   dash: weeklyRingDashed ? [4, 2] : []))
                        .opacity(reduceTransparency ? 1 : 0.7)

                    GaugeArc(fraction: weeklySweep,
                             color: weeklyRingColor,
                             radius: radius,
                             lineWidth: NotchLayout.weeklyRingStroke,
                             dashed: weeklyRingDashed,
                             opacity: reduceTransparency ? 1 : 0.8)
                }

                if warning {
                    // The disc inside the track, under the glyph: 30% to 90% of the deep red
                    // on a 3.2 s breath, as the Windows notch's band (`draw_warning_band`).
                    (still ? ShapeRingRenderer() : renderer).pulsingRing(
                        color: Palette.critical.opacity(0.9),
                        inset: NotchLayout.warningInset,
                        lineWidth: 0,
                        lowOpacity: reduceTransparency ? 0.6 : 0.3 / 0.9,
                        pulses: !reduceMotion && !still,
                        halfPeriod: 1.6,
                        filled: true)
                        .frame(width: NotchLayout.ringDiameter, height: NotchLayout.ringDiameter)
                        .allowsHitTesting(false)
                }

                ProviderGlyphView(glyph: glyph)
                    .foregroundStyle(Palette.textPrimary)
                    // A spent limit dims its glyph so the ring reads as "waiting".
                    // Under reduce-transparency, boost opacity so it stays legible without low alpha.
                    .opacity(band == .exhausted ? (reduceTransparency ? 0.7 : 0.35) : 1)
            }
            .opacity(isStale ? (reduceTransparency ? 0.75 : 0.45) : 1)

            if let activity, activity.state != .idle {
                ActivityArc(summary: activity)
            }

            if burst { burstOverlay() }

            if let accountBadge {
                // Inside the ring's own frame, bottom trailing, so it never reaches a neighbour.
                Text(accountLive ? accountBadge + "\u{2022}" : accountBadge)
                    .font(.system(size: 9, weight: .bold, design: .rounded))
                    .foregroundStyle(Palette.textPrimary)
                    .padding(.horizontal, 3)
                    .background(Capsule().fill(Palette.ringTrack))
                    .frame(width: NotchLayout.ringDiameter, height: NotchLayout.ringDiameter,
                           alignment: .bottomTrailing)
                    .allowsHitTesting(false)
            }
        }
        .frame(width: NotchLayout.ringDiameter, height: NotchLayout.ringDiameter)
        // Pressed in while it works, and released when the answer lands. The
        // ring is the button, so the ring is what should feel pressed.
        .scaleEffect(isRefreshing ? 0.93 : 1)
        .animation(.spring(response: 0.3, dampingFraction: 0.62), value: isRefreshing)
        .onChange(of: isRefreshing) { _, refreshing in
            guard refreshing, !reduceMotion else { return }
            // Exactly one turn, and it stops by itself.
            //
            // The obvious spelling is a `repeatForever` linear spin started on
            // the way in and cancelled on the way out — but `repeatForever` does
            // not stop when you set the value back, and if the value you set is
            // the one it is already animating toward, nothing changes and it
            // simply keeps going. The ring then spins for ever after a refresh
            // that finished half a second in.
            //
            // A single finite turn has no cancellation problem at all: 360° is
            // the same angle as 0°, so it lands exactly where the reading
            // belongs. It eases out, so it settles rather than stopping dead.
            //
            // The turn itself is `GaugeArcView`'s: a layer animation on the same
            // curve and length as the SwiftUI one it replaces.
            spin += 360
        }
    }
}

/// The inner indicator: a short arc that spins while work is happening, and a
/// full pulsing ring when something is blocked waiting on you.
private struct ActivityArc: View {
    @Environment(\.ringArcRenderer) private var renderer
    let summary: ActivitySummary

    /// Pulse always animates; the system Reduce Motion setting is ignored (Adrian, 2026-10-11).
    private let reduceMotion = false
    @Environment(\.codenotchReduceTransparency) private var reduceTransparency
    @Environment(\.viewShotStill) private var still
    /// How much of the circle the moving arc covers.
    private let arcFraction: CGFloat = 0.25

    private var inset: CGFloat {
        (NotchLayout.ringDiameter - NotchLayout.activityDiameter) / 2
    }

    var body: some View {
        Group {
            switch summary.state {
            // Pulse fork: no spinner while an agent works — constant motion
            // at the screen edge is noise. The hover card still lists sessions.
            case .working: EmptyView()
            case .waiting, .success: pulse
            case .idle:    EmptyView()
            }
        }
        .frame(width: NotchLayout.ringDiameter, height: NotchLayout.ringDiameter)
    }

    /// Dots are a line of things: with requests queued behind the running one
    /// the arc becomes a ring of them, still turning, so a backed-up model is
    /// told apart from a busy one at a glance.
    private var queued: Bool { summary.queued > 0 }

    /// Turned by Core Animation, not by SwiftUI.
    ///
    /// A `repeatForever` rotation re-runs the hosting view's layout on every
    /// frame, and the notch is one hosting view: while any session was working
    /// that alone kept the app near 4% of a core, which is most of the time
    /// for anyone who leaves Claude Code running. A layer animation is carried
    /// out by the render server and costs the app nothing between frames.
    private var spinner: some View {
        (still ? ShapeRingRenderer() : renderer).spinningArc(
            color: summary.color,
            arcFraction: queued ? 1 : arcFraction,
            dashed: queued,
            inset: inset,
            turns: !reduceMotion
        )
    }

    /// Pulsed by Core Animation, not by SwiftUI: the same `repeatForever` that
    /// would turn the ring re-runs the notch's layout on every frame, including
    /// while the notch is folded and the ring is at zero opacity.
    private var pulse: some View {
        (still ? ShapeRingRenderer() : renderer).pulsingRing(
            color: summary.color,
            inset: inset,
            lineWidth: NotchLayout.activityStroke,
            lowOpacity: reduceTransparency ? 0.65 : 0.3,
            // A still render shows the ring at full strength, not caught mid-pulse.
            pulses: !reduceMotion && !still)
    }
}

/// A gauge's coloured arc: starts at 12 o'clock and sweeps clockwise by
/// `fraction`, centred on the ring, `radius` out to the middle of its stroke.
///
/// Drawn as a shape layer that Core Animation sweeps, recolours and spins, the
/// way `PulsingRing` pulses. The readings change every few seconds, and a
/// SwiftUI spring on each one made the notch's hosting view commit a frame and
/// lay out its whole tree at the display's rate for most of every interval.
/// SwiftUI only hands this the target; nothing that sizes the notch reads an
/// animating value.
private struct GaugeArc: View {
    let fraction: CGFloat
    let color: Color
    let radius: CGFloat
    let lineWidth: CGFloat
    var dashed: Bool = false
    var opacity: Double = 1
    /// Degrees added by each refresh; the layer turns to it.
    var spin: Double = 0
    @Environment(\.viewShotStill) private var still
    @Environment(\.ringArcRenderer) private var renderer

    var body: some View {
        // `ImageRenderer` cannot draw AppKit-backed views: a still render takes the drawn arc.
        (still ? ShapeRingRenderer() : renderer)
            .arc(fraction: fraction, color: color, radius: radius, lineWidth: lineWidth,
                 dashed: dashed, opacity: opacity, spin: spin)
    }
}

struct ProviderCell: View {
    let snapshot: ProviderSnapshot
    var activity: ActivitySummary?
    var isRefreshing: Bool = false
    var weeklyRing: WeeklyRing = .off
    /// Whether the reading adds the weekly ring's percentage, as "30%/70%".
    var showsWeeklyReading: Bool = false
    /// Whether the percentage is drawn under the ring.
    ///
    /// Off where the cell sits in a menu-bar strip beside the hardware notch:
    /// the strip is the menu bar's height, which one ring already fills, and a
    /// second line would be drawn in the bezel. The reading is still a hover
    /// away in the card.
    var showsReading: Bool = true
    /// The cell's height, passed by the view from the model it observes; the default is the static for non-view callers.
    var cellExtent: CGFloat = NotchLayout.cellExtent
    /// Hover emphasis across the stack (liquid-motion pass §2): 1 for the
    /// hovered ring, which grows to 1.12 about its own centre; −1 for the
    /// others while one is hovered, which dull to 45% (track, arc, inner ring
    /// and glyph together); 0 when nothing is hovered.
    var emphasis: Int = 0
    /// A gauge is past its warning level: the Send ring's centre band pulses deep red (#375).
    var warning: Bool = false

    private var reading: ProviderReading {
        ProviderReading(snapshot: snapshot, weeklyRing: weeklyRing,
                        showsWeeklyReading: showsWeeklyReading)
    }

    private var readingText: String { reading.text }

    var body: some View {
        VStack(spacing: NotchLayout.ringLabelGap) {
            ProviderRing(
                usedFraction: snapshot.hasReading ? snapshot.ringFraction : nil,
                glyph: snapshot.glyph,
                isStale: snapshot.status.isStale || !snapshot.hasReading,
                isBlocked: snapshot.block != nil,
                activity: activity,
                isRefreshing: isRefreshing,
                weeklyFraction: snapshot.hasReading ? snapshot.weeklyFraction : nil,
                weeklyRing: weeklyRing,
                bandOverride: snapshot.bandOverride,
                warning: warning,
                burst: snapshot.providerID == ProviderIDs.send,
                accountBadge: snapshot.accountBadge,
                accountLive: snapshot.accountLive
            )
            // Only on hover, so nothing here runs between readings.
            .scaleEffect(emphasis > 0 ? Self.emphasisScale : 1)
            .animation(NotchMotion.emphasis, value: emphasis)
            .opacity(emphasis < 0 ? Self.dulledOpacity : 1)
            .animation(NotchMotion.dull, value: emphasis)
            if showsReading { reading }
        }
        .frame(height: cellExtent)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibilityText)
    }

    /// 44pt becomes 49pt: felt, and still clear of the bezel in a 60.5pt body.
    static let emphasisScale: CGFloat = 1.12
    static let dulledOpacity: Double = 0.45

    /// Everything the cell says, as one sentence for VoiceOver and the tests.
    var accessibilityText: String {
        if let name = snapshot.accountName, snapshot.accountBadge != nil {
            return "\(snapshot.displayName) \(name), \(readingText)"
        }
        return "\(snapshot.displayName), \(readingText)"
    }

}

private struct WeeklyRingDashedKey: EnvironmentKey {
    static let defaultValue: Bool = false
}

extension EnvironmentValues {
    var weeklyRingDashed: Bool {
        get { self[WeeklyRingDashedKey.self] }
        set { self[WeeklyRingDashedKey.self] = newValue }
    }
}

/// **A cell's percentage**, on its own: under its ring, or — merged into the
/// Mac's notch with a single ring — on the other side of the Mac's notch,
/// where the notch widens with nothing else to carry.
struct ProviderReading: View {
    let snapshot: ProviderSnapshot
    var weeklyRing: WeeklyRing = .off
    /// Whether the reading adds the weekly ring's percentage, as "30%/70%".
    var showsWeeklyReading: Bool = false
    /// Drawn on its own across the Mac's notch rather than under the ring: the
    /// larger size, and no more room along the bar than `width`.
    var across: CGFloat? = nil
    /// Which end of that room it sits at — the Mac's notch's.
    var acrossAlignment: Alignment = .leading

    /// A dash, not "0%": nothing read is not the same as nothing used.
    var text: String {
        guard snapshot.hasReading else { return "—" }
        guard let weekly = weeklyReading else { return snapshot.headlineText }
        return "\(snapshot.headlineText)/\(Percent.text(for: weekly))%"
    }

    /// What the weekly ring draws, when it and its reading are on. The pair
    /// mirrors the two rings, so with the weekly limit as the main ring or the
    /// daily pace ring the second number is the session, as the thin ring is.
    ///
    /// Only after a percentage: a count or a cost with a percentage after it
    /// would read as one quantity, and it is not.
    private var weeklyReading: Double? {
        guard showsWeeklyReading, weeklyRing != .off,
              snapshot.usedFraction != nil, snapshot.headline?.prefersUsedText != true
        else { return nil }
        return snapshot.weeklyFraction
    }

    /// Whether it reads as the pair, "30%/70%".
    var isPair: Bool { snapshot.hasReading && weeklyReading != nil }

    /// How wide it is drawn across the Mac's notch, in design points — what
    /// the side carrying it is sized to.
    var acrossWidth: CGFloat {
        let size = isPair ? Typography.percentPairAcrossSize : Typography.percentAcrossSize
        let text = self.text
        // Measured once per (text, size): asked for by the notch's layout on
        // nearly every pass, and the text almost never changes.
        let key = "\(size)|\(text)"
        if let known = Self.measured[key] { return known }
        let width = ceil(TextMeasuringRegistry.shared.width(
            of: text, size: size, weight: .semibold, monospacedDigits: true))
        if Self.measured.count > 64 { Self.measured.removeAll() }
        Self.measured[key] = width
        return width
    }

    private static var measured: [String: CGFloat] = [:]

    var body: some View {
        if let width = across {
            Text(text)
                .font(isPair ? Typography.percentPairAcross : Typography.percentAcross)
                .monospacedDigit()
                .foregroundStyle(Palette.textPrimary)
                .lineLimit(1)
                // Never into the side's own curved end: smaller before that.
                .minimumScaleFactor(0.4)
                .frame(width: width, alignment: acrossAlignment)
                .contentTransition(.numericText())
        } else {
            underTheRing
        }
    }

    private var underTheRing: some View {
        Text(text)
            .font(snapshot.hasReading && weeklyReading != nil ? Typography.percentPair : Typography.percent)
            .foregroundStyle(Palette.textPrimary)
            .lineLimit(1)
            .fixedSize(horizontal: true, vertical: false)
            .frame(height: NotchLayout.percentLineHeight)
            .contentTransition(.numericText())
    }
}
#endif
