#if canImport(CryptoKit) && canImport(LocalAuthentication) && !os(watchOS)
import Foundation
import LocalAuthentication
import XCTest
import RightKitCredentials

final class OwnerKeyUnavailableTests: XCTestCase {
    func testUnavailableHardwareReportsTypedFailureWithoutAuthenticationUI() throws {
        try XCTSkipIf(OwnerKey.isAvailable, "Requires a host without usable Secure Enclave/biometry; enrolled-host signing is separate native evidence")
        let context = LAContext()
        context.interactionNotAllowed = true
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("rightkit-qualification-\(UUID().uuidString).key")
        defer { try? FileManager.default.removeItem(at: url) }
        let key = OwnerKey(fileURL: url)
        XCTAssertThrowsError(try key.publicKey(context: context)) { error in
            XCTAssertEqual(error as? OwnerKey.Failure, .unavailable)
        }
    }
}
#endif
