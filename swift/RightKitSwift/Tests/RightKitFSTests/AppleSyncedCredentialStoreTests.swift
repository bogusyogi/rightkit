import RightKitAppleCredentials
import XCTest
@testable import RightKitFS
/// Policy tests for the iCloud Keychain sync layer (consent switch and `pro.bridge`), run against
/// RightKit's in-memory backend so they never touch the real synchronizable Keychain.
/// Values here are throwaway test strings.
final class AppleSyncedCredentialStoreTests: XCTestCase {
    private let suite = "coderight.tests.applesync.\(UUID().uuidString)"
    private let backend = InMemoryAppleCredentialBackend()
    private lazy var defaults: UserDefaults = UserDefaults(suiteName: suite) ?? .standard

    override func tearDown() async throws {
        defaults.removePersistentDomain(forName: suite)
        try await super.tearDown()
    }

    private func makeStore() -> AppleSyncedCredentialStore {
        AppleSyncedCredentialStore(defaults: defaults, backend: backend)
    }

    private func rawRemote(_ account: SyncedCredentialAccount) -> String? {
        guard case let .success(data) = backend.read(
            service: AppleSyncedCredentialStore.service, account: account.rawValue, scope: .synced
        ), let data else { return nil }
        return String(data: data, encoding: .utf8)
    }

    // MARK: - Allow-list

    func testOnlyConsentAndBridgeAccountsAreAllowed() {
        XCTAssertEqual(SyncedCredentialAccount(rawValue: "sync.consent"), .syncConsent)
        XCTAssertEqual(SyncedCredentialAccount(rawValue: "pro.bridge"), .proBridge)
        for rejected in ["license.key", "device.id", "session.token", "mcp.github", "api.anthropic.v1", "api.example.com", ""] {
            XCTAssertNil(SyncedCredentialAccount(rawValue: rejected), "\(rejected) must not sync")
        }
    }

    // MARK: - Consent off

    func testConsentOffTouchesNoKeychain() throws {
        let store = makeStore()
        XCTAssertFalse(store.isSyncEnabled)
        XCTAssertFalse(try store.write("bridge", for: .proBridge))
        XCTAssertNil(rawRemote(.proBridge))
        XCTAssertNil(store.read(.proBridge))
        XCTAssertFalse(store.exists(.proBridge))
        XCTAssertNil(store.read(.syncConsent))
    }

    // MARK: - Consent on

    func testEnableWritesConsentMarkerAndDisableKeepsTheICloudCopy() throws {
        let store = makeStore()
        store.enableSync()
        XCTAssertTrue(store.isSyncEnabled)
        XCTAssertEqual(rawRemote(.syncConsent), AppleSyncedCredentialStore.syncConsentMarker)

        XCTAssertTrue(try store.write("bridge-1", for: .proBridge))
        XCTAssertEqual(store.read(.proBridge), "bridge-1")
        XCTAssertTrue(store.exists(.proBridge))

        store.disableSync()
        XCTAssertFalse(store.isSyncEnabled)
        XCTAssertNil(store.read(.proBridge), "sync off stops reading iCloud")
        XCTAssertEqual(rawRemote(.proBridge), "bridge-1", "turning sync off never deletes the iCloud copy")
    }

    func testAdoptAccountConsentNeverEnablesSync() {
        let store = makeStore()
        XCTAssertFalse(store.adoptAccountConsentIfNeeded())
        store.enableSync()
        XCTAssertTrue(store.adoptAccountConsentIfNeeded())
        store.disableSync()
        XCTAssertFalse(store.adoptAccountConsentIfNeeded())
    }

    // MARK: - Reconcile

    func testReconcileOutcomes() throws {
        let store = makeStore()
        XCTAssertEqual(store.reconcile(.proBridge, local: nil), .unavailable)
        XCTAssertEqual(store.reconcile(.proBridge, local: "a"), .seededRemote)
        XCTAssertEqual(store.reconcile(.proBridge, local: nil), .adoptedRemote)
        XCTAssertEqual(store.reconcile(.proBridge, local: " a "), .unchanged)
        XCTAssertEqual(store.reconcile(.proBridge, local: "b"), .conflict)
        XCTAssertEqual(rawRemote(.proBridge), "a", "a conflict leaves the iCloud copy untouched")
    }

    func testEnableSyncReconcilesTheGivenAccounts() {
        let store = makeStore()
        let outcomes = store.enableSync(accounts: [(account: .proBridge, local: "bridge-1")])
        XCTAssertEqual(outcomes[.proBridge], .seededRemote)
        XCTAssertEqual(rawRemote(.proBridge), "bridge-1")
    }

    // MARK: - Explicit delete

    func testRemoveEverywhereDeletesTheICloudCopy() throws {
        let store = makeStore()
        store.enableSync()
        try store.write("bridge-1", for: .proBridge)
        store.removeEverywhere(.proBridge)
        XCTAssertNil(rawRemote(.proBridge))
        store.removeEverywhere(.proBridge)
    }
}
