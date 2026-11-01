#if os(macOS)
import SwiftUI

/// The motion vocabulary, in one place so the whole surface moves like one thing.
///
/// Springs rather than eased curves throughout: macOS motion reads as physical
/// because things overshoot slightly and settle, and a panel that scales without
/// any of that feels like a slideshow. The numbers are chosen to be *just* under
/// bouncy — `dampingFraction` in the high 0.7s gives a single soft settle rather
/// than a wobble.
enum NotchMotion {
    /// Folding open and shut.
    ///
    /// Quick and settled: it lands in about a third of a second with no
    /// overshoot, so hover reads as immediate rather than as slow motion
    /// (issue #399). The earlier 0.62 response with a 0.72 damping was too slow.
    ///
    /// It only reads as one movement because `SideNotchShape` is `Animatable`:
    /// the spring carries the shape's own numbers, not just the rect it is drawn
    /// into. See its `animatableData`.
    static let unfold = Animation.spring(response: 0.34, dampingFraction: 0.82)

    /// **Letting go of the display's notch** when the notch is picked up.
    ///
    /// Short and firm, because it is the one movement that can still be running
    /// when the drag starts arriving underneath it: the hand goes down and moves
    /// at once. Anything eased while the drag steps is eased on a different
    /// clock from the drag, and for however long that lasts the two can draw
    /// out of step. Settled in about a quarter of a second, with no overshoot to
    /// come back from.
    static let lift = Animation.spring(response: 0.26, dampingFraction: 0.92)

    /// Contents arriving after the shape has started opening. Kept a little
    /// quicker than `unfold` so the readings catch up with the black rather
    /// than dragging behind it.
    static let contents = Animation.spring(response: 0.30, dampingFraction: 0.85)

    /// The tooltip travelling between cells. Slower and more damped than the
    /// fold: it is a bigger object moving a longer way, and the same spring that
    /// feels crisp on a 10pt pill feels abrupt on a 226pt card.
    static let glide = Animation.spring(response: 0.30, dampingFraction: 0.90)

    /// The neck's anchor when a shown card moves to another ring (card-motion
    /// spec §2, option 4): quick and settled like `unfold`, started with no
    /// delay, so the neck follows the body without lagging behind it.
    static let neckTrail = Animation.spring(response: 0.34, dampingFraction: 0.85)

    /// Contents changing inside something that is already moving. Short, and an
    /// ease rather than a spring — a spring on a crossfade has nothing to
    /// overshoot and just arrives late.
    static let crossfade = Animation.easeInOut(duration: 0.16)

    /// A percentage changing under you. Slower on purpose: a ring that snaps to a
    /// new value reads as a glitch, one that sweeps reads as a measurement.
    static let reading = Animation.spring(response: readingResponse, dampingFraction: readingDamping)

    /// `reading`'s numbers, so the gauge rings' Core Animation springs
    /// (`GaugeArcView`) are the same spring rather than a copy that can drift.
    static let readingResponse: Double = 0.35
    static let readingDamping: Double = 0.9

    /// The notch gathering itself about its end rings, and letting go
    /// (liquid-motion pass §1). Bouncy on purpose: released, the ends puff back
    /// out past where they were before they settle.
    static let gather = Animation.spring(response: 0.45, dampingFraction: 0.62)

    /// The handle's bulb coming out of the foot on its neck, and going back in
    /// (liquid-motion pass §1).
    static let bulb = Animation.spring(response: 0.28, dampingFraction: 0.82)

    /// The bulb squeezing to 0.86 as the notch is taken hold of, and giving
    /// way again.
    static let squeeze = Animation.spring(response: 0.28, dampingFraction: 0.7)

    /// The hovered ring growing to 1.12 about its own centre (liquid-motion pass §2).
    static let emphasis = Animation.spring(response: 0.3, dampingFraction: 0.7)

    /// The rings that are not hovered dulling to 45%, and coming back.
    static let dull = Animation.easeInOut(duration: 0.14)

    /// A toast or a hover card coming out of the notch on its neck, and going
    /// back in (liquid-motion pass §4): the bulb's own spring, so every card
    /// moves the way the handle does (Adrian, 2026-10-12). Moving to another
    /// ring does not go in and out: it glides on `glide`.
    static let notice = bulb

    /// How long after the strip starts sliding out it starts opening (and, going
    /// in, after it starts closing that it starts sliding back): option A.
    static let noticeOpenDelay: Double = 0.05

    /// About how long a card takes to be back inside the notch, for the next
    /// queued toast to wait on: closing, then the slide a beat later.
    static let noticeIn: Double = 0.6

    /// A wheel segment swelling under the pointer. Livelier than `contents` so the
    /// swell answers the pointer, damped enough to settle in one soft overshoot.
    static let swell = Animation.spring(response: 0.3, dampingFraction: 0.72)

    /// The settings arc being taken back into the notch.
    ///
    /// Quicker than `unfold` and with no delay, which is the whole point: on
    /// the staggered spring the arc lagged the fold, so the notch began closing
    /// first and the arc appeared to leave with the screen edge instead of
    /// being absorbed. It has to be inside the black while there is still black
    /// to be inside. Eased *in* because a thing being drawn into a mass
    /// accelerates as it goes.
    static let merge = Animation.easeIn(duration: 0.2)

    /// Each cell trails the one above it, so the stack unfurls rather than
    /// appearing all at once. Capped so a long list never feels sluggish.
    static func stagger(index: Int) -> Animation {
        contents.delay(min(Double(index) * 0.02, 0.06))
    }

    /// Everything above, unless the system has been asked for less movement.
    static func respectingReduceMotion(_ animation: Animation, _ reduce: Bool) -> Animation? {
        reduce ? nil : animation
    }
}
#endif
