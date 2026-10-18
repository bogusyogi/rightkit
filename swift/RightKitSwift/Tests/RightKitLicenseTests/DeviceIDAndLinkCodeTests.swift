import Foundation
import Security
import Testing
import RightKitCredentials
@testable import RightKitLicense

private func memStore() throws -> SecretsStore {
    try SecretsStore(namespace: "test.\(UUID().uuidString)", environment: [:], backend: MemorySecretsBackend())
}

private func licenseStore() -> KeychainLicenseStore {
    KeychainLicenseStore(service: "test.\(UUID().uuidString)", account: "activation")
}

// MARK: #702 device-id options

@Test func newInstallDeviceIDUsesTheCallerSeed() throws {
    let s = try memStore()
    let id = try licenseStore().deviceIDLocked(account: "device-id", store: s, seed: { "VENDOR-ID-1" }, reapply: false)
    #expect(id == "VENDOR-ID-1")
    // Persisted: a later call with a different seed returns the stored id.
    #expect(try licenseStore().deviceIDLocked(account: "device-id", store: s, seed: { "OTHER" }, reapply: false) == "VENDOR-ID-1")
}

@Test func emptyOrNilSeedFallsBackToARandomUUID() throws {
    for seed in [nil, { "  " }, { nil }] as [(@Sendable () -> String?)?] {
        let id = try licenseStore().deviceIDLocked(account: "device-id", store: try memStore(), seed: seed, reapply: false)
        #expect(UUID(uuidString: id) != nil)
    }
}

@Test func corruptIDReseedsFromTheSeedAndFailClosedStaysThePlainDefault() throws {
    let s = try memStore()
    let store = licenseStore()
    try s.set("device-id", Data([0xff, 0xfe]))
    #expect(throws: LicenseError.corruptDeviceID) {
        _ = try store.deviceIDLocked(account: "device-id", store: s, seed: { "S" }, reapply: false)
    }
    let reseeded = try store.replaceCorruptDeviceIDLocked(account: "device-id", store: s, seed: { "S" })
    #expect(reseeded == "S")
    #expect(try s.getString("device-id") == "S")
}

@Test func optionsDefaultsDoNotRewriteAccessibilityAndFailClosed() {
    let o = KeychainLicenseStore.DeviceIDOptions()
    #expect(o.reapplyAccessibilityOnRead == false)
    #expect(o.onCorrupt == .failClosed)
    #expect(o.accessibility == .afterFirstUnlockThisDeviceOnly)
    #expect(KeychainLicenseStore.DeviceIDOptions(accessibility: .afterFirstUnlock).accessibility == .afterFirstUnlock)
}

// MARK: #603 public platform

@Test func platformIsPublicAndOneOfTheLeaseWireValues() async {
    let value = await LicenseActivationClient.platform()
    #expect(["ios", "ipados", "macos"].contains(value))
}

// MARK: #365 Windows link code

@Test func devicesURLSitsBesideTheLeaseEndpoint() {
    let config = LicenseClientConfiguration(app: "coderight", baseURL: URL(string: "https://example.test/v1/licenses")!, deviceID: { "d" })
    #expect(config.devicesURL(["windows-link", "request"]).absoluteString == "https://example.test/v1/pro/coderight/devices/windows-link/request")
    #expect(config.leaseURL(["renew"]).absoluteString == "https://example.test/v1/pro/coderight/devices/lease/renew")
    #expect(config.leaseURL([]).absoluteString == "https://example.test/v1/pro/coderight/devices/lease")
}

@Test func windowsLinkCodeRequiresALeaseBoundActivation() async {
    let config = LicenseClientConfiguration(app: "coderight", baseURL: URL(string: "https://example.test/v1/licenses")!, deviceID: { "d" })
    let client = LicenseActivationClient(configuration: config, store: InMemoryLicenseStore())
    await #expect(throws: LicenseError.self) { _ = try await client.requestWindowsLinkCode() }
    await #expect(throws: LicenseError.self) {
        _ = try await client.requestWindowsLinkCode(activationToken: "", leaseId: "l", deviceId: "d")
    }
}
