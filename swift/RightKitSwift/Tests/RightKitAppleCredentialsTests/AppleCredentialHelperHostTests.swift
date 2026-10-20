import Foundation
import XCTest
@testable import RightKitAppleCredentials

/// PK16-2 (HeardRight helper credential hosting): the wire protocol of the credential CLI helper, run
/// over an in-memory backend. Response lines are exact (sorted keys), because the Rust host parses
/// them (`src-tauri/src/pro_bridge.rs`). No Keychain item is touched.
final class AppleCredentialHelperHostTests: XCTestCase {
    private final class Scripted: AppleCredentialBackend, @unchecked Sendable {
        let inner = InMemoryAppleCredentialBackend()
        private let lock = NSLock()
        private var _failDeviceWrites = false
        private var _failReads = false
        private var _statusFailure: AppleCredentialError?
        var failDeviceWrites: Bool {
            get { lock.withLock { _failDeviceWrites } }
            set { lock.withLock { _failDeviceWrites = newValue } }
        }
        var failReads: Bool {
            get { lock.withLock { _failReads } }
            set { lock.withLock { _failReads = newValue } }
        }
        var statusFailure: AppleCredentialError? {
            get { lock.withLock { _statusFailure } }
            set { lock.withLock { _statusFailure = newValue } }
        }

        func status() -> Result<String?, AppleCredentialError> { .success(nil) }
        func status(service: String) -> Result<String?, AppleCredentialError> {
            if let failure = statusFailure { return .failure(failure) }
            return .success(nil)
        }
        func read(service: String, account: String, scope: AppleCredentialScope) -> Result<Data?, AppleCredentialError> {
            if failReads { return .failure(.keychainStatus(-25308)) }
            return inner.read(service: service, account: account, scope: scope)
        }
        func exists(service: String, account: String, scope: AppleCredentialScope) -> Result<Bool, AppleCredentialError> {
            inner.exists(service: service, account: account, scope: scope)
        }
        func write(_ data: Data, service: String, account: String, scope: AppleCredentialScope) -> Result<Void, AppleCredentialError> {
            if failDeviceWrites && scope == .device { return .failure(.keychainStatus(-25308)) }
            return inner.write(data, service: service, account: account, scope: scope)
        }
        func delete(service: String, account: String, scope: AppleCredentialScope) -> Result<Void, AppleCredentialError> {
            inner.delete(service: service, account: account, scope: scope)
        }
    }

    private let configuration = AppleCredentialHelperHost.Configuration(
        accounts: ["pro.bridge", "sync.consent"], defaultAccount: "pro.bridge",
        missingEntitlementDetail: "This build is missing the keychain-access-groups entitlement for device continuity."
    )

    private func make() -> (AppleCredentialHelperHost, Scripted) {
        let backend = Scripted()
        let store = AppleCredentialStore(service: "app.example.shared.credentials", accessGroup: "TEAM.app.example.shared", backend: backend)
        return (AppleCredentialHelperHost(configuration: configuration, store: store, backend: backend), backend)
    }

    private func run(_ host: AppleCredentialHelperHost, _ json: String) -> String {
        let (line, exit) = host.respond(toLine: json)
        XCTAssertEqual(exit, 0, line)
        return line
    }

    func testStatusReadyWhenTheKeychainIsUsable() {
        let (host, _) = make()
        XCTAssertEqual(run(host, #"{"command":"status"}"#), #"{"ok":true,"status":"ready"}"#)
    }

    func testMissingEntitlementIsAnUnavailableStatusInTheProductsWords() {
        let (host, backend) = make()
        backend.statusFailure = .missingEntitlement
        XCTAssertEqual(
            run(host, #"{"command":"status"}"#),
            #"{"detail":"This build is missing the keychain-access-groups entitlement for device continuity.","ok":true,"status":"unavailable"}"#
        )
        backend.statusFailure = .unavailable("Keychain unavailable.")
        XCTAssertEqual(run(host, #"{"command":"status"}"#), #"{"detail":"Keychain unavailable.","ok":true,"status":"unavailable"}"#)
    }

    func testWriteThenReadRoundTripsAndAnAbsentItemReadsAsExplicitNull() {
        let (host, _) = make()
        XCTAssertEqual(run(host, #"{"command":"read","scope":"synced"}"#), #"{"ok":true,"value":null}"#)
        XCTAssertEqual(run(host, #"{"command":"write","scope":"synced","value":"bridge-token"}"#), #"{"ok":true}"#)
        // `account` omitted means the default account, as older callers sent it.
        XCTAssertEqual(run(host, #"{"command":"read","scope":"synced"}"#), #"{"ok":true,"value":"bridge-token"}"#)
        XCTAssertEqual(run(host, #"{"command":"read","scope":"synced","account":"pro.bridge"}"#), #"{"ok":true,"value":"bridge-token"}"#)
        XCTAssertEqual(run(host, #"{"command":"read","scope":"synced","account":"sync.consent"}"#), #"{"ok":true,"value":null}"#)
    }

    func testASyncedReadNeverReturnsADeviceOnlyCopy() {
        let (host, _) = make()
        XCTAssertEqual(run(host, #"{"command":"write","scope":"device","value":"local"}"#), #"{"ok":true}"#)
        XCTAssertEqual(run(host, #"{"command":"read","scope":"synced"}"#), #"{"ok":true,"value":null}"#)
        XCTAssertEqual(run(host, #"{"command":"read","scope":"device"}"#), #"{"ok":true,"value":"local"}"#)
    }

    func testRequestsOutsideTheAllowlistOrWithBadFieldsAreRefusedBeforeTheKeychain() {
        let (host, backend) = make()
        XCTAssertEqual(run(host, #"{"command":"read"}"#), #"{"error":"missing or invalid scope","ok":false}"#)
        XCTAssertEqual(run(host, #"{"command":"read","scope":"everywhere"}"#), #"{"error":"missing or invalid scope","ok":false}"#)
        XCTAssertEqual(run(host, #"{"command":"read","scope":"synced","account":"other.secret"}"#), #"{"error":"missing or invalid account","ok":false}"#)
        XCTAssertEqual(run(host, #"{"command":"write","scope":"synced"}"#), #"{"error":"missing value","ok":false}"#)
        XCTAssertEqual(run(host, #"{"command":"write","scope":"synced","value":""}"#), #"{"error":"missing value","ok":false}"#)
        XCTAssertEqual(run(host, #"{"command":"write","scope":"synced","value":"v","account":"other.secret"}"#), #"{"error":"missing or invalid account","ok":false}"#)
        XCTAssertEqual(run(host, #"{"command":"delete_everywhere","account":"other.secret"}"#), #"{"error":"missing or invalid account","ok":false}"#)
        XCTAssertEqual(run(host, #"{"command":"copy_synced_to_device_only","account":"other.secret"}"#), #"{"error":"missing or invalid account","ok":false}"#)
        XCTAssertEqual(run(host, #"{"command":"shutdown"}"#), #"{"error":"unknown command: shutdown","ok":false}"#)
        XCTAssertEqual(try? backend.inner.accounts(service: "app.example.shared.credentials", scope: .synced).get(), [])
    }

    func testAFailedReadIsAnErrorNeverAbsent() {
        let (host, backend) = make()
        backend.failReads = true
        XCTAssertEqual(
            run(host, #"{"command":"read","scope":"synced"}"#),
            #"{"error":"Keychain operation failed (status -25308).","ok":false}"#
        )
    }

    func testTurningSyncOffCopiesToDeviceOnlyAndLeavesTheSyncedItem() {
        let (host, backend) = make()
        XCTAssertEqual(run(host, #"{"command":"copy_synced_to_device_only"}"#), #"{"copied":false,"ok":true}"#, "nothing synced is not an error")
        _ = run(host, #"{"command":"write","scope":"synced","value":"tok"}"#)
        XCTAssertEqual(run(host, #"{"command":"copy_synced_to_device_only"}"#), #"{"copied":true,"ok":true}"#)
        XCTAssertEqual(run(host, #"{"command":"read","scope":"device"}"#), #"{"ok":true,"value":"tok"}"#)
        XCTAssertEqual(run(host, #"{"command":"read","scope":"synced"}"#), #"{"ok":true,"value":"tok"}"#, "the synced item is never deleted")
        XCTAssertNotNil(backend)
    }

    func testACopyThatCouldNotBeWrittenIsNeverReportedAsCopied() {
        let (host, backend) = make()
        _ = run(host, #"{"command":"write","scope":"synced","value":"tok"}"#)
        backend.failDeviceWrites = true
        XCTAssertEqual(
            run(host, #"{"command":"copy_synced_to_device_only"}"#),
            #"{"error":"Keychain operation failed (status -25308).","ok":false}"#
        )
        XCTAssertEqual(run(host, #"{"command":"read","scope":"synced"}"#), #"{"ok":true,"value":"tok"}"#)
    }

    func testDeleteEverywhereRemovesBothCopiesOfOneAccountOnly() {
        let (host, _) = make()
        _ = run(host, #"{"command":"write","scope":"synced","value":"tok"}"#)
        _ = run(host, #"{"command":"write","scope":"device","value":"tok"}"#)
        _ = run(host, #"{"command":"write","scope":"synced","account":"sync.consent","value":"yes"}"#)
        XCTAssertEqual(run(host, #"{"command":"delete_everywhere"}"#), #"{"ok":true}"#)
        XCTAssertEqual(run(host, #"{"command":"read","scope":"synced"}"#), #"{"ok":true,"value":null}"#)
        XCTAssertEqual(run(host, #"{"command":"read","scope":"device"}"#), #"{"ok":true,"value":null}"#)
        XCTAssertEqual(run(host, #"{"command":"read","scope":"synced","account":"sync.consent"}"#), #"{"ok":true,"value":"yes"}"#)
        XCTAssertEqual(run(host, #"{"command":"delete_everywhere"}"#), #"{"ok":true}"#, "idempotent")
    }

    func testMalformedRequestLinesExitNonZeroWithTheSharedFailureLine() {
        let (host, _) = make()
        XCTAssertEqual(host.respond(toLine: nil).line, #"{"error":"no request on stdin","ok":false}"#)
        XCTAssertEqual(host.respond(toLine: nil).exitCode, 1)
        XCTAssertEqual(host.respond(toLine: "   ").exitCode, 1)
        let garbage = host.respond(toLine: "not json")
        XCTAssertEqual(garbage.line, #"{"error":"invalid request JSON","ok":false}"#)
        XCTAssertEqual(garbage.exitCode, 1)
    }

    func testTheDefaultBackendDeletesBothScopes() {
        let backend = InMemoryAppleCredentialBackend()
        _ = backend.write(Data("a".utf8), service: "s", account: "x", scope: .synced)
        _ = backend.write(Data("b".utf8), service: "s", account: "x", scope: .device)
        XCTAssertNoThrow(try backend.deleteEverywhere(service: "s", account: "x").get())
        XCTAssertNil(try backend.read(service: "s", account: "x", scope: .synced).get())
        XCTAssertNil(try backend.read(service: "s", account: "x", scope: .device).get())
    }
}
