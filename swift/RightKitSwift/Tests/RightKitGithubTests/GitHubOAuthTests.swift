import XCTest
@testable import RightKitGithub
@MainActor
final class GitHubOAuthTests: XCTestCase {
    @MainActor
    func testClientIDRejectsEmptyAndUnexpandedBuildSetting() {
        XCTAssertThrowsError(try GitHubOAuth.validClientID("  "))
        XCTAssertThrowsError(try GitHubOAuth.validClientID("$(CODERIGHT_GITHUB_CLIENT_ID)"))
    }

    @MainActor
    func testClientIDTrimsValidPublicIdentifier() throws {
        XCTAssertEqual(try GitHubOAuth.validClientID("  Iv1.example  "), "Iv1.example")
    }

    func testCredentialRoundTripPreservesRefreshState() throws {
        let credential = GitHubOAuthCredential(
            accessToken: "access",
            refreshToken: "refresh",
            expiresAt: Date(timeIntervalSince1970: 100),
            refreshExpiresAt: Date(timeIntervalSince1970: 200)
        )

        XCTAssertEqual(
            try JSONDecoder().decode(
                GitHubOAuthCredential.self,
                from: JSONEncoder().encode(credential)
            ).refreshToken,
            "refresh"
        )
    }
}
