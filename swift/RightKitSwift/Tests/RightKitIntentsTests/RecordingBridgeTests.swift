import Testing
@testable import RightKitIntents

@Test func actionNamesAndErrorDescriptionsAreStable() {
    #expect(RightKitRecordingAction.stopAndCopy.rawValue == "stopAndCopy")
    #expect(RightKitAppIntentError.commandRejected.errorDescription?.isEmpty == false)
}

@Test @MainActor func extensionDispatchRequiresQueueAdapter() async {
    RightKitRecordingBridge.driver = nil
    do {
        _ = try await RightKitRecordingBridge.dispatch(.start)
        Issue.record("dispatch unexpectedly succeeded without driver or queue")
    } catch let error as RightKitAppIntentError {
        #expect(error == .driverUnavailable)
    } catch {
        Issue.record("unexpected error: \(error)")
    }
}

@Test @MainActor func extensionDispatchQueuesAndPostsWithoutDriver() async throws {
    RightKitRecordingBridge.driver = nil
    var queued = false
    _ = try await RightKitRecordingBridge.dispatch(.toggle) { queued = true }
    #expect(queued)
}
