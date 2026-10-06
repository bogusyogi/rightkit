import Foundation
import XCTest
@testable import RightKitAppleCredentials

final class AppleCredentialTests: XCTestCase {
    private func store() -> (AppleCredentialStore, InMemoryAppleCredentialBackend) {
        let backend = InMemoryAppleCredentialBackend()
        return (AppleCredentialStore(service: "test.\(UUID().uuidString)", backend: backend), backend)
    }

    func testRoundTripAndPassivePresence() throws {
        let (store, _) = store()
        XCTAssertFalse(try store.exists("alpha", scope: .device).get())
        try store.write("opaque-fixture", account: "alpha", scope: .device).get()
        XCTAssertTrue(try store.exists("alpha", scope: .device).get())
        XCTAssertEqual(try store.read("alpha", scope: .device).get(), "opaque-fixture")
    }

    func testScopesAndAccountsAreIsolated() throws {
        let (store, _) = store()
        try store.write("synced-fixture", account: "alpha", scope: .synced).get()
        try store.write("device-fixture", account: "alpha", scope: .device).get()
        try store.write("other-fixture", account: "beta", scope: .device).get()
        XCTAssertEqual(try store.read("alpha", scope: .synced).get(), "synced-fixture")
        XCTAssertEqual(try store.read("alpha", scope: .device).get(), "device-fixture")
        XCTAssertNil(try store.read("beta", scope: .synced).get())
        XCTAssertEqual(try store.copySyncedToDeviceOnly("alpha").get(), true)
        XCTAssertEqual(try store.read("alpha", scope: .device).get(), "synced-fixture")
    }

    func testDeleteEverywhereAndInputValidation() throws {
        let (store, _) = store()
        try store.write("fixture", account: "alpha", scope: .synced).get()
        try store.write("fixture", account: "alpha", scope: .device).get()
        try store.deleteEverywhere("alpha").get()
        XCTAssertFalse(try store.exists("alpha", scope: .synced).get())
        XCTAssertFalse(try store.exists("alpha", scope: .device).get())
        XCTAssertThrowsError(try store.write("fixture", account: "bad account", scope: .device).get())
        XCTAssertThrowsError(try store.write("", account: "alpha", scope: .device).get())
    }

#if canImport(Security)
    /// Pure query construction: no Keychain call is made, so no real item or prompt is possible.
    func testProductionQueriesPreserveExplicitSynchronizableScope() {
        let backend = SecurityAppleCredentialBackend(accessGroup: nil)
        let synced = backend.scoped("svc.test", "acct", .synced)
        let device = backend.scoped("svc.test", "acct", .device)
        XCTAssertEqual(synced[kSecAttrSynchronizable as String] as? Bool, true)
        XCTAssertEqual(device[kSecAttrSynchronizable as String] as? Bool, false)
        XCTAssertEqual(SecurityAppleCredentialBackend.accessibility(for: .synced) as String, kSecAttrAccessibleAfterFirstUnlock as String)
        XCTAssertEqual(SecurityAppleCredentialBackend.accessibility(for: .device) as String, kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly as String)
        for scope in AppleCredentialScope.allCases {
            let value = backend.scoped("svc.test", "acct", scope)[kSecAttrSynchronizable as String] as? Bool
            XCTAssertNotNil(value, "scoped query must never use the broad kSecAttrSynchronizableAny wildcard")
        }
    }
#endif
}

private extension Result {
    func get() throws -> Success {
        switch self {
        case let .success(value): return value
        case let .failure(error): throw error
        }
    }
}
