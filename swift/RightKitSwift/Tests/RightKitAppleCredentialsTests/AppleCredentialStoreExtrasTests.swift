import Foundation
import XCTest
@testable import RightKitAppleCredentials

#if canImport(Security)
import Security
#endif

/// #605 (account validator, raw data, enumeration) and #699 (broad synced read, status service).
final class AppleCredentialStoreExtrasTests: XCTestCase {
    private func make(validator: (@Sendable (String) -> Bool)? = nil) -> (AppleCredentialStore, InMemoryAppleCredentialBackend) {
        let backend = InMemoryAppleCredentialBackend()
        return (AppleCredentialStore(service: "test.\(UUID().uuidString)", backend: backend, accountValidator: validator), backend)
    }

    func testDefaultValidatorStillRejectsUrlShapedAccounts() {
        let (store, _) = make()
        XCTAssertThrowsError(try store.write("v", account: "daemon-token:https://a.example", scope: .device).get())
    }

    func testArbitraryAccountsValidatorAcceptsDaemonTokenAccounts() throws {
        let (store, _) = make(validator: AppleCredentialStore.arbitraryAccounts)
        let account = "daemon-token:https://a.example:8443"
        try store.write("tok", account: account, scope: .device).get()
        XCTAssertEqual(try store.read(account, scope: .device).get(), "tok")
        XCTAssertThrowsError(try store.write("v", account: "", scope: .device).get())
        XCTAssertThrowsError(try store.write("v", account: "bad\nnewline", scope: .device).get())
    }

    func testCustomValidatorReplacesTheDefaultRule() throws {
        let (store, _) = make(validator: { $0.hasPrefix("ok:") })
        try store.write("v", account: "ok:1", scope: .device).get()
        XCTAssertThrowsError(try store.write("v", account: "plain", scope: .device).get())
    }

    func testRawDataIsNotTrimmedOrReencoded() throws {
        let (store, backend) = make()
        let bytes = Data([0x20, 0xff, 0x00, 0x0a])
        try store.writeData(bytes, account: "blob", scope: .device).get()
        XCTAssertEqual(try store.readData("blob", scope: .device).get(), bytes)
        XCTAssertEqual(try backend.read(service: store.service, account: "blob", scope: .device).get(), bytes)
        XCTAssertThrowsError(try store.writeData(Data(), account: "blob", scope: .device).get())
        // The text path still trims.
        try store.write("  padded \n", account: "text", scope: .device).get()
        XCTAssertEqual(try store.read("text", scope: .device).get(), "padded")
    }

    func testSingleScopeDeleteLeavesTheOtherScope() throws {
        let (store, _) = make()
        try store.write("a", account: "x", scope: .synced).get()
        try store.write("b", account: "x", scope: .device).get()
        try store.delete("x", scope: .device).get()
        XCTAssertNil(try store.read("x", scope: .device).get())
        XCTAssertEqual(try store.read("x", scope: .synced).get(), "a")
        try store.delete("x", scope: .device).get()
    }

    func testAccountEnumerationIsPerScopeAndPerService() throws {
        let (store, backend) = make(validator: AppleCredentialStore.arbitraryAccounts)
        try store.write("1", account: "daemon-token:b", scope: .device).get()
        try store.write("2", account: "daemon-token:a", scope: .device).get()
        try store.write("3", account: "daemon-token:s", scope: .synced).get()
        let other = AppleCredentialStore(service: "test.other", backend: backend, accountValidator: AppleCredentialStore.arbitraryAccounts)
        try other.write("4", account: "foreign", scope: .device).get()
        XCTAssertEqual(try store.accounts(scope: .device).get(), ["daemon-token:a", "daemon-token:b"])
        XCTAssertEqual(try store.accounts(scope: .synced).get(), ["daemon-token:s"])
    }

    func testBroadSyncedReadFallsBackToTheSyncedScopeOnPlainBackends() throws {
        let (store, _) = make()
        try store.write("device-only", account: "x", scope: .device).get()
        XCTAssertNil(try store.read("x", scope: .synced, broadSynced: true).get())
        try store.write("synced", account: "x", scope: .synced).get()
        XCTAssertEqual(try store.read("x", scope: .synced, broadSynced: true).get(), "synced")
        // `.device` ignores the flag.
        XCTAssertEqual(try store.read("x", scope: .device, broadSynced: true).get(), "device-only")
    }

    func testStatusUsesTheStoresOwnServiceAndSurfacesMissingEntitlement() {
        final class Probe: AppleCredentialBackend, @unchecked Sendable {
            nonisolated(unsafe) var probed: [String] = []
            func status() -> Result<String?, AppleCredentialError> { .success("default") }
            func status(service: String) -> Result<String?, AppleCredentialError> {
                probed.append(service); return .failure(.missingEntitlement)
            }
            func read(service: String, account: String, scope: AppleCredentialScope) -> Result<Data?, AppleCredentialError> { .success(nil) }
            func exists(service: String, account: String, scope: AppleCredentialScope) -> Result<Bool, AppleCredentialError> { .success(false) }
            func write(_ data: Data, service: String, account: String, scope: AppleCredentialScope) -> Result<Void, AppleCredentialError> { .success(()) }
            func delete(service: String, account: String, scope: AppleCredentialScope) -> Result<Void, AppleCredentialError> { .success(()) }
        }
        let probe = Probe()
        let store = AppleCredentialStore(service: "app.coderight.shared.credentials", backend: probe)
        guard case let .failure(error) = store.status() else { return XCTFail("expected failure") }
        XCTAssertEqual(error, .missingEntitlement)
        XCTAssertEqual(probe.probed, ["app.coderight.shared.credentials"])
    }

    func testBackendWithoutEnumerationReportsUnavailable() {
        final class Minimal: AppleCredentialBackend, @unchecked Sendable {
            func status() -> Result<String?, AppleCredentialError> { .success(nil) }
            func read(service: String, account: String, scope: AppleCredentialScope) -> Result<Data?, AppleCredentialError> { .success(nil) }
            func exists(service: String, account: String, scope: AppleCredentialScope) -> Result<Bool, AppleCredentialError> { .success(false) }
            func write(_ data: Data, service: String, account: String, scope: AppleCredentialScope) -> Result<Void, AppleCredentialError> { .success(()) }
            func delete(service: String, account: String, scope: AppleCredentialScope) -> Result<Void, AppleCredentialError> { .success(()) }
        }
        let store = AppleCredentialStore(service: "s", backend: Minimal())
        guard case .failure = store.accounts(scope: .device) else { return XCTFail("expected failure") }
    }

    #if canImport(Security)
    func testMissingEntitlementStatusMapsToItsOwnError() {
        XCTAssertEqual(SecurityAppleCredentialBackend.error(forStatus: errSecMissingEntitlement), .missingEntitlement)
        XCTAssertEqual(SecurityAppleCredentialBackend.error(forStatus: errSecAuthFailed), .keychainStatus(errSecAuthFailed))
    }

    func testBroadSyncedRowFilterAcceptsOnlySynchronizableRows() {
        XCTAssertTrue(SecurityAppleCredentialBackend.isSynchronizable(true))
        XCTAssertTrue(SecurityAppleCredentialBackend.isSynchronizable(NSNumber(value: 1)))
        XCTAssertFalse(SecurityAppleCredentialBackend.isSynchronizable(false))
        XCTAssertFalse(SecurityAppleCredentialBackend.isSynchronizable(NSNumber(value: 0)))
        XCTAssertFalse(SecurityAppleCredentialBackend.isSynchronizable(nil))
    }
    #endif
}
