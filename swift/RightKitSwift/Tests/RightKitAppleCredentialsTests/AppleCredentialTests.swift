import Foundation
import XCTest
@testable import RightKitAppleCredentials

#if canImport(Security)
import Security
import LocalAuthentication
#endif

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
    func testProbePreservesEveryRawStatus() {
        let cases: [(OSStatus, CredentialProbe)] = [
            (errSecSuccess, .available),
            (errSecItemNotFound, .notFound),
            (errSecMissingEntitlement, .missingEntitlement),
            (errSecInteractionNotAllowed, .interactionNotAllowed),
            (errSecAuthFailed, .failure(errSecAuthFailed)),
            (-12345, .failure(-12345)),
        ]
        for (status, expected) in cases {
            let probe = CredentialProbe(rawStatus: status)
            XCTAssertEqual(probe, expected)
            XCTAssertEqual(probe.rawStatus, status)
        }
    }

    func testProbeQueriesPreserveIdentityAndNeverRequestDataOrUI() {
        let backend = SecurityAppleCredentialBackend(accessGroup: "test.group")
        let scopes: [AppleCredentialScope?] = [nil, .synced, .device]
        for scope in scopes {
            let query = backend.probeQuery("test.service", "test.account", scope)
            XCTAssertEqual(query[kSecAttrService as String] as? String, "test.service")
            XCTAssertEqual(query[kSecAttrAccount as String] as? String, "test.account")
            XCTAssertEqual(query[kSecAttrAccessGroup as String] as? String, "test.group")
            XCTAssertEqual(query[kSecUseDataProtectionKeychain as String] as? Bool, true)
            XCTAssertEqual(query[kSecUseAuthenticationUI as String] as? String, kSecUseAuthenticationUISkip as String)
            // LAContext's property readback requires CoreAuthentication, which sandboxed hosts can deny.
            XCTAssertNotNil(query[kSecUseAuthenticationContext as String] as? LAContext)
            XCTAssertEqual(query[kSecReturnData as String] as? Bool, false)
            XCTAssertEqual(query[kSecMatchLimit as String] as? String, kSecMatchLimitOne as String)
            if let scope {
                XCTAssertEqual(query[kSecAttrSynchronizable as String] as? Bool, scope == .synced)
            } else {
                XCTAssertEqual(query[kSecAttrSynchronizable as String] as? String, kSecAttrSynchronizableAny as String)
            }
        }
    }

    func testBroadDeletionAttemptsEveryLocationAfterFailure() {
        let backend = SecurityAppleCredentialBackend(accessGroup: "test.group")
        var queries: [[String: Any]] = []
        let outcomes = backend.deleteAnyScope(service: "test.service", account: "test.account", deleteItem: { query in
            queries.append(query as! [String: Any])
            return queries.count == 1 ? errSecMissingEntitlement : errSecItemNotFound
        })
        XCTAssertEqual(outcomes.first?.scope, .dataProtection)
        XCTAssertEqual(outcomes.first?.rawStatus, errSecMissingEntitlement)
        XCTAssertEqual(outcomes.first?.succeeded, false)
        XCTAssertEqual(queries[0][kSecAttrSynchronizable as String] as? String, kSecAttrSynchronizableAny as String)
        XCTAssertEqual(queries[0][kSecAttrAccessGroup as String] as? String, "test.group")
        XCTAssertEqual(queries[0][kSecUseDataProtectionKeychain as String] as? Bool, true)
#if os(macOS)
        XCTAssertEqual(outcomes.count, 2)
        XCTAssertEqual(outcomes[1].scope, .legacyFile)
        XCTAssertEqual(outcomes[1].rawStatus, errSecItemNotFound)
        XCTAssertTrue(outcomes[1].succeeded)
        XCTAssertEqual(queries[1][kSecUseDataProtectionKeychain as String] as? Bool, false)
        XCTAssertNil(queries[1][kSecAttrSynchronizable as String])
        XCTAssertNil(queries[1][kSecAttrAccessGroup as String])
#else
        XCTAssertEqual(outcomes.count, 1)
#endif
        for query in queries {
            XCTAssertEqual(query[kSecAttrService as String] as? String, "test.service")
            XCTAssertEqual(query[kSecAttrAccount as String] as? String, "test.account")
            XCTAssertNotNil(query[kSecUseAuthenticationContext as String] as? LAContext)
            XCTAssertEqual(query[kSecUseAuthenticationUI as String] as? String, kSecUseAuthenticationUIFail as String)
            XCTAssertNil(query[kSecMatchLimit as String], "deletion must remove all matching items")
            XCTAssertNil(query[kSecReturnData as String])
        }
        XCTAssertTrue(CredentialDeletionOutcome(scope: .dataProtection, rawStatus: errSecSuccess).succeeded)
        XCTAssertTrue(CredentialDeletionOutcome(scope: .dataProtection, rawStatus: errSecItemNotFound).succeeded)
        XCTAssertFalse(CredentialDeletionOutcome(scope: .dataProtection, rawStatus: errSecAuthFailed).succeeded)
    }

    func testRealProbeMatchesRawSecurityStatus() {
        let backend = SecurityAppleCredentialBackend(accessGroup: "org.rightkit.tests.\(UUID().uuidString)")
        let service = "org.rightkit.tests.\(UUID().uuidString)"
        let query = backend.probeQuery(service, "probe", nil)
        let rawStatus = SecItemCopyMatching(query as CFDictionary, nil)
        let probe = backend.probeStatus(service: service, account: "probe")
        XCTAssertNotEqual(rawStatus, errSecSuccess, "a unique service cannot contain a fixture")
        XCTAssertEqual(probe.rawStatus, rawStatus)
        XCTAssertEqual(probe, CredentialProbe(rawStatus: rawStatus))
    }

    func testRealBroadDeletionOfAbsentAccountPreservesPerLocationStatus() {
        let backend = SecurityAppleCredentialBackend()
        let service = "org.rightkit.tests.\(UUID().uuidString)"
        let outcomes = backend.deleteAnyScope(service: service, account: "absent")
        XCTAssertEqual(outcomes.first?.scope, .dataProtection)
#if os(macOS)
        XCTAssertEqual(outcomes.map(\.scope), [.dataProtection, .legacyFile])
#else
        XCTAssertEqual(outcomes.map(\.scope), [.dataProtection])
#endif
        for outcome in outcomes {
            let query = backend.deletionQuery(service, "absent", outcome.scope)
            let rawStatus = SecItemDelete(query as CFDictionary)
            XCTAssertEqual(outcome.rawStatus, rawStatus)
            XCTAssertEqual(outcome.succeeded, rawStatus == errSecSuccess || rawStatus == errSecItemNotFound)
        }
    }

    func testRealBroadDeletionRemovesBothSyncStatesAndPreservesOtherAccounts() throws {
        let backend = SecurityAppleCredentialBackend()
        let service = "org.rightkit.tests.\(UUID().uuidString)"
        defer {
            _ = backend.deleteAnyScope(service: service, account: "alpha")
            _ = backend.deleteAnyScope(service: service, account: "beta")
        }
        // SwiftPM hosts can lack data-protection Keychain entitlement/session access.
        if case let .failure(error) = backend.write(Data("device-fixture".utf8), service: service, account: "alpha", scope: .device) {
            try skipUnavailableKeychain(error)
            throw error
        }
        XCTAssertEqual(backend.probeStatus(service: service, account: "alpha", scope: .device), .available)
        if case let .failure(error) = backend.write(Data("synced-fixture".utf8), service: service, account: "alpha", scope: .synced) {
            try skipUnavailableKeychain(error)
            throw error
        }
        try backend.write(Data("other-fixture".utf8), service: service, account: "beta", scope: .device).get()
        let outcomes = backend.deleteAnyScope(service: service, account: "alpha")
        XCTAssertTrue(outcomes.allSatisfy(\.succeeded), "\(outcomes)")
        XCTAssertEqual(backend.probeStatus(service: service, account: "alpha", scope: .synced), .notFound)
        XCTAssertEqual(backend.probeStatus(service: service, account: "alpha", scope: .device), .notFound)
        XCTAssertEqual(try backend.read(service: service, account: "beta", scope: .device).get(), Data("other-fixture".utf8))
        let repeated = backend.deleteAnyScope(service: service, account: "alpha")
        XCTAssertTrue(repeated.allSatisfy(\.succeeded))
        XCTAssertTrue(repeated.allSatisfy { $0.rawStatus == errSecItemNotFound })
    }

    private func skipUnavailableKeychain(_ error: AppleCredentialError) throws {
        if case let .keychainStatus(status) = error,
           [errSecMissingEntitlement, errSecNotAvailable, errSecInteractionNotAllowed].contains(status) {
            throw XCTSkip("Test host cannot access Keychain: \(error)")
        }
    }

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
