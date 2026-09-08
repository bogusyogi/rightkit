import SwiftUI

public enum RightKitFallbackMaterial: Sendable {
    case ultraThin
    case thin
    case regular
}

public enum RightKitGlassStyle: Sendable {
    case regular
    case clear
}

enum RightKitGlassRenderingMode: Equatable {
    case native
    case material
    case opaque
}

enum RightKitGlassSurfacePolicy {
    static func renderingMode(
        supportsNativeGlass: Bool,
        reduceTransparency: Bool,
        increasedContrast: Bool,
        forceLegacy: Bool,
        forceOpaque: Bool
    ) -> RightKitGlassRenderingMode {
        if forceOpaque || reduceTransparency || increasedContrast { return .opaque }
        if forceLegacy || !supportsNativeGlass { return .material }
        return .native
    }
}

public extension View {
    /// Native Liquid Glass with one shared accessibility & older-OS fallback.
    /// Product code supplies radius & colors; RightKit owns only platform branching.
    func rightKitAdaptiveGlass(
        minimumCornerRadius: CGFloat,
        glassStyle: RightKitGlassStyle = .regular,
        fallbackMaterial: RightKitFallbackMaterial = .regular,
        fallbackBorder: Color,
        opaqueBackground: Color,
        opaqueBorder: Color,
        glassTint: Color? = nil,
        forceLegacy: Bool = false,
        forceOpaque: Bool = false
    ) -> some View {
        modifier(
            RightKitAdaptiveGlassModifier(
                minimumCornerRadius: minimumCornerRadius,
                glassStyle: glassStyle,
                fallbackMaterial: fallbackMaterial,
                fallbackBorder: fallbackBorder,
                opaqueBackground: opaqueBackground,
                opaqueBorder: opaqueBorder,
                glassTint: glassTint,
                forceLegacy: forceLegacy,
                forceOpaque: forceOpaque
            )
        )
    }
}

private struct RightKitAdaptiveGlassModifier: ViewModifier {
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    @Environment(\.colorSchemeContrast) private var contrast

    let minimumCornerRadius: CGFloat
    let glassStyle: RightKitGlassStyle
    let fallbackMaterial: RightKitFallbackMaterial
    let fallbackBorder: Color
    let opaqueBackground: Color
    let opaqueBorder: Color
    let glassTint: Color?
    let forceLegacy: Bool
    let forceOpaque: Bool

    @ViewBuilder
    func body(content: Content) -> some View {
        if #available(iOS 26.0, macOS 26.0, *) {
            let mode = RightKitGlassSurfacePolicy.renderingMode(
                supportsNativeGlass: true,
                reduceTransparency: reduceTransparency,
                increasedContrast: contrast == .increased,
                forceLegacy: forceLegacy,
                forceOpaque: forceOpaque
            )
            let shape = ConcentricRectangle(
                corners: .concentric(minimum: .fixed(max(0, minimumCornerRadius)))
            )
            switch mode {
            case .native:
                content.glassEffect(nativeGlass.tint(glassTint), in: shape)
            case .material:
                content
                    .background(fallbackStyle, in: shape)
                    .overlay(shape.stroke(fallbackBorder, lineWidth: 1))
            case .opaque:
                content
                    .background(opaqueBackground, in: shape)
                    .overlay(shape.stroke(opaqueBorder, lineWidth: 1))
            }
        } else {
            let shape = RoundedRectangle(
                cornerRadius: max(0, minimumCornerRadius),
                style: .continuous
            )
            let mode = RightKitGlassSurfacePolicy.renderingMode(
                supportsNativeGlass: false,
                reduceTransparency: reduceTransparency,
                increasedContrast: contrast == .increased,
                forceLegacy: forceLegacy,
                forceOpaque: forceOpaque
            )
            switch mode {
            case .opaque:
                content
                    .background(opaqueBackground, in: shape)
                    .overlay(shape.stroke(opaqueBorder, lineWidth: 1))
            case .native, .material:
                content
                    .background(fallbackStyle, in: shape)
                    .overlay(shape.stroke(fallbackBorder, lineWidth: 1))
            }
        }
    }

    private var fallbackStyle: AnyShapeStyle {
        switch fallbackMaterial {
        case .ultraThin: AnyShapeStyle(.ultraThinMaterial)
        case .thin: AnyShapeStyle(.thinMaterial)
        case .regular: AnyShapeStyle(.regularMaterial)
        }
    }

    @available(iOS 26.0, macOS 26.0, *)
    private var nativeGlass: Glass {
        switch glassStyle {
        case .regular: .regular
        case .clear: .clear
        }
    }
}
