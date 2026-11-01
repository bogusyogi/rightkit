#if os(macOS)
import SwiftUI

/// **The handle** (liquid-motion pass §1): nothing at rest; with the pointer at
/// the foot, a bulb comes down out of the bottom cap on a neck, carrying the
/// Settings glyph, while the notch gathers itself about its end rings. A click
/// opens Settings; a press that moves 3pt carries the notch (see
/// `NotchPanel.trackPressOrDrag`).
///
/// Drawn in its own frame, centred on the end ring with +y running along the
/// stack away from it; the root view turns it onto the edge with `turn`. It is
/// drawn under the notch, so the neck leaves the cap's black and the ring stays
/// on top. Nothing is drawn, and nothing runs, while it is in.
struct NotchBulb: View, Animatable {
    /// 0 in the notch, 1 out.
    var swell: CGFloat
    /// The bulb's own size: 1, 0.9 in a click's dip, 0.86 while carried.
    var squeeze: CGFloat
    /// The turn from the bulb's frame onto the edge, in radians.
    let turn: Double
    /// Carried: the glyph dims to half.
    let carried: Bool
    /// What the bulb carries: the Settings glyph at the stack's end, or the minimise button's
    /// chevrons off the first ring (#375: the same knob, mirrored at the start of the stack, on
    /// the frame turned the other way).
    var mark: Mark = .settings

    enum Mark: Equatable {
        case settings
        /// The notch's minimise button. `restores`: the notch is drawn minimised, so a click
        /// opens it out (chevrons pointing away from the button); else it gathers toward it.
        case minimize(restores: Bool)
    }

    var animatableData: AnimatablePair<CGFloat, CGFloat> {
        get { AnimatablePair(swell, squeeze) }
        set {
            swell = newValue.first
            squeeze = newValue.second
        }
    }

    /// Along the stack from the end ring's centre to the bulb's: just inside
    /// the cap at rest, 41pt out.
    static let restCentre: CGFloat = 8
    static let outCentre = NotchBulbMetrics.outCentre
    static let restRadius: CGFloat = 14
    static let outRadius = NotchBulbMetrics.outRadius
    /// What a press or a click on the bulb answers to: its out size and a
    /// little over, so the pointer does not have to be exact on a moving thing.
    static let hitRadius = NotchBulbMetrics.hitRadius
    /// The neck leaves the cap from this far along, wide and inside the black;
    /// its width there, pinched toward the bulb, and the bulb's end of it as a
    /// multiple of the bulb's radius (the mock's `strand(base 44, pinch
    /// 44→30, top 1.7·r)`).
    static let neckFrom: CGFloat = 18
    static let neckBase: CGFloat = 44
    static let neckPinchOut: CGFloat = 30
    static let neckTop: CGFloat = 1.7
    /// The glyph's size in the bulb.
    static let glyphSize: CGFloat = 12
    /// The minimise chevrons: each one's tip is `chevronHeight` ahead of its arms' ends, which
    /// are `chevronHalf` either side; the two sit `chevronPitch` apart along the stack, 1.5pt
    /// strokes (the Windows notch's `draw_mini_glyph`).
    static let chevronHeight: CGFloat = 2.6
    static let chevronHalf: CGFloat = 3.3
    static let chevronPitch: CGFloat = 3.6
    static let chevronStroke: CGFloat = 1.5
    /// The frame: room for the bulb all the way out, either side of the ring.
    static let side: CGFloat = 2 * (outCentre + outRadius + 12)

    static func centre(_ swell: CGFloat) -> CGFloat {
        restCentre + (outCentre - restCentre) * swell
    }

    static func radius(_ swell: CGFloat) -> CGFloat {
        restRadius + (outRadius - restRadius) * min(max(swell, 0), 1)
    }

    /// The glyph comes in over the second half of the swell.
    private var glyphOpacity: Double {
        let shown = Double(GooArc.step((swell - 0.45) / 0.5))
        return carried ? shown * 0.5 : shown
    }

    var body: some View {
        let s = max(swell, 0)
        let along = Self.centre(s)
        let r = Self.radius(s) * squeeze
        ZStack {
            if s > 0.005 {
                Canvas { context, size in
                    let c = CGPoint(x: size.width / 2, y: size.height / 2)
                    // Goo: more while it moves, less once it is out.
                    let moving = s > 0.02 && s < 0.98
                    context.addFilter(.alphaThreshold(min: 0.5, color: Palette.notch))
                    context.addFilter(.blur(radius: moving ? 4.5 : 2.6))
                    context.drawLayer { layer in
                        let at = CGPoint(x: c.x, y: c.y + along)
                        layer.fill(Path(ellipseIn: CGRect(x: at.x - r, y: at.y - r,
                                                          width: 2 * r, height: 2 * r)),
                                   with: .color(.black))
                        // The neck, from inside the cap below the ring to the
                        // bulb: wide at the cap, pinched toward the bulb, the
                        // bulb's end 1.7 radii across. It is only a neck once
                        // the bulb is past where it starts.
                        let from = CGPoint(x: c.x, y: c.y + Self.neckFrom)
                        guard at.y > from.y else { return }
                        let base = Self.neckBase / 2
                        let pinch = (Self.neckBase + (Self.neckPinchOut - Self.neckBase) * min(s, 1)) * squeeze / 2
                        let top = r * Self.neckTop / 2
                        let middle = (from.y + at.y) / 2
                        var neck = Path()
                        neck.move(to: CGPoint(x: from.x - base, y: from.y))
                        neck.addQuadCurve(to: CGPoint(x: at.x - top, y: at.y),
                                          control: CGPoint(x: c.x - pinch, y: middle))
                        neck.addLine(to: CGPoint(x: at.x + top, y: at.y))
                        neck.addQuadCurve(to: CGPoint(x: from.x + base, y: from.y),
                                          control: CGPoint(x: c.x + pinch, y: middle))
                        neck.closeSubpath()
                        layer.fill(neck, with: .color(.black))
                    }
                }
                switch mark {
                case .settings:
                    Image(systemName: "slider.horizontal.3")
                        .font(.system(size: Self.glyphSize, weight: .semibold))
                        .foregroundStyle(Color.white)
                        .opacity(glyphOpacity)
                        .scaleEffect(squeeze)
                        .rotationEffect(.radians(-turn))
                        .offset(y: along)
                case .minimize(let restores):
                    Path { path in
                        let c = CGPoint(x: Self.side / 2, y: Self.side / 2)
                        // In the bulb's frame +y runs away from the first ring, toward the
                        // start of the stack: a notch to restore opens out the other way.
                        let reach: CGFloat = restores ? -1 : 1
                        for step in [-0.5, 0.5] as [CGFloat] {
                            let tip = c.y + along + step * Self.chevronPitch * -1 + reach * Self.chevronHeight / 2
                            let back = tip - reach * Self.chevronHeight
                            path.move(to: CGPoint(x: c.x - Self.chevronHalf, y: back))
                            path.addLine(to: CGPoint(x: c.x, y: tip))
                            path.addLine(to: CGPoint(x: c.x + Self.chevronHalf, y: back))
                        }
                    }
                    .stroke(Color.white.opacity(glyphOpacity),
                            style: StrokeStyle(lineWidth: Self.chevronStroke, lineCap: .round,
                                               lineJoin: .round))
                    .frame(width: Self.side, height: Self.side)
                }
            }
        }
        .frame(width: Self.side, height: Self.side)
        .rotationEffect(.radians(turn))
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}

/// The update and permissions badges, which sit on the foot's lower edge at
/// rest, on the bezel's side of it, and ride out onto the bulb's edge with the
/// hover. Drawn over the notch, unlike the bulb.
struct NotchBulbMarks: View, Animatable {
    var swell: CGFloat
    var squeeze: CGFloat
    let turn: Double
    /// +1 when the frame's +x points at the bezel once turned onto the edge,
    /// -1 when it points away from it. The badges ride the bezel's side.
    let bezelSide: CGFloat
    let badge: Bool
    let permissionsBadge: Bool

    var animatableData: AnimatablePair<CGFloat, CGFloat> {
        get { AnimatablePair(swell, squeeze) }
        set {
            swell = newValue.first
            squeeze = newValue.second
        }
    }

    var body: some View {
        let s = min(max(swell, 0), 1)
        let r = NotchBulb.radius(s) * squeeze
        let cap = NotchLayout.cornerRadius
        // At rest on the cap's edge, 0.74 along and 0.62 across its radius; out,
        // 0.72 of the bulb's radius round it, both ways.
        let foot = CGPoint(x: cap * 0.74, y: cap * 0.62)
        let edge = CGPoint(x: NotchBulb.centre(s) + r * 0.72, y: r * 0.72)
        let along = foot.x + (edge.x - foot.x) * s
        let aside = (foot.y + (edge.y - foot.y) * s) * bezelSide
        ZStack {
            if badge || permissionsBadge {
                Circle()
                    .fill(permissionsBadge ? Color(hex: 0xFFB340) : Palette.critical)
                    .overlay(Circle().strokeBorder(Palette.notch, lineWidth: SettingsOrb.badgeRing))
                    .frame(width: permissionsBadge ? 6 : SettingsOrb.badgeSize,
                           height: permissionsBadge ? 6 : SettingsOrb.badgeSize)
                    .offset(x: aside, y: along)
            }
        }
        .frame(width: NotchBulb.side, height: NotchBulb.side)
        .rotationEffect(.radians(turn))
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}
#endif
