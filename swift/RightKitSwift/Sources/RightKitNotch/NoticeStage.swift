import RightKitNotchCore
import SwiftUI

// **The way a card comes out of the notch** (liquid-motion pass §4, and §2's
// hover card, which is the same object on the same spring).
//
// The notch is a body of liquid: the card slides out of its face on a neck,
// on the handle's own spring (`NotchMotion.bulb`), solid black like the notch
// (Adrian, 2026-10-12: toasts slide out the way the bulb does, and are black).
// The neck stays, a strand from the notch's face to the card, wide where it
// leaves the notch, pinched in the middle, widening into the card, as the
// bulb's — one silhouette with the card. Going in is the same slide back.
//
// Everything below is worked out in **card space**: `s` is how far out from the
// notch's face (the notch is at s < 0, the card at s >= gap + neck) and `v` how
// far along the notch from where the neck points. One affine map turns that
// onto the panel, so the same numbers are right on all four edges.

/// The curves the emergence is cut from, the mock's.
enum NoticeCurve {
    static func clamp(_ x: CGFloat) -> CGFloat { min(max(x, 0), 1) }

    static func step(_ x: CGFloat) -> CGFloat {
        let t = clamp(x)
        return t * t * (3 - 2 * t)
    }

    static func lerp(_ a: CGFloat, _ b: CGFloat, _ t: CGFloat) -> CGFloat { a + (b - a) * t }
}

/// **What the stage is asked to hold up**: one card, where it hangs and what it
/// says. The root view works it out from the model on every update; the stage
/// decides when it comes out and goes in.
struct NoticeSlot {
    enum Kind {
        /// Something the notch says on its own.
        case toast
        /// A ring's hover card: the same motion as a toast.
        case hover
    }

    /// What is shown, for hand-overs: a toast and a hover card are one object
    /// with one silhouette, and a change of key crossfades the contents.
    var key: String
    var kind: Kind
    /// Where the notch's face is, in from the bezel.
    var inset: CGFloat
    /// The card's own size, the neck aside, as drawn: width and height.
    var size: CGSize
    /// Along the notch: where the neck points (the ring the card belongs to),
    /// and where the card's middle is (kept on the screen, so the neck may be
    /// off to one side).
    var tail: CGFloat
    var centre: CGFloat
    /// The contents. Not compared: they follow the model on their own.
    var content = AnyView(EmptyView())
    /// The card's corner radius: a toast's, unless the card is something larger (the clipboard strip).
    var corner: CGFloat = NoticeMetrics.corner

    /// What the choreography follows: which card, and where. Not the contents,
    /// which change under a card that is simply shown (a download's progress, a
    /// countdown) and must reach the screen without a glide or a re-emergence.
    /// The slot itself is deliberately not `Equatable`: SwiftUI would use that
    /// to skip the stage's body, and the drawn contents would go stale.
    struct Placement: Equatable {
        var key: String
        var kind: Kind
        var inset: CGFloat
        var size: CGSize
        var tail: CGFloat
        var centre: CGFloat
        var corner: CGFloat
    }

    var placement: Placement {
        Placement(key: key, kind: kind, inset: inset, size: size, tail: tail, centre: centre, corner: corner)
    }
}

/// The numbers that glide when a shown card moves or resizes.
struct NoticePose: Equatable {
    var tail: CGFloat
    var centre: CGFloat
    /// The card's extent away from the bezel, and along the notch.
    var across: CGFloat
    var length: CGFloat

    static let zero = NoticePose(tail: 0, centre: 0, across: 0, length: 0)

    init(tail: CGFloat, centre: CGFloat, across: CGFloat, length: CGFloat) {
        self.tail = tail
        self.centre = centre
        self.across = across
        self.length = length
    }

    init(_ slot: NoticeSlot, edge: NotchEdge) {
        tail = slot.tail
        centre = slot.centre
        across = edge.isVertical ? slot.size.width : slot.size.height
        length = edge.isVertical ? slot.size.height : slot.size.width
    }
}

/// **One card out of the notch**, at `slide` and `open`, each 0 to 1. Animatable,
/// so two springs (the bulb's own, `NotchMotion.notice`, the second a beat
/// later) play the whole thing, overshoot included, and nothing runs once it is
/// still.
///
/// Option A of docs/liquid-motion-pass/card-motion-options.html (Adrian,
/// 2026-10-12): a strip the height of the neck, the card's full width, slides
/// out from under the notch's face on its neck (`slide`); then it opens up and
/// down about the ring's line to the card's height (`open`). Going in is the
/// same played backwards: it closes to the strip, then the strip slides in.
/// Solid black, the notch's own colour. The contents ride with the card and come
/// in as it opens. While it opens the body is blurred and cut back to a hard
/// edge, so the neck runs into it as the bulb's does; it settles crisp. Drawn
/// under the notch, so whatever of it is still inside the notch is hidden.
struct NoticeEmergence: View, Animatable {
    var slide: CGFloat
    var open: CGFloat
    var tail: CGFloat
    var centre: CGFloat
    var across: CGFloat
    var length: CGFloat
    let place: NotchPlacement
    let inset: CGFloat
    let key: String
    let size: CGSize
    let content: AnyView
    var corner: CGFloat = NoticeMetrics.corner

    var animatableData: AnimatablePair<AnimatablePair<CGFloat, CGFloat>,
                                       AnimatablePair<CGFloat, AnimatablePair<CGFloat, AnimatablePair<CGFloat, CGFloat>>>> {
        get {
            AnimatablePair(AnimatablePair(slide, open),
                           AnimatablePair(tail, AnimatablePair(centre, AnimatablePair(across, length))))
        }
        set {
            slide = newValue.first.first
            open = newValue.first.second
            tail = newValue.second.first
            centre = newValue.second.second.first
            across = newValue.second.second.second.first
            length = newValue.second.second.second.second
        }
    }

    // MARK: - The shape at one instant

    /// What one instant draws, in card space.
    private struct Moment {
        /// The card and its neck as one outline.
        var body: Path?
        /// The card's near face, from the notch's face.
        var near: CGFloat = 0
        /// The goo's blur: 0 once it has arrived, so it settles crisp.
        var blur: CGFloat = 0
        var contents: Double = 0
    }

    /// Where the neck is anchored along the notch: at `tail`, unless the card
    /// has been kept on the screen away from it and `tail` is past the straight
    /// of its side — then as far toward it as the card's corner allows.
    private var anchor: CGFloat {
        let most = max(0, length / 2 - corner - Self.neckWidth / 2)
        return centre + min(max(tail - centre, -most), most)
    }

    private static let gap = NoticeMetrics.gap
    private static let tailLength = NoticeMetrics.tailLength
    private static let neckWidth = NoticeMetrics.neckWidth
    private static let neckPinch = NoticeMetrics.neckPinch
    /// How far into the notch the neck starts, so no seam shows at its face.
    private static let neckInside: CGFloat = 2
    /// The strip's height along the notch while it slides, before it opens:
    /// a little more than the neck, so the neck joins its straight side.
    static let strip: CGFloat = 34

    /// The card's face, from the notch's face.
    static var face: CGFloat { gap + tailLength }

    private func moment() -> Moment {
        typealias C = NoticeCurve
        var f = Moment()
        let s = max(slide, 0)
        guard s > 0.004 else { return f }
        let o = max(open, 0)
        let opened = min(o, 1)
        // In, the strip is wholly inside the notch: its far face 2pt under the
        // notch's face. Out, its near face is at `face`; on the overshoot it
        // goes a little past, and the neck stretches.
        let near = C.lerp(-across - Self.neckInside, Self.face, s)
        // Along the notch: the strip about the ring's line, opening to the card
        // about its own middle (kept on the screen, so it may be off the line).
        let wL = C.lerp(Self.strip, length, o)
        let vc = (centre - anchor) * opened
        // Rounded like the card once open; as a strip, rounded enough to read as
        // one, with a straight side left for the neck.
        let rx = min(corner, max((wL - Self.neckWidth) / 2 - 0.5, wL * 0.2))
        f.body = Self.outline(near: near, far: near + across, top: vc - wL / 2, bottom: vc + wL / 2,
                              rx: rx, neckBase: Self.neckWidth, neckPinch: Self.neckPinch,
                              neckEnd: Self.neckWidth, inside: Self.neckInside)
        f.near = near
        f.blur = 3.5 * (1 - C.step((opened - 0.6) / 0.35))
        f.contents = Double(C.step((opened - 0.55) / 0.4))
        return f
    }

    /// **The card and its neck as one outline**. The card spans `near...far`
    /// away from the notch and `top...bottom` along it; the neck leaves the near
    /// face, centred on v = 0 where it can (clamped to the card's straight),
    /// runs to the notch's face and `inside` it, `neckBase` wide there,
    /// `neckEnd` wide at the card, with concave sides (quadratic curves pulled
    /// in to `neckPinch`, as `NotchBulb`). No neck while the card is still at
    /// the notch's face.
    private static func outline(near: CGFloat, far: CGFloat, top: CGFloat, bottom: CGFloat,
                                rx: CGFloat, neckBase: CGFloat, neckPinch: CGFloat, neckEnd: CGFloat,
                                inside: CGFloat) -> Path {
        var path = Path()
        let midS = (near + far) / 2
        let end = min(neckEnd, max(0, bottom - top - 2 * rx - 1))
        let ta = max(-end / 2, top + rx)
        let tb = min(end / 2, bottom - rx)
        let mouth = -inside
        path.move(to: CGPoint(x: midS, y: top))
        path.addArc(tangent1End: CGPoint(x: near, y: top), tangent2End: CGPoint(x: near, y: bottom), radius: rx)
        if end > 0.5 && tb - ta > 0.5 && near - mouth > 1 {
            let middle = (near + mouth) / 2
            path.addLine(to: CGPoint(x: near, y: ta))
            path.addQuadCurve(to: CGPoint(x: mouth, y: -neckBase / 2),
                              control: CGPoint(x: middle, y: -neckPinch / 2))
            path.addLine(to: CGPoint(x: mouth, y: neckBase / 2))
            path.addQuadCurve(to: CGPoint(x: near, y: tb),
                              control: CGPoint(x: middle, y: neckPinch / 2))
        }
        path.addArc(tangent1End: CGPoint(x: near, y: bottom), tangent2End: CGPoint(x: far, y: bottom), radius: rx)
        path.addArc(tangent1End: CGPoint(x: far, y: bottom), tangent2End: CGPoint(x: far, y: top), radius: rx)
        path.addArc(tangent1End: CGPoint(x: far, y: top), tangent2End: CGPoint(x: near, y: top), radius: rx)
        path.closeSubpath()
        return path
    }

    /// Card space onto the panel, on whichever edge this is.
    private var transform: CGAffineTransform {
        let o = place.point(along: anchor, across: inset)
        let s = place.point(along: anchor, across: inset + 1)
        let v = place.point(along: anchor + 1, across: inset)
        return CGAffineTransform(a: s.x - o.x, b: s.y - o.y, c: v.x - o.x, d: v.y - o.y, tx: o.x, ty: o.y)
    }

    // MARK: - Body

    var body: some View {
        let f = moment()
        let map = transform
        let outline = (f.body ?? Path()).applying(map)
        ZStack(alignment: .topLeading) {
            if f.body != nil {
                Canvas { context, _ in
                    if f.blur >= 0.3 {
                        context.addFilter(.alphaThreshold(min: 0.5, color: Palette.notch))
                        context.addFilter(.blur(radius: f.blur))
                    }
                    context.drawLayer { layer in
                        layer.fill(outline, with: .color(Palette.notch))
                    }
                }
                .frame(width: place.panelSize.width, height: place.panelSize.height)
                .allowsHitTesting(false)
            }
            if f.contents > 0.001 {
                ZStack {
                    // One overlapping crossfade (card-motion-spec §2): on a change
                    // of key the old contents fade out as the new fade in, both on
                    // the same ease over the same 0.16 s, so they are complementary
                    // and the card is never empty. Both sit in this one frame and
                    // follow the card.
                    content
                        .id(key)
                        .transition(.asymmetric(insertion: .opacity.animation(NotchMotion.crossfade),
                                                removal: .opacity.animation(NotchMotion.crossfade)))
                }
                .frame(width: size.width, height: size.height)
                .opacity(f.contents)
                .position(contentCentre(near: f.near))
                .clipShape(NoticeOutlineShape(outline: outline))
                .allowsHitTesting(f.contents > 0.5)
            }
        }
        .frame(width: place.panelSize.width, height: place.panelSize.height, alignment: .topLeading)
    }

    /// Where the contents sit: against the card's near face as it is now, so
    /// they ride out with it, at the size they were laid out at, so a card that
    /// is gliding to another size is a mask moving over contents that stay put.
    private func contentCentre(near: CGFloat) -> CGPoint {
        let extent = place.edge.isVertical ? size.width : size.height
        return place.point(along: centre, across: inset + near + extent / 2)
    }
}

/// A path in the panel's own space, as a shape.
struct NoticeOutlineShape: Shape {
    let outline: Path

    func path(in rect: CGRect) -> Path { outline }
}

/// **The slot's stage**: holds one card up, and takes the card in and out of
/// the notch on the bulb's spring. A spring takes a change of mind from where
/// it is, so a card asked back while going in, or sent in while coming out,
/// simply turns round.
///
/// A card that comes while another is out does not go in and out: it glides to
/// its new place as one body on `NotchMotion.glide`, its neck trailing behind on
/// `NotchMotion.neckTrail` (card-motion-spec §2), and its contents crossfade.
struct NoticeStage: View {
    let slot: NoticeSlot?
    let place: NotchPlacement

    /// The last card that was up, kept for the way back in.
    private final class Memory {
        var last: NoticeSlot?
    }

    @State private var memory = Memory()
    /// Whether a card is up or on its way in or out.
    @State private var present = false
    @State private var slide: CGFloat = 0
    @State private var open: CGFloat = 0
    @State private var pose = NoticePose.zero
    /// Which run is current: a finished animation that is not the latest one
    /// is ignored.
    @State private var generation = 0
    @Environment(\.viewShotStill) private var still

    private var live: NoticeSlot? {
        if let slot {
            memory.last = slot
            return slot
        }
        return present ? memory.last : nil
    }

    var body: some View {
        ZStack(alignment: .topLeading) {
            // A picture is the card settled and fully out: no clock, no glide.
            if still, let slot {
                let settled = NoticePose(slot, edge: place.edge)
                NoticeEmergence(slide: 1, open: 1, tail: settled.tail, centre: settled.centre,
                                across: settled.across, length: settled.length,
                                place: place, inset: slot.inset, key: slot.key,
                                size: slot.size, content: slot.content, corner: slot.corner)
            } else if let live {
                NoticeEmergence(slide: slide, open: open, tail: pose.tail, centre: pose.centre,
                                across: pose.across, length: pose.length,
                                place: place, inset: live.inset, key: live.key,
                                size: live.size, content: live.content, corner: live.corner)
            }
        }
        .onAppear { sync(slot) }
        .onChange(of: slot?.placement) { _, _ in sync(slot) }
    }

    private func sync(_ new: NoticeSlot?) {
        guard let new else {
            if present { close() }
            return
        }
        memory.last = new
        let target = NoticePose(new, edge: place.edge)
        generation += 1
        let g = generation
        if present {
            // Out, or on its way in or out: glide to the new place and turn
            // round from wherever it is. The body glides at once; the neck's
            // anchor follows on its own softer, later spring, so the neck leans
            // behind the card and catches up (card-motion-spec §2, option 4).
            // Two transactions, each moving only its own numbers.
            if pose != target {
                withAnimation(NotchMotion.glide) {
                    pose.centre = target.centre
                    pose.across = target.across
                    pose.length = target.length
                }
                withAnimation(NotchMotion.neckTrail) { pose.tail = target.tail }
            }
            comeOut()
            return
        }
        present = true
        set(pose: target)
        // A frame later, so the card is already mounted at 0 when the spring
        // starts rather than appearing at its end.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.02) {
            MainActor.assumeIsolated {
                guard g == generation else { return }
                comeOut()
            }
        }
    }

    /// Straight to `target`, with nothing to glide from.
    private func set(pose target: NoticePose) {
        guard pose != target else { return }
        var transaction = Transaction(animation: nil)
        transaction.disablesAnimations = true
        withTransaction(transaction) { pose = target }
    }

    /// Out: the strip slides out, and opens a beat later.
    private func comeOut() {
        withAnimation(NotchMotion.notice) { slide = 1 }
        withAnimation(NotchMotion.notice.delay(NotchMotion.noticeOpenDelay)) { open = 1 }
    }

    /// In, the way out backwards: it closes to the strip, and the strip slides
    /// in a beat later.
    private func close() {
        generation += 1
        let g = generation
        withAnimation(NotchMotion.notice) { open = 0 }
        withAnimation(NotchMotion.notice.delay(NotchMotion.noticeOpenDelay),
                      completionCriteria: .logicallyComplete) {
            slide = 0
        } completion: {
            guard g == generation else { return }
            present = false
        }
    }
}

/// **A card as a picture**: settled and fully out, drawn with the same code the
/// stage uses, for the CI view renderer. `ImageRenderer` cannot draw glass, so
/// this is the dim and the rim over whatever is behind it.
struct NoticeSettled<Content: View>: View {
    let width: CGFloat
    let height: CGFloat
    var direction: NotchEdge.TooltipDirection = .leading
    /// Where the neck points, from the card's middle along the notch.
    var tailOffset: CGFloat = 0
    var corner: CGFloat = NoticeMetrics.corner
    @ViewBuilder let content: Content

    private var edge: NotchEdge {
        switch direction {
        case .leading:  return .right
        case .trailing: return .left
        case .down:     return .top
        case .up:       return .bottom
        }
    }

    var body: some View {
        let across = edge.isVertical ? width : height
        let length = edge.isVertical ? height : width
        let panel = NotchPlacement.panelSize(edge: edge, length: length,
                                             depth: NoticeMetrics.gap + NoticeMetrics.tailLength + across)
        NoticeEmergence(slide: 1, open: 1, tail: length / 2 + tailOffset, centre: length / 2,
                        across: across, length: length,
                        place: NotchPlacement(edge: edge, panelSize: panel), inset: 0,
                        key: "settled", size: CGSize(width: width, height: height),
                        content: AnyView(content), corner: corner)
            .frame(width: panel.width, height: panel.height)
    }
}
