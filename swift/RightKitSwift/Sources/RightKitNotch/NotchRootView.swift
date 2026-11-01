#if os(macOS)
import SwiftUI

struct NotchRootView: View {
    @ObservedObject var model: NotchViewModel
    /// Pulse always animates; the system Reduce Motion setting is ignored (Adrian, 2026-10-11).
    private let reduceMotion = false
    @Environment(\.codenotchReduceTransparency) private var reduceTransparency
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        // Measured rather than assumed: the panel's real size is whatever
        // AppKit settled on, and the notch has to sit flush against *that*
        // edge, not against the size we asked for.
        GeometryReader { proxy in
            let place = NotchPlacement(edge: model.edge, panelSize: proxy.size)

            ZStack(alignment: .topLeading) {
                Color.clear

                // Under the bar, so the bar's own outline is what shows where the
                // two overlap. No transition: it only ever appears and goes with
                // nothing of it to see — see `NotchViewModel.neck`.
                if let neck = model.neck {
                    GooNeck(neck: neck)
                        .fill(Palette.notch)
                        .transition(.identity)
                }
                // **The handle's bulb**, under the notch so the neck leaves its
                // black and the end ring stays on top — see `NotchBulb`.
                handleUnder(place)
                // **The minimise button's knob** (#375), the same bulb at the other end.
                minimizeUnder(place)
                // **The cards**: toasts and the hover card, out of the notch and
                // back into it — see `NoticeStage`. Under the notch, which no
                // card overlaps (its neck runs up into the face), so the swell
                // at the notch's side and the neck run into the black above it.
                NoticeStage(slot: noticeSlot(place), place: place)
                ForEach(model.wings) { wing in
                    NotchFaceView(model: model, place: place, wing: wing)
                        // The side carrying the percentage across the Mac's
                        // notch is as long as the number: "9%" to "10%" eases
                        // it longer rather than stepping.
                        .animation(motion(NotchMotion.reading), value: model.readingAcrossTextWidth)
                }

                // The handle's badges, and its caption after a dwell — over
                // the notch, which the bulb itself is under.
                handleOver(place)
            }
            .frame(width: proxy.size.width, height: proxy.size.height)
        }
        .tint(model.accentColor.color)
        .environment(\.codenotchAccentColor, model.accentColor.color)
        .environment(\.notchSurfaceStyle, model.surfaceStyle)
        .environment(\.tooltipSecondaryInk, TooltipGlassContrast.secondaryInk(
            surfaceStyle: model.surfaceStyle,
            colorScheme: colorScheme,
            reduceTransparency: reduceTransparency
        ))
        .environment(\.weeklyRingDashed, model.weeklyRingDashed)
        .environment(\.usageWatchLimit, model.watchLimit)
        .environment(\.usageCriticalLimit, model.criticalLimit)
        .environment(\.colorTransitionStyle, model.colorTransitionStyle)
        // A click's dip, and the caption after a dwell — see "The handle" below.
        .task(id: model.settingsSpins) { await dipBulb() }
        .task(id: captionWanted) { await dwellOnHandle() }
    }

    // MARK: The handle (liquid-motion pass §1)
    //
    // Nothing at rest. With the pointer at the foot the notch gathers itself
    // (`model.hug`, on the shape) and the bulb comes out of the end ring's cap
    // on a neck: `NotchBulb` under the notch, `NotchBulbMarks` over it.

    /// The bulb's dip in a click: 1, and 0.9 while it is down.
    @State private var bulbDip: CGFloat = 1
    /// The caption beside the bulb, shown after a dwell on the handle.
    @State private var showsHandleCaption = false

    /// 1 with the bulb out: the pointer on the handle zone, or the notch in the
    /// hand.
    private var bulbSwell: CGFloat {
        model.isExpanded && (model.isHoveringSettings || model.carry != nil) ? 1 : 0
    }

    /// 0.86 while carried, 0.9 in a click's dip, otherwise 1.
    private var bulbSqueeze: CGFloat {
        model.carry != nil ? 0.86 : bulbDip
    }

    /// Whether the pointer is resting on the handle for the caption: never
    /// while the notch is carried.
    private var captionWanted: Bool {
        model.isExpanded && model.showsSettingsHandle
            && model.isHoveringSettings && model.carry == nil
    }

    /// The click's dip: down in 0.09 s, and back with a bounce.
    private func dipBulb() async {
        guard model.settingsSpins > 0 else { return }
        withAnimation(.snappy(duration: 0.09)) { bulbDip = 0.9 }
        try? await Task.sleep(nanoseconds: 90_000_000)
        guard !Task.isCancelled else { return }
        withAnimation(.bouncy(duration: 0.34)) { bulbDip = 1 }
    }

    /// The caption comes after the pointer has been on the handle for half a
    /// second, and goes as soon as it is not. One sleep, no timer.
    private func dwellOnHandle() async {
        guard captionWanted else {
            if showsHandleCaption {
                withAnimation(.easeOut(duration: 0.2)) { showsHandleCaption = false }
            }
            return
        }
        try? await Task.sleep(nanoseconds: 500_000_000)
        guard !Task.isCancelled else { return }
        withAnimation(.easeOut(duration: 0.2)) { showsHandleCaption = true }
    }

    /// The end ring's centre on the stack axis, in the panel: the bulb's frame
    /// is centred here, and scaled about it with the notch.
    /// How far the face overhangs the bezel; owned by `NotchFaceView`.
    static let bezelBleed = NotchFaceView.bezelBleed

    private func handleCentre(_ place: NotchPlacement) -> CGPoint {
        place.point(along: model.ringAlong(index: model.handleRingIndex, in: model.handleWing),
                    across: model.notchDepth / 2 * model.sizeScale)
    }

    /// The turn from the bulb's frame (+y along the stack, away from the end
    /// ring) onto the screen: the angle of increasing `along` minus a quarter
    /// turn, worked out from the placement so it is right on all four edges.
    private func handleTurn(_ place: NotchPlacement, direction: CGFloat? = nil) -> Double {
        let from = place.point(along: 0, across: 0)
        let to = place.point(along: direction ?? model.handleDirection, across: 0)
        return Double(atan2(to.y - from.y, to.x - from.x)) - .pi / 2
    }

    /// +1 when the bulb frame's +x, turned by `turn`, points at the bezel; -1
    /// when it points away. The badges ride the bezel's side.
    private func handleBezelSide(turn: Double) -> CGFloat {
        let out = model.edge.outward
        let dot = CGFloat(cos(turn)) * out.x + CGFloat(sin(turn)) * out.y
        return dot >= 0 ? 1 : -1
    }

    /// The bulb, under the notch.
    @ViewBuilder
    private func handleUnder(_ place: NotchPlacement) -> some View {
        if model.showsSettingsHandle {
            let swell = bulbSwell
            NotchBulb(swell: swell, squeeze: bulbSqueeze, turn: handleTurn(place),
                      carried: model.carry != nil)
                .animation(motion(NotchMotion.bulb), value: swell)
                .animation(motion(NotchMotion.squeeze), value: model.carry != nil)
                // Before `position`, as the orb was: scaled about its own
                // centre, which `handleCentre` then places.
                .scaleEffect(model.sizeScale)
                .position(handleCentre(place))
        }
    }

    /// The badges, over the notch, and the caption.
    @ViewBuilder
    private func handleOver(_ place: NotchPlacement) -> some View {
        if model.showsSettingsHandle {
            let swell = bulbSwell
            let turn = handleTurn(place)
            NotchBulbMarks(swell: swell, squeeze: bulbSqueeze, turn: turn,
                           bezelSide: handleBezelSide(turn: turn),
                           badge: model.updatePending,
                           permissionsBadge: model.permissionsPending)
                .animation(motion(NotchMotion.bulb), value: swell)
                .animation(motion(NotchMotion.squeeze), value: model.carry != nil)
                .scaleEffect(model.sizeScale)
                .position(handleCentre(place))
                // Folded, the pill carries the permissions dot instead.
                .opacity(model.isExpanded ? 1 : 0)
            if showsHandleCaption {
                handleCaption(place)
            }
        }
    }

    // MARK: The minimise button (#375)
    //
    // The settings bulb mirrored at the start of the stack: the same goo knob out of the first
    // ring's cap, on the same spring, under the notch, carrying two chevrons. Nothing at rest;
    // out while the pointer is in its zone (`isHoveringMinimize`) or on it. Drawn only; the
    // controller owns the pointer (see `NotchWindowController`).

    /// The button is out while the pointer is at the notch's leading end (the way the bulb is
    /// at the trailing one), and while it is being previewed.
    private var minimizeOut: Bool {
        model.isExpanded && model.canMinimize && model.carry == nil
            && (model.isHoveringMinimize || model.isOnMinimizeButton)
    }

    /// The button, under the notch so its neck leaves the cap's black.
    @ViewBuilder
    private func minimizeUnder(_ place: NotchPlacement) -> some View {
        if model.canMinimize {
            let swell: CGFloat = minimizeOut ? 1 : 0
            NotchBulb(swell: swell, squeeze: 1, turn: handleTurn(place, direction: -1),
                      carried: false, mark: .minimize(restores: model.showsMinimized))
                .animation(motion(NotchMotion.bulb), value: swell)
                .scaleEffect(model.sizeScale)
                .position(place.point(along: model.ringAlong(index: 0, in: model.handleWing),
                                      across: model.notchDepth / 2 * model.sizeScale))
        }
    }

    /// "Hold and drag to move", beside the bulb on the side toward the
    /// inside of the screen, 10pt clear of it: its near edge is anchored at the
    /// point and it grows away from the bezel.
    private func handleCaption(_ place: NotchPlacement) -> some View {
        let scale = model.sizeScale
        let out = model.bulbOutPoint
        let anchor = place.point(along: model.handleWing.lead + out.x * scale,
                                 across: (out.y + NotchBulb.outRadius + 10) * scale)
        let alignment: Alignment
        switch model.edge {
        case .right:  alignment = .trailing
        case .left:   alignment = .leading
        case .top:    alignment = .top
        case .bottom: alignment = .bottom
        }
        return Text(L10n.t("Hold and drag to move"))
            .font(.system(size: 8.5 * scale, weight: .semibold))
            .foregroundStyle(Color.white)
            .padding(.horizontal, 8 * scale)
            .frame(height: 16 * scale)
            .background(Capsule().fill(Palette.notch))
            .fixedSize()
            .frame(width: 0, height: 0, alignment: alignment)
            .position(anchor)
            .transition(.opacity)
            .allowsHitTesting(false)
            .accessibilityHidden(true)
    }


    private func motion(_ animation: Animation) -> Animation? {
        NotchMotion.respectingReduceMotion(animation, reduceMotion)
    }

    /// The warning line a minimised Send ring's hover card carries in its strip, else nil.
    private func warningStripLine(for snapshot: ProviderSnapshot) -> String? {
        snapshot.providerID == NearbySharing.providerID ? model.warningLine : nil
    }

    private func tooltipLength(_ snapshot: ProviderSnapshot) -> CGFloat {
        model.edge.isVertical ? cardAcross(snapshot) : NotchLayout.cardWidth
    }

    /// The card's extent across the stack (its height on a horizontal edge).
    private func cardAcross(_ snapshot: ProviderSnapshot) -> CGFloat {
        NotchLayout.cardHeight(
            windowCount: snapshot.windows.count,
            groupCount: snapshot.windowGroupCount,
            moneyWindowCount: snapshot.windows.filter { $0.money != nil }.count,
            sessionCount: model.activity(for: snapshot.id)?.sessions.count ?? 0,
            sessionCap: model.sessionCap,
            statusMessage: snapshot.statusMessage,
            blockMessage: snapshot.block?.summary(now: model.now),
            hasTokenUsage: snapshot.tokenUsage != nil,
            hasPlan: snapshot.subtitle != nil,
            hasResetCredits: snapshot.hasAvailableResetCredits,
            compactRowCount: snapshot.compactRowCount,
            extraHeight: snapshot.sendExtraHeight)
    }

    // MARK: The cards (liquid-motion pass §2 and §4)
    //
    // What is up in the one card slot, in the order it always was: an update
    // offered or installing, then a disk image's (or nearby sharing's) card, then
    // a notice (while no ring is hovered), then a ring's hover card. The slot is
    // handed to `NoticeStage`, which brings it out of the notch and takes it back;
    // everything here is where it hangs and what it says.

    private func noticeSlot(_ place: NotchPlacement) -> NoticeSlot? {
        guard model.isExpanded else { return nil }
        let hanging = place.edge.isVertical
        let inset = model.notchDrawnDepth
        func length(_ size: CGSize) -> CGFloat { hanging ? size.height : size.width }

        if let prompt = model.updatePrompt {
            let notice = NoticePrompt(update: prompt)
            let size = NoticeCard.size(for: notice)
            return toast(key: "update", notice: notice, size: size, inset: inset,
                         tail: model.notchMiddleAlong,
                         centre: model.cardAlong(centredOn: model.notchMiddleAlong, length: length(size)),
                         onHover: { model.noticeHeld = $0 })
        }
        if let disk = model.diskImagePrompt {
            let notice = NoticePrompt(disk: disk)
            return toast(key: "disk", notice: notice, size: NoticeCard.size(for: notice), inset: inset,
                         tail: model.diskImageTailTarget(for: disk),
                         centre: model.diskImageCardAlong(for: disk),
                         onHover: {
                             model.noticeHeld = $0
                             model.onDiskImageHover?($0)
                         })
        }
        if let event = model.activeResetAlert, model.hoveredIndex == nil {
            let notice = NoticePrompt(event: event, life: model.resetAlertLife)
            return toast(key: "notice", notice: notice, size: NoticeCard.size(for: notice), inset: inset,
                         tail: model.resetAlertTailTarget(for: event),
                         centre: model.resetAlertCardAlong(for: event),
                         onHover: { model.noticeHeld = $0 })
        }
        if model.showsHoverCards, let snapshot = model.hoveredSnapshot, let index = model.hoveredIndex {
            let card = TooltipCard(
                snapshot: snapshot,
                activity: model.activity(for: snapshot),
                now: model.now,
                direction: model.edge.tooltipDirection,
                sessionCap: model.sessionCap,
                resetTimeFormat: model.resetTimeFormat,
                onFocusSession: model.onFocusSession)
            // One object that travels and resizes between rings, which reads far
            // better than one card leaving and another arriving. What must not
            // interpolate is its contents — see `TooltipCard`.
            //
            // The minimised Send ring with a gauge in warning: the normal Send card, unchanged
            // and in its regular place, with the warning line in a strip above it (top in screen space) (Adrian,
            // 2026-10-22). The strip is kept for the whole hover, not only on the ring's band,
            // so the card does not resize under a pointer travelling into its buttons.
            let warning = warningStripLine(for: snapshot)
            let strip = warning == nil ? 0 : NotchLayout.cardWarningStrip
            let sendAcross = cardAcross(snapshot)
            let content: AnyView
            if let warning {
                content = AnyView(VStack(spacing: 0) {
                    Text(warning)
                        .font(Typography.cardTitle)
                        .foregroundStyle(Palette.textPrimary)
                        .lineLimit(1)
                        .padding(.horizontal, NotchLayout.cardPadding)
                        .frame(width: NotchLayout.cardWidth, height: strip, alignment: .leading)
                        .background(alignment: .bottom) { Rectangle().fill(Palette.textPrimary.opacity(0.12)).frame(height: 1) }
                    card.frame(width: NotchLayout.cardWidth, height: sendAcross, alignment: .top)
                }
                .environment(\.colorScheme, .dark)
                .environment(\.tooltipSecondaryInk, NoticeMetrics.detailInk))
            } else {
                content = AnyView(card
                    .environment(\.colorScheme, .dark)
                    .environment(\.tooltipSecondaryInk, NoticeMetrics.detailInk))
            }
            return NoticeSlot(
                key: "hover", kind: .hover, inset: inset,
                size: CGSize(width: NotchLayout.cardWidth, height: sendAcross + strip),
                tail: model.ringAlong(index: index, in: model.cellWing),
                centre: model.tooltipAlong(index: index,
                                           length: tooltipLength(snapshot) + (hanging ? strip : 0)),
                content: content)
        }
        return nil
    }

    private func toast(key: String, notice: NoticePrompt, size: CGSize, inset: CGFloat,
                       tail: CGFloat, centre: CGFloat,
                       onHover: @escaping (Bool) -> Void) -> NoticeSlot {
        let card = NoticeCard(prompt: notice, held: model.noticeHeld || model.noticeToolHold) { choice in
            switch choice {
            case .update(let answer): model.onUpdateChoice?(answer)
            case .disk(let answer):   model.onDiskImageChoice?(answer)
            case .dismissNotice:      model.activeResetAlert = nil
            case .openHub(let section):
                model.onOpenHub?(section)
                model.activeResetAlert = nil
            }
            return
        }
        return NoticeSlot(key: key, kind: .toast, inset: inset, size: size, tail: tail, centre: centre,
                          content: AnyView(card
                            .onHover(perform: onHover)
                            .environment(\.tooltipSecondaryInk, NoticeMetrics.detailInk)))
    }
}
#endif
