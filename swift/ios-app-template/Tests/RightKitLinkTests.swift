import RightKitArtifacts
import RightKitDiagnostics
import RightKitLicense
import XCTest

// Proves the app target actually links every RightKitSwift product — a link/import failure is the
// thing that breaks when a package does not resolve. Product behaviour is out of scope here.
final class RightKitLinkTests: XCTestCase {
    func testLicenseClaimsAreConstructible() {
        let claims = LicenseClaims(
            version: 2, id: "lic", app: "template", tier: "pro",
            features: ["demo"], deviceId: "dev-1", issuedAt: 0, expiresAt: .max,
        )
        XCTAssertTrue(claims.isPro)
        XCTAssertTrue(claims.hasFeature("demo"))
    }

    func testDiagnosticsAndArtifactsLink() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("link-test-\(UUID().uuidString).jsonl")
        let store = JSONLDiagnosticStore(fileURL: url)
        XCTAssertTrue(store.record(DiagnosticEvent(name: "test", fields: [:])))
        XCTAssertFalse(store.validLines().isEmpty)
        XCTAssertFalse(String(describing: ArtifactInstaller.self).isEmpty)
        try? FileManager.default.removeItem(at: url)
    }
}
