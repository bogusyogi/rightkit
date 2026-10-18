import Foundation
import XCTest
@testable import RightKitAppleCredentials

final class SyncedCredentialPolicyTests: XCTestCase {
    private let key = "test.sync.enabled"

    private func make() -> (SyncedCredentialReconciler, AppleCredentialStore, UserDefaults) {
        let defaults = UserDefaults(suiteName: "rk.synced.\(UUID().uuidString)")!
        let store = AppleCredentialStore(service: "test.\(UUID().uuidString)", backend: InMemoryAppleCredentialBackend())
        let reconciler = SyncedCredentialReconciler(
            store: store, defaults: defaults, enabledDefaultsKey: key,
            consent: (account: "sync.consent", marker: "app.sync.v1"))
        return (reconciler, store, defaults)
    }

    func testDecisionTable() {
        XCTAssertEqual(SyncedCredentialPolicy.decide(remote: nil, local: nil), .nothing)
        XCTAssertEqual(SyncedCredentialPolicy.decide(remote: "r", local: nil), .adoptRemote)
        XCTAssertEqual(SyncedCredentialPolicy.decide(remote: nil, local: "l"), .seedRemote("l"))
        XCTAssertEqual(SyncedCredentialPolicy.decide(remote: "x", local: "x"), .unchanged)
        XCTAssertEqual(SyncedCredentialPolicy.decide(remote: "r", local: "l"), .conflict)
    }

    func testDefaultsOffAndConsentNeverAutoEnables() {
        XCTAssertFalse(SyncedCredentialPolicy.isSyncEnabled(stored: nil))
        XCTAssertFalse(SyncedCredentialPolicy.adoptsAccountConsent(stored: nil, remoteMarkerMatches: true))
        XCTAssertFalse(SyncedCredentialPolicy.adoptsAccountConsent(stored: false, remoteMarkerMatches: true))
        XCTAssertTrue(SyncedCredentialPolicy.adoptsAccountConsent(stored: true, remoteMarkerMatches: false))
    }

    func testNonTextSyncedItemIsUnreadableAndNeverOverwritten() throws {
        let (rec, store, _) = make()
        let binary = Data([0xFF, 0xFE, 0x00, 0x80])
        try store.writeData(binary, account: "pro.bridge", scope: .synced).get()
        try store.write("token", account: "pro.bridge", scope: .device).get()
        XCTAssertEqual(rec.reconcile("pro.bridge"), .unreadable)
        XCTAssertEqual(try store.readData("pro.bridge", scope: .synced).get(), binary)
    }

    func testEnableSeedsRemoteAndPublishesConsent() throws {
        let (rec, store, _) = make()
        try store.write("token", account: "pro.bridge", scope: .device).get()
        let outcomes = rec.setSyncEnabled(true, accounts: ["pro.bridge"])
        XCTAssertEqual(outcomes["pro.bridge"], .seededRemote)
        XCTAssertTrue(rec.isSyncEnabled)
        XCTAssertEqual(try store.read("pro.bridge", scope: .synced).get(), "token")
        XCTAssertEqual(try store.read("sync.consent", scope: .synced).get(), "app.sync.v1")
    }

    func testConflictPreservesBothCopies() throws {
        let (rec, store, _) = make()
        try store.write("remote", account: "pro.bridge", scope: .synced).get()
        try store.write("local", account: "pro.bridge", scope: .device).get()
        XCTAssertEqual(rec.reconcile("pro.bridge"), .conflict)
        XCTAssertEqual(try store.read("pro.bridge", scope: .synced).get(), "remote")
        XCTAssertEqual(try store.read("pro.bridge", scope: .device).get(), "local")
    }

    func testDisableCopiesDownAndKeepsSyncedCopy() throws {
        let (rec, store, defaults) = make()
        try store.write("token", account: "pro.bridge", scope: .synced).get()
        rec.setSyncEnabled(false, accounts: ["pro.bridge"])
        XCTAssertFalse(rec.isSyncEnabled)
        XCTAssertEqual(defaults.object(forKey: key) as? Bool, false)
        XCTAssertEqual(try store.read("pro.bridge", scope: .device).get(), "token")
        XCTAssertEqual(try store.read("pro.bridge", scope: .synced).get(), "token")
    }

    func testReadAndWriteFollowThePolicy() throws {
        let (rec, store, _) = make()
        try rec.write("  device-value \n", account: "a").get()
        XCTAssertEqual(try store.read("a", scope: .device).get(), "device-value")
        XCTAssertNil(try store.read("a", scope: .synced).get())
        rec.setSyncEnabled(true, accounts: [])
        try rec.write("synced-value", account: "a").get()
        XCTAssertEqual(try store.read("a", scope: .synced).get(), "synced-value")
        XCTAssertEqual(rec.read("a"), "synced-value")
        try rec.write("   ", account: "a").get()
        XCTAssertNil(rec.read("a"))
        XCTAssertNil(try store.read("a", scope: .device).get())
    }
}
