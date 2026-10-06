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

@MainActor private final class StubDriver: RightKitRecordingDriver {
    let processHandle = "stub"
    var isRecording = false
    var transcript: String?
    func start(source: String) async throws {}
    func stopAndCopy(source: String) async throws -> String { "stopped" }
    func cancel(source: String) async throws {}
    func toggle(source: String) async throws {}
}

@MainActor private final class TranscriptDriver: RightKitRecordingDriver {
    let processHandle = "transcript"
    var isRecording = false
    func start(source: String) async throws {}
    func stopAndCopy(source: String) async throws -> String { "stopped" }
    func cancel(source: String) async throws {}
    func toggle(source: String) async throws {}
    func copyLatestTranscript(source: String) async throws -> String { "latest transcript from \(source)" }
}

@Test @MainActor func copyLatestTranscriptDispatchesToDriverOrFailsExplicitly() async throws {
    defer { RightKitRecordingBridge.driver = nil }
    RightKitRecordingBridge.driver = TranscriptDriver()
    #expect(try await RightKitRecordingBridge.dispatch(.copyLatestTranscript, source: "shortcut") == "latest transcript from shortcut")

    RightKitRecordingBridge.driver = StubDriver()
    await #expect(throws: RightKitAppIntentError.actionUnsupported("copyLatestTranscript")) {
        _ = try await RightKitRecordingBridge.dispatch(.copyLatestTranscript)
    }
    await #expect(throws: RightKitAppIntentError.actionUnsupported("openApp")) {
        _ = try await RightKitRecordingBridge.dispatch(.openApp)
    }
}
