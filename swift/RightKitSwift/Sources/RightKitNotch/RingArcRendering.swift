#if os(macOS)
import SwiftUI

/// How a ring's arcs are drawn. The default draws shapes SwiftUI (and
/// `ImageRenderer`) can render anywhere; the Mac registers a renderer that
/// hands the sweep, spin and pulse to Core Animation.
///
/// Signatures follow swiftright-split-plan.md 2c; `filled` is added to
/// `pulsingRing` for the warning disc.
protocol RingArcRendering {
    func arc(fraction: CGFloat, color: Color, radius: CGFloat, lineWidth: CGFloat,
             dashed: Bool, opacity: Double, spin: Double) -> AnyView
    func spinningArc(color: Color, arcFraction: CGFloat, dashed: Bool, inset: CGFloat,
                     turns: Bool) -> AnyView
    func pulsingRing(color: Color, inset: CGFloat, lineWidth: CGFloat, lowOpacity: Float,
                     pulses: Bool, halfPeriod: CFTimeInterval, filled: Bool) -> AnyView
}

extension RingArcRendering {
    func pulsingRing(color: Color, inset: CGFloat, lineWidth: CGFloat, lowOpacity: Float,
                     pulses: Bool, halfPeriod: CFTimeInterval = 0.9) -> AnyView {
        pulsingRing(color: color, inset: inset, lineWidth: lineWidth, lowOpacity: lowOpacity,
                    pulses: pulses, halfPeriod: halfPeriod, filled: false)
    }
}

/// Drawn shapes, static: what `ImageRenderer` draws today for the arc (the old `still` branch),
/// and the shared default.
struct ShapeRingRenderer: RingArcRendering {
    func arc(fraction: CGFloat, color: Color, radius: CGFloat, lineWidth: CGFloat,
             dashed: Bool, opacity: Double, spin: Double) -> AnyView {
        AnyView(
            Circle()
                .inset(by: NotchLayout.ringDiameter / 2 - radius)
                .trim(from: 0, to: fraction)
                .stroke(color,
                        style: StrokeStyle(lineWidth: lineWidth,
                                           lineCap: dashed ? .butt : .round,
                                           dash: dashed ? [4, 2] : []))
                .opacity(opacity)
                .rotationEffect(.degrees(-90 + spin))
        )
    }

    func spinningArc(color: Color, arcFraction: CGFloat, dashed: Bool, inset: CGFloat,
                     turns: Bool) -> AnyView {
        AnyView(
            Circle()
                .inset(by: inset)
                .trim(from: 0, to: arcFraction)
                .stroke(color,
                        style: StrokeStyle(lineWidth: NotchLayout.activityStroke,
                                           lineCap: .round,
                                           dash: dashed ? [0.01, NotchLayout.activityStroke * 2.2] : []))
        )
    }

    func pulsingRing(color: Color, inset: CGFloat, lineWidth: CGFloat, lowOpacity: Float,
                     pulses: Bool, halfPeriod: CFTimeInterval, filled: Bool) -> AnyView {
        // Shown at full strength, not caught mid-pulse.
        if filled {
            return AnyView(Circle().inset(by: inset).fill(color))
        }
        return AnyView(Circle().inset(by: inset).stroke(color, lineWidth: lineWidth))
    }
}

/// The dots around the Send ring while the clipboard burst is open. The Mac registers
/// `ClipBurstDots` in `RingBurstOverlayHost` at launch (`MacNotchAdapters.register()`); the face
/// never names it, and draws nothing until one is registered.
@MainActor
enum RingBurstOverlayHost {
    static var make: @MainActor () -> AnyView = { AnyView(EmptyView()) }
}

private struct RingBurstOverlayKey: EnvironmentKey {
    static var defaultValue: @MainActor () -> AnyView { { RingBurstOverlayHost.make() } }
}

extension EnvironmentValues {
    var ringBurstOverlay: @MainActor () -> AnyView {
        get { self[RingBurstOverlayKey.self] }
        set { self[RingBurstOverlayKey.self] = newValue }
    }
}

private struct RingArcRendererKey: EnvironmentKey {
    /// `RingArcRenderingRegistry` is the platform seam: the Mac target compiles
    /// `ProviderRing+AppKit.swift`, which registers the Core Animation renderer.
    static var defaultValue: RingArcRendering { RingArcRenderingRegistry.shared }
}

extension EnvironmentValues {
    var ringArcRenderer: RingArcRendering {
        get { self[RingArcRendererKey.self] }
        set { self[RingArcRendererKey.self] = newValue }
    }
}
#endif
