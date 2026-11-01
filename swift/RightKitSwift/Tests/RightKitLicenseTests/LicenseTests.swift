import RightKitLicense
import XCTest
@testable import RightKitLicense
/// Exercises the tier/feature projection only: `LicenseClaims` is constructed directly (its
/// initializer is public) and set on the store's plain `claims` var, never through
/// `load()`/`setToken()`/`clear()` — those touch Keychain and are out of scope here.
@MainActor
final class LicenseTests: XCTestCase {
    func testProClaimsReportIsProTrue() {
        let store = LicenseStore()
        store.claims = LicenseClaims(
            version: 2, id: "lic-pro", app: "coderight", tier: "pro",
            features: ["multi_model_orchestration", "brews_conductor"], deviceId: "dev-1",
            issuedAt: 0, expiresAt: .max,
        )
        XCTAssertTrue(store.isPro)
    }

    func testFreeClaimsReportIsProFalse() {
        let store = LicenseStore()
        store.claims = LicenseClaims(
            version: 2, id: "lic-free", app: "coderight", tier: "free",
            features: [], deviceId: "dev-1", issuedAt: 0, expiresAt: .max,
        )
        XCTAssertFalse(store.isPro)
    }

    func testNilClaimsReportIsProFalse() {
        let store = LicenseStore()
        XCTAssertNil(store.claims)
        XCTAssertFalse(store.isPro)
    }

    func testHasFeatureProjectsFromClaims() {
        let claims = LicenseClaims(
            version: 2, id: "lic-pro", app: "coderight", tier: "pro",
            features: ["multi_model_orchestration"], deviceId: "dev-1", issuedAt: 0, expiresAt: .max,
        )
        XCTAssertTrue(claims.hasFeature("multi_model_orchestration"))
        XCTAssertFalse(claims.hasFeature("schedules"))
    }

    func testUnknownTierIsNotPro() {
        let claims = LicenseClaims(
            version: 2, id: "lic-trial", app: "coderight", tier: "trial",
            features: ["schedules"], deviceId: "dev-1", issuedAt: 0, expiresAt: .max,
        )
        XCTAssertFalse(claims.isPro)
    }
}
