import Foundation
import Security
import Testing
import RightKitLicense

@Test func synchronousLicenseStoreRoundTripsOrReportsNativeKeychainFailure() throws {
    let store = KeychainLicenseStore(service: "invalid.rightkit.qualification.\(UUID().uuidString)")
    let license = StoredLicense(status: "active", deviceId: "qualification-device", activationToken: "qualification-only")
    // Scoped cleanup only: never touch app identity or a shared access group.
    defer { try? store.removeNow() }
    do {
        try store.saveNow(license)
    } catch let error as LicenseStorageError {
        guard case let .keychain(status) = error else { Issue.record("Unexpected storage encoding failure"); return }
        #expect(status != errSecSuccess)
        #expect(store.loadNow() == nil)
        return
    }
    let loaded = try #require(store.loadNow())
    #expect(loaded.deviceId == license.deviceId)
    #expect(loaded.activationToken == license.activationToken)
    try store.removeNow()
    #expect(store.loadNow() == nil)
}
