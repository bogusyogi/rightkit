import Foundation
import XCTest
@testable import RightKitMesh
@MainActor
final class NearbyDiscoveryServiceTests: XCTestCase {
    func testDiscoveryOnlyPublishesCandidatesNeverConnects() {
        let service = NearbyDiscoveryService()

        // Starting/stopping browse must never mutate `DaemonStore` or produce a connected
        // config — discovery is candidate-list only, confirmed pairing stays a separate,
        // user-driven step through `PairingView`.
        XCTAssertTrue(service.candidates.isEmpty)
        service.start()
        // No real Bonjour service is advertised in the test environment, so the candidate
        // list stays empty — the behavior under test is that starting discovery never
        // synthesizes or auto-selects a candidate.
        XCTAssertTrue(service.candidates.isEmpty)
        service.stop()
        XCTAssertTrue(service.candidates.isEmpty)
        XCTAssertFalse(service.isBrowsing)
    }
}
