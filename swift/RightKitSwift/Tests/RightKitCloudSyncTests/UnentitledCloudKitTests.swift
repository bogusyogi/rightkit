#if os(macOS) && canImport(CloudKit)
import Foundation
import Testing
import RightKitCloudSync

@Test func unentitledTransportFailsWithTypedPermissionDeniedWithoutConstructingContainer() async throws {
    let identifier = "iCloud.invalid.rightkit.qualification"
    let entitlement = CloudKitEntitlement.current(containerIdentifier: identifier)
    try #require(!entitlement.allowsCloudKit, "This test requires an unentitled native test runner")
    let transport = CloudKitSyncTransport(containerIdentifier: identifier, zoneName: "Qualification", subscriptionID: "qualification-mac")
    await #expect(throws: SyncTransportError.permissionDenied) { _ = try await transport.accountState() }
    await #expect(throws: SyncTransportError.permissionDenied) { _ = try await transport.accountIdentity() }
    await #expect(throws: SyncTransportError.permissionDenied) { try await transport.prepareZone() }
    await #expect(throws: SyncTransportError.permissionDenied) { try await transport.ensureSubscription() }
    await #expect(throws: SyncTransportError.permissionDenied) { _ = try await transport.fetchChanges(since: nil) }
    await #expect(throws: SyncTransportError.permissionDenied) { _ = try await transport.fetchRecord(named: "probe") }
    let writes = try await transport.save([.init(recordName: "probe", recordType: "Qualification", fields: [:])])
    #expect(writes["probe"] == .failed(.permissionDenied))
}

@MainActor @Test func unentitledWakeHostRemainsDisabledWithoutCloudKitContainerOrObserverDelivery() throws {
    let identifier = "iCloud.invalid.rightkit.qualification"
    try #require(!CloudKitEntitlement.current(containerIdentifier: identifier).allowsCloudKit)
    let host = CloudKitWakeHost(configuration: .init(
        containerIdentifier: identifier, zoneName: "Qualification", subscriptionID: "qualification-mac",
        logSubsystem: "invalid.rightkit.qualification"))
    host.setObserver { _ in Issue.record("An unentitled host must never deliver a wake") }
    host.setEnabled(true)
    host.invalidateSubscription()
    host.setEnabled(false)
    host.setObserver(nil)
}
#endif
