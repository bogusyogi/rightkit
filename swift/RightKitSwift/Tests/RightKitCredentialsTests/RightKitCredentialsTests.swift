import Foundation
import Security
import Testing
@testable import RightKitCredentials

private func memoryStore(_ env: [String: String] = [:], guardEnv: [String] = [], mode: SecretsGuardMode = .memory) throws -> SecretsStore {
    try SecretsStore(namespace: "test.\(UUID().uuidString)", guardEnv: guardEnv, guardMode: mode, environment: env, backend: MemorySecretsBackend())
}

@Test func setGetDeleteReportExistence() throws {
    let s = try memoryStore()
    #expect(try s.get("k") == nil)
    try s.setString("k", "v1"); try s.setString("k", "v2")
    #expect(try s.getString("k") == "v2")
    #expect(try s.delete("k") == true)
    #expect(try s.delete("k") == false)
}

@Test func setIfAbsentNeverOverwrites() throws {
    let s = try memoryStore()
    #expect(try s.setIfAbsent("k", string: "first"))
    #expect(try s.setIfAbsent("k", string: "second") == false)
    #expect(try s.getString("k") == "first")
}

@Test func rotationBeginCommitAbort() throws {
    let s = try memoryStore()
    try s.setString("key", "old")
    #expect(try s.beginRotation("key", new: Data("new".utf8)))
    #expect(try s.beginRotation("key", new: Data("other".utf8)) == false)
    #expect(try s.getString("key") == "old")
    #expect(try s.abortRotation("key"))
    #expect(try s.commitRotation("key") == false)
    #expect(try s.beginRotation("key", new: Data("new".utf8)))
    #expect(try s.commitRotation("key"))
    #expect(try s.getString("key") == "new")
    #expect(try s.pendingRotation("key") == nil)
    #expect(throws: SecretsError.self) { _ = try s.beginRotation("key.rotation-pending", new: Data("x".utf8)) }
}

@Test func invalidInputsAreTypedErrors() throws {
    let s = try memoryStore()
    #expect(throws: SecretsError.invalid("value is empty")) { try s.set("k", Data()) }
    #expect(throws: SecretsError.invalid("key is empty")) { _ = try s.get("") }
    #expect(throws: SecretsError.invalid("namespace is empty")) { _ = try SecretsStore(namespace: "") }
}

@Test func guardMemoryAndDeny() throws {
    let mem = try memoryStore(["APP_NO_KEYCHAIN": "1"], guardEnv: ["APP_NO_KEYCHAIN"])
    #expect(mem.guardActive == "APP_NO_KEYCHAIN" && !mem.isDurable)
    try mem.setString("k", "v"); #expect(try mem.getString("k") == "v")
    _ = try mem.delete("k")
    let deny = try memoryStore(["RIGHTKIT_NO_KEYCHAIN": "1"], mode: .deny)
    #expect(throws: SecretsError.guarded("RIGHTKIT_NO_KEYCHAIN")) { _ = try deny.get("k") }
    let off = try memoryStore(["APP_NO_KEYCHAIN": "0"], guardEnv: ["APP_NO_KEYCHAIN"])
    #expect(off.guardActive == nil)
}

@Test func keychainIdentityIsNamespaceAccountDeviceOnlyAndGroupScoped() {
    let b = KeychainSecretsBackend(accessGroup: "group.org.rightkit.test")
    let q = b.identity(namespace: "svc", key: "acct")
    #expect(q[kSecAttrService as String] as? String == "svc")
    #expect(q[kSecAttrAccount as String] as? String == "acct")
    #expect(q[kSecAttrSynchronizable as String] as? Bool == false)
    #expect(q[kSecAttrAccessGroup as String] as? String == "group.org.rightkit.test")
    #expect(KeychainSecretsBackend().identity(namespace: "s", key: "a")[kSecAttrAccessGroup as String] == nil)
    #expect(SecretsAccessibility.afterFirstUnlockThisDeviceOnly.attribute as String == kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly as String)
}

@Test func everyKeychainQueryUsesTheDataProtectionKeychain() {
    // Without the flag macOS ignores access groups and accessibility classes (file keychain).
    let grouped = KeychainSecretsBackend(accessGroup: "group.org.rightkit.test")
    #expect(grouped.identity(namespace: "svc", key: "acct")[kSecUseDataProtectionKeychain as String] as? Bool == true)
    #expect(KeychainSecretsBackend().identity(namespace: "s", key: "a")[kSecUseDataProtectionKeychain as String] as? Bool == true)
    #expect(KeychainSecretsBackend().identity(namespace: "s", key: "a")[kSecAttrSynchronizable as String] as? Bool == false)
}

@Test func legacyIdentityIsTheSameItemWithoutTheDataProtectionFlag() {
    // Pre-M11 items were written without the flag; the one-time migration reads them through this query.
    let backend = KeychainSecretsBackend(accessGroup: "group.org.rightkit.test")
    let legacy = backend.legacyIdentity(namespace: "svc", key: "acct")
    #expect(legacy[kSecUseDataProtectionKeychain as String] == nil)
    #expect(legacy[kSecAttrService as String] as? String == "svc")
    #expect(legacy[kSecAttrAccount as String] as? String == "acct")
    #expect(legacy[kSecAttrSynchronizable as String] as? Bool == false)
    #expect(legacy[kSecAttrAccessGroup as String] as? String == "group.org.rightkit.test")
}

/// Real Keychain round trip. Where the host denies Keychain access (no entitlement / locked) the
/// typed `.keychain` error is the expected, non-plaintext outcome and the test records it.
@Test func realKeychainRoundTripOrTypedFailure() throws {
    let s = try SecretsStore(namespace: "test.rightkit.secrets.\(UUID().uuidString)", environment: [:])
    do {
        #expect(try s.setIfAbsent("k", string: "a"))
        #expect(try s.setIfAbsent("k", string: "b") == false)
        try s.setString("k", "c")
        #expect(try s.getString("k") == "c")
        #expect(try s.beginRotation("k", new: Data("d".utf8)))
        #expect(try s.commitRotation("k"))
        #expect(try s.getString("k") == "d")
        #expect(try s.delete("k")); #expect(try s.delete("k") == false)
    } catch let SecretsError.keychain(status) {
        #expect(status != errSecSuccess)
    }
}

// MARK: Key enumeration (#601) and per-item tag (#602)

@Test func keysListsAccountNamesSortedAndFiltersByPrefix() throws {
    let s = try memoryStore()
    try s.setString("mcp.b", "1"); try s.setString("mcp.a", "2"); try s.setString("other", "3")
    #expect(try s.keys() == ["mcp.a", "mcp.b", "other"])
    #expect(try s.keys(prefix: "mcp.") == ["mcp.a", "mcp.b"])
    #expect(try s.keys(prefix: "zzz").isEmpty)
}

@Test func keysIsScopedToTheNamespaceAndHidesRotationMarkers() throws {
    let backend = MemorySecretsBackend()
    let a = try SecretsStore(namespace: "ns.a", environment: [:], backend: backend)
    let b = try SecretsStore(namespace: "ns.b", environment: [:], backend: backend)
    try a.setString("k1", "v"); try b.setString("k2", "v")
    #expect(try a.beginRotation("k1", new: Data("n".utf8)))
    #expect(try a.keys() == ["k1"])
    #expect(try b.keys() == ["k2"])
}

@Test func keysFailsClosedOnABackendThatCannotListAndOnADeniedGuard() throws {
    struct NoList: SecretsBackend {
        func get(namespace: String, key: String) throws -> Data? { nil }
        func add(_ value: Data, namespace: String, key: String) throws -> Bool { true }
        func update(_ value: Data, namespace: String, key: String) throws -> Bool { false }
        func delete(namespace: String, key: String) throws -> Bool { false }
        func reapplyAccessibility(namespace: String, key: String) throws -> Bool { false }
    }
    let s = try SecretsStore(namespace: "n", environment: [:], backend: NoList())
    #expect(throws: SecretsError.self) { _ = try s.keys() }
    #expect(throws: SecretsError.self) { try s.setString("k", "v", tag: "t") }
    let denied = try memoryStore(["RIGHTKIT_NO_KEYCHAIN": "1"], mode: .deny)
    #expect(throws: SecretsError.guarded("RIGHTKIT_NO_KEYCHAIN")) { _ = try denied.keys() }
}

@Test func tagIsStoredBesideTheValueAndReadableWithoutIt() throws {
    let s = try memoryStore()
    #expect(try s.tag("k") == nil)
    try s.setString("k", "secret", tag: "bearer")
    #expect(try s.tag("k") == "bearer")
    #expect(try s.getString("k") == "secret")
    try s.setString("k", "secret2", tag: "api_key")
    #expect(try s.tag("k") == "api_key")
    #expect(try s.getString("k") == "secret2")
    #expect(try s.delete("k"))
    #expect(try s.tag("k") == nil)
}

@Test func untaggedItemsHaveNoTagAndAnUntaggedOverwriteKeepsTheTag() throws {
    let s = try memoryStore()
    try s.setString("plain", "v")
    #expect(try s.tag("plain") == nil)
    try s.setString("t", "v", tag: "x")
    try s.setString("t", "v2")
    #expect(try s.tag("t") == "x")
    #expect(throws: SecretsError.self) { try s.set("t", Data(), tag: "x") }
}

#if canImport(Security)
/// Hardware/entitlement-bound: needs a signed host with Keychain access, so it is skipped by default.
@Test(.disabled("needs a signed host with Keychain access; run from an app-hosted test bundle"))
func keychainBackendListsAndTagsRoundTrip() throws {
    let ns = "org.rightkit.tests.\(UUID().uuidString)"
    let s = try SecretsStore(namespace: ns, environment: [:])
    defer { for k in (try? s.keys()) ?? [] { _ = try? s.delete(k) } }
    try s.setString("a.1", "v", tag: "bearer")
    #expect(try s.keys(prefix: "a.") == ["a.1"])
    #expect(try s.tag("a.1") == "bearer")
}
#endif
