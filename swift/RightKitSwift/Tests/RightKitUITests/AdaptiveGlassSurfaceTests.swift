import XCTest
@testable import RightKitUI

final class AdaptiveGlassSurfaceTests: XCTestCase {
    func testNativeGlassWhenSupportedAndAccessibilityAllowsTransparency() {
        XCTAssertEqual(
            mode(supportsNativeGlass: true),
            .native
        )
    }

    func testMaterialFallbackOnOlderOSOrForcedLegacyCapture() {
        XCTAssertEqual(mode(supportsNativeGlass: false), .material)
        XCTAssertEqual(mode(supportsNativeGlass: true, forceLegacy: true), .material)
    }

    func testOpaqueFallbackWinsForAccessibilityAndDebugOverride() {
        XCTAssertEqual(mode(supportsNativeGlass: true, reduceTransparency: true), .opaque)
        XCTAssertEqual(mode(supportsNativeGlass: true, increasedContrast: true), .opaque)
        XCTAssertEqual(
            mode(supportsNativeGlass: false, forceLegacy: true, forceOpaque: true),
            .opaque
        )
    }

    private func mode(
        supportsNativeGlass: Bool,
        reduceTransparency: Bool = false,
        increasedContrast: Bool = false,
        forceLegacy: Bool = false,
        forceOpaque: Bool = false
    ) -> RightKitGlassRenderingMode {
        RightKitGlassSurfacePolicy.renderingMode(
            supportsNativeGlass: supportsNativeGlass,
            reduceTransparency: reduceTransparency,
            increasedContrast: increasedContrast,
            forceLegacy: forceLegacy,
            forceOpaque: forceOpaque
        )
    }
}
