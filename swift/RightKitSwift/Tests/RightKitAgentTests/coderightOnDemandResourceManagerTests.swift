import Foundation
import XCTest
@testable import RightKitAgent
private final class FakeOnDemandRequest: OnDemandResourceRequesting, @unchecked Sendable {
    var loadingPriority: Double = 0.5
    let progress = Progress(totalUnitCount: 100)
    var isResident = false
    var ended = false

    func beginAccessingResources(completionHandler: @escaping @Sendable (Error?) -> Void) {
        isResident = true
        progress.completedUnitCount = 100
        completionHandler(nil)
    }

    func conditionallyBeginAccessingResources(completionHandler: @escaping @Sendable (Bool) -> Void) {
        completionHandler(isResident)
    }

    func endAccessingResources() {
        ended = true
    }
}

@MainActor
final class OnDemandResourceManagerTests: XCTestCase {
    func testShippedCatalogMatchesTaggedPayload() {
        let pack = try? XCTUnwrap(OnDemandResourceManager.shippedCatalog.first)
        XCTAssertEqual(pack?.tag, "coderight-portable-tools-v1")
        XCTAssertEqual(pack?.manifestFileName, "manifest.json")
        XCTAssertEqual(pack?.payloadFileNames, ["portable-tools.json"])
    }

    func testFetchPinAndReleaseLifecycle() async throws {
        let request = FakeOnDemandRequest()
        let manifest = OnDemandResourceManifest(
            schema: "app.coderight.odr-pack.v1",
            tag: "coderight-portable-tools-v1",
            capabilities: ["portable-tool-metadata"],
            files: ["portable-tools.json"]
        )
        let manager = OnDemandResourceManager(
            requestFactory: { _ in request },
            manifestResolver: { _ in manifest }
        )
        let tag = try XCTUnwrap(manager.catalog.first?.tag)

        try await manager.fetch(tag: tag)
        XCTAssertEqual(manager.state(for: tag), .fetched)
        XCTAssertEqual(manager.fetchedPacks, [tag])
        XCTAssertEqual(manager.fetchedCapabilityIDs, ["portable-tool-metadata"])

        manager.pin(tag: tag)
        XCTAssertEqual(manager.state(for: tag), .pinned)
        XCTAssertEqual(request.loadingPriority, NSBundleResourceRequestLoadingPriorityUrgent)

        manager.release(tag: tag)
        XCTAssertEqual(manager.state(for: tag), .notFetched)
        XCTAssertTrue(request.ended)
        XCTAssertTrue(manager.fetchedPacks.isEmpty)
        XCTAssertTrue(manager.fetchedCapabilityIDs.isEmpty)
    }

    func testEmptyCatalogDoesNotRequestOrAdvertisePacks() async {
        let request = FakeOnDemandRequest()
        let manager = OnDemandResourceManager(catalog: [], requestFactory: { _ in request })

        await manager.refreshFetchedPacks()

        XCTAssertTrue(manager.fetchedResourcePackTags.isEmpty)
        XCTAssertTrue(manager.states.isEmpty)
        XCTAssertFalse(request.ended)
    }

    func testResidentTagWithoutMatchingManifestIsNotProjected() async {
        let request = FakeOnDemandRequest()
        request.isResident = true
        let manager = OnDemandResourceManager(
            requestFactory: { _ in request },
            manifestResolver: { _ in
                OnDemandResourceManifest(
                    schema: "app.coderight.odr-pack.v1",
                    tag: "wrong-tag",
                    capabilities: ["portable-tool-metadata"],
                    files: ["portable-tools.json"]
                )
            }
        )

        await manager.refreshFetchedPacks()

        XCTAssertTrue(manager.fetchedResourcePackTags.isEmpty)
        XCTAssertTrue(manager.fetchedCapabilityIDs.isEmpty)
        XCTAssertTrue(request.ended)
    }
}
