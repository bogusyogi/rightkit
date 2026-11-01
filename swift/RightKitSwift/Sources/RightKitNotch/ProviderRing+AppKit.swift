#if os(macOS)
import AppKit
import SwiftUI

/// The Mac ring renderer: the three `CAShapeLayer` views, moved verbatim from
/// `ProviderRing.swift`. Core Animation carries out the sweep, colour change,
/// spin and pulse, so SwiftUI never re-runs the notch layout per frame.
struct AppKitRingArcRendering: RingArcRendering {
    func arc(fraction: CGFloat, color: Color, radius: CGFloat, lineWidth: CGFloat,
             dashed: Bool, opacity: Double, spin: Double) -> AnyView {
        AnyView(GaugeArcLayer(fraction: fraction, color: color, radius: radius,
                              lineWidth: lineWidth, dashed: dashed, opacity: opacity, spin: spin)
            .frame(width: NotchLayout.ringDiameter, height: NotchLayout.ringDiameter))
    }

    func spinningArc(color: Color, arcFraction: CGFloat, dashed: Bool, inset: CGFloat,
                     turns: Bool) -> AnyView {
        AnyView(SpinningArc(color: color, arcFraction: arcFraction, dashed: dashed,
                            inset: inset, turns: turns))
    }

    func pulsingRing(color: Color, inset: CGFloat, lineWidth: CGFloat, lowOpacity: Float,
                     pulses: Bool, halfPeriod: CFTimeInterval, filled: Bool) -> AnyView {
        AnyView(PulsingRing(color: color, inset: inset, lineWidth: lineWidth,
                            lowOpacity: lowOpacity, pulses: pulses,
                            halfPeriod: halfPeriod, filled: filled))
    }
}

/// The Mac renderer, registered by default. Owner: move this line into
/// `MacNotchAdapters.swift` (and inject it at `NotchHostingView`) when step 2 lands.
enum RingArcRenderingRegistry {
    static var shared: RingArcRendering = AppKitRingArcRendering()
}

struct GaugeArcLayer: NSViewRepresentable {
    let fraction: CGFloat
    let color: Color
    let radius: CGFloat
    let lineWidth: CGFloat
    let dashed: Bool
    let opacity: Double
    let spin: Double

    func makeNSView(context: Context) -> GaugeArcView { GaugeArcView() }

    func updateNSView(_ view: GaugeArcView, context: Context) {
        view.configure(fraction: fraction, color: NSColor(color), radius: radius,
                       lineWidth: lineWidth, dashed: dashed, opacity: opacity, spin: spin)
    }
}

final class GaugeArcView: NSView {
    static let sweepKey = "sweep"
    static let colorKey = "colour"
    static let spinKey = "spin"
    /// Below this change in the fraction (half a percent of the ring) a new
    /// reading is set without a sweep: the eye cannot see it, and an animation
    /// is a frame schedule.
    static let visibleStep: CGFloat = 0.005
    /// The same for colour, as the largest change in any sRGB component.
    static let visibleColorStep: CGFloat = 0.02

    let arc = CAShapeLayer()
    private var configured = false
    private var fraction: CGFloat = 0
    private var source: NSColor = .clear
    private var shown: CGColor = NSColor.clear.cgColor
    private var radius: CGFloat = 0
    private var spin: Double = 0

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        arc.fillColor = nil
        arc.strokeStart = 0
        arc.strokeEnd = 0
        arc.isHidden = true
        layer?.addSublayer(arc)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// Decoration only: clicks belong to the ring and the notch beneath it.
    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    func configure(fraction requested: CGFloat, color: NSColor, radius: CGFloat, lineWidth: CGFloat,
                   dashed: Bool, opacity: Double, spin newSpin: Double) {
        let target = min(max(requested, 0), 1)
        let resolved = Self.resolve(color, in: self)
        let animates = configured && window != nil

        CATransaction.begin()
        CATransaction.setDisableActions(true)

        arc.lineWidth = lineWidth
        arc.lineCap = dashed ? .butt : .round
        arc.lineDashPattern = dashed ? [4, 2] : nil
        arc.opacity = Float(opacity)
        self.radius = radius
        rebuildPath()

        // Sweep.
        if !configured || target != fraction {
            let moving = arc.animation(forKey: Self.sweepKey) != nil
            let step = abs(target - fraction)
            if animates, moving || step >= Self.visibleStep {
                // From where the layer is drawn now, so a reading that lands
                // mid-sweep carries on from there rather than from the last target.
                let from = arc.presentation()?.strokeEnd ?? arc.strokeEnd
                arc.strokeEnd = target
                arc.add(Self.spring("strokeEnd", from: from, to: target), forKey: Self.sweepKey)
            } else {
                arc.removeAnimation(forKey: Self.sweepKey)
                arc.strokeEnd = target
            }
            fraction = target
        }

        // Colour.
        if !configured || !CFEqual(resolved, shown) {
            let moving = arc.animation(forKey: Self.colorKey) != nil
            let step = Self.distance(resolved, shown)
            if animates, moving || step >= Self.visibleColorStep {
                let from = arc.presentation()?.strokeColor ?? arc.strokeColor ?? resolved
                arc.strokeColor = resolved
                arc.add(Self.spring("strokeColor", from: from, to: resolved), forKey: Self.colorKey)
            } else {
                arc.removeAnimation(forKey: Self.colorKey)
                arc.strokeColor = resolved
            }
            shown = resolved
        }
        source = color

        // Spin: one eased turn per refresh, ending where the arc belongs.
        if newSpin != spin {
            let to = -newSpin * .pi / 180
            arc.setValue(to, forKeyPath: "transform.rotation.z")
            if animates {
                let turn = CABasicAnimation(keyPath: "transform.rotation.z")
                turn.fromValue = -spin * .pi / 180
                turn.toValue = to
                turn.duration = 0.95
                turn.timingFunction = CAMediaTimingFunction(controlPoints: 0.32, 0, 0.14, 1)
                arc.add(turn, forKey: Self.spinKey)
            }
            spin = newSpin
        }

        // An empty arc draws nothing, whatever the caps: a round cap on a
        // zero-length stroke can show as a dot.
        if target > 0 {
            arc.isHidden = false
        } else if arc.animation(forKey: Self.sweepKey) != nil {
            CATransaction.setCompletionBlock { [weak self] in
                guard let self, self.fraction <= 0 else { return }
                self.arc.isHidden = true
            }
        } else {
            arc.isHidden = true
        }
        configured = true
        CATransaction.commit()
    }

    override func layout() {
        super.layout()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        rebuildPath()
        CATransaction.commit()
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        let resolved = Self.resolve(source, in: self)
        guard configured, !CFEqual(resolved, shown) else { return }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        arc.removeAnimation(forKey: Self.colorKey)
        arc.strokeColor = resolved
        shown = resolved
        CATransaction.commit()
    }

    /// Starts at 12 o'clock and runs clockwise. The layer's own coordinates run
    /// y-up, so that is angle π/2 and then decreasing angles (see `SpinningArcView`).
    /// The path is the full circle; `strokeEnd` is the trim.
    private func rebuildPath() {
        arc.bounds = CGRect(origin: .zero, size: bounds.size)
        arc.position = CGPoint(x: bounds.midX, y: bounds.midY)
        let path = CGMutablePath()
        path.addArc(center: CGPoint(x: bounds.midX, y: bounds.midY), radius: max(0, radius),
                    startAngle: .pi / 2, endAngle: .pi / 2 - 2 * .pi, clockwise: true)
        arc.path = path
    }

    /// `NotchMotion.reading` — a spring of response 0.9 and damping fraction 0.9,
    /// unit mass — as the layer spring with the same stiffness and damping.
    private static func spring(_ keyPath: String, from: Any, to: Any) -> CASpringAnimation {
        let spring = CASpringAnimation(keyPath: keyPath)
        spring.mass = 1
        spring.stiffness = pow(2 * Double.pi / NotchMotion.readingResponse, 2)
        spring.damping = 4 * Double.pi * NotchMotion.readingDamping / NotchMotion.readingResponse
        spring.initialVelocity = 0
        spring.fromValue = from
        spring.toValue = to
        spring.duration = spring.settlingDuration
        return spring
    }

    private static func resolve(_ color: NSColor, in view: NSView) -> CGColor {
        var resolved = color.cgColor
        view.effectiveAppearance.performAsCurrentDrawingAppearance { resolved = color.cgColor }
        return resolved
    }

    private static func distance(_ a: CGColor, _ b: CGColor) -> CGFloat {
        guard let space = CGColorSpace(name: CGColorSpace.sRGB),
              let x = a.converted(to: space, intent: .defaultIntent, options: nil)?.components,
              let y = b.converted(to: space, intent: .defaultIntent, options: nil)?.components,
              x.count == y.count
        else { return 1 }
        return zip(x, y).map { abs($0 - $1) }.max() ?? 0
    }
}


/// The working arc as a shape layer: a quarter of the activity circle from
/// 3 o'clock clockwise, or the whole circle as dots while requests are queued,
/// turning clockwise once every 1.1 seconds.
struct SpinningArc: NSViewRepresentable {
    let color: Color
    let arcFraction: CGFloat
    let dashed: Bool
    let inset: CGFloat
    let turns: Bool

    func makeNSView(context: Context) -> SpinningArcView { SpinningArcView() }

    func updateNSView(_ view: SpinningArcView, context: Context) {
        view.configure(color: NSColor(color), arcFraction: arcFraction,
                       dashed: dashed, inset: inset, turns: turns)
    }
}

final class SpinningArcView: NSView {
    static let turnDuration: CFTimeInterval = 1.1
    static let animationKey = "turn"

    let arc = CAShapeLayer()
    private var color: NSColor = .white
    private var inset: CGFloat = 0
    private var turns = true

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        arc.fillColor = nil
        arc.lineCap = .round
        arc.lineWidth = NotchLayout.activityStroke
        arc.strokeStart = 0
        layer?.addSublayer(arc)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// Decoration only: clicks belong to the ring and the notch beneath it.
    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    func configure(color: NSColor, arcFraction: CGFloat, dashed: Bool, inset: CGFloat, turns: Bool) {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        self.color = color
        self.inset = inset
        self.turns = turns
        arc.strokeEnd = arcFraction
        arc.lineDashPattern = dashed
            ? [0.01, NSNumber(value: Double(NotchLayout.activityStroke * 2.2))]
            : nil
        applyColor()
        rebuildPath()
        CATransaction.commit()
        updateAnimation()
    }

    override func layout() {
        super.layout()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        rebuildPath()
        CATransaction.commit()
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        updateAnimation()
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        applyColor()
    }

    private func applyColor() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            arc.strokeColor = color.cgColor
        }
    }

    /// The layer's own coordinates run y-up, so a visually clockwise circle
    /// starting at 3 o'clock is drawn with decreasing angles — the same start
    /// and direction as SwiftUI's `Circle().trim(from: 0, …)`.
    private func rebuildPath() {
        arc.frame = bounds
        let radius = max(0, min(bounds.width, bounds.height) / 2 - inset)
        let path = CGMutablePath()
        path.addArc(center: CGPoint(x: bounds.midX, y: bounds.midY), radius: radius,
                    startAngle: 0, endAngle: -2 * .pi, clockwise: true)
        arc.path = path
    }

    /// Re-added whenever it has gone missing: AppKit drops layer animations
    /// when a window leaves the screen, and the notch's panel does.
    private func updateAnimation() {
        guard turns, window != nil else {
            arc.removeAnimation(forKey: Self.animationKey)
            return
        }
        guard arc.animation(forKey: Self.animationKey) == nil else { return }
        let turn = CABasicAnimation(keyPath: "transform.rotation.z")
        turn.fromValue = 0
        turn.toValue = -2 * Double.pi
        turn.duration = Self.turnDuration
        turn.repeatCount = .infinity
        turn.isRemovedOnCompletion = false
        arc.add(turn, forKey: Self.animationKey)
    }
}

/// The pulsing ring as a shape layer: a full circle at the activity radius that
/// eases between full strength and `lowOpacity` and back, 0.9 seconds each way.
struct PulsingRing: NSViewRepresentable {
    let color: Color
    let inset: CGFloat
    let lineWidth: CGFloat
    let lowOpacity: Float
    let pulses: Bool
    /// Seconds from full to low (and from low back): 0.9 for the activity ring, slower for a warning.
    var halfPeriod: CFTimeInterval = 0.9
    /// A disc rather than a stroked ring.
    var filled = false

    func makeNSView(context: Context) -> PulsingRingView { PulsingRingView() }

    func updateNSView(_ view: PulsingRingView, context: Context) {
        view.configure(color: NSColor(color), inset: inset, lineWidth: lineWidth,
                       lowOpacity: lowOpacity, pulses: pulses, halfPeriod: halfPeriod,
                       filled: filled)
    }
}

final class PulsingRingView: NSView {
    static let animationKey = "pulse"

    let ring = CAShapeLayer()
    private var color: NSColor = .white
    private var inset: CGFloat = 0
    private var lowOpacity: Float = 0.3
    private var pulses = true
    private var halfPeriod: CFTimeInterval = 0.9
    private var filled = false

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        ring.fillColor = nil
        layer?.addSublayer(ring)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// Decoration only: clicks belong to the ring and the notch beneath it.
    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    func configure(color: NSColor, inset: CGFloat, lineWidth: CGFloat,
                   lowOpacity: Float, pulses: Bool, halfPeriod: CFTimeInterval = 0.9,
                   filled: Bool = false) {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        let restart = lowOpacity != self.lowOpacity || halfPeriod != self.halfPeriod
        self.halfPeriod = halfPeriod
        self.filled = filled
        self.color = color
        self.inset = inset
        self.pulses = pulses
        self.lowOpacity = lowOpacity
        ring.lineWidth = lineWidth
        applyColor()
        rebuildPath()
        if restart { ring.removeAnimation(forKey: Self.animationKey) }
        CATransaction.commit()
        updateAnimation()
    }

    override func layout() {
        super.layout()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        rebuildPath()
        CATransaction.commit()
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        updateAnimation()
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        applyColor()
    }

    private func applyColor() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            if filled {
                ring.fillColor = color.cgColor
                ring.strokeColor = nil
            } else {
                ring.strokeColor = color.cgColor
            }
        }
    }

    private func rebuildPath() {
        ring.frame = bounds
        let radius = max(0, min(bounds.width, bounds.height) / 2 - inset)
        let path = CGMutablePath()
        path.addEllipse(in: CGRect(x: bounds.midX - radius, y: bounds.midY - radius,
                                   width: radius * 2, height: radius * 2))
        ring.path = path
    }

    /// Re-added whenever it has gone missing: AppKit drops layer animations
    /// when a window leaves the screen, and the notch's panel does.
    private func updateAnimation() {
        guard pulses, window != nil else {
            ring.removeAnimation(forKey: Self.animationKey)
            return
        }
        guard ring.animation(forKey: Self.animationKey) == nil else { return }
        let fade = CABasicAnimation(keyPath: "opacity")
        fade.fromValue = 1
        fade.toValue = lowOpacity
        fade.duration = halfPeriod
        fade.autoreverses = true
        fade.repeatCount = .infinity
        fade.timingFunction = CAMediaTimingFunction(name: .easeInEaseOut)
        fade.isRemovedOnCompletion = false
        ring.add(fade, forKey: Self.animationKey)
    }
}
#endif
