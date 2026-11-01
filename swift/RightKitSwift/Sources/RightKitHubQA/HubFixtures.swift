import Foundation
import RightKitSystemUI
import RightKitHubQA

/// Canned data for every section, for the off-screen renderer. QA only: release builds never link
/// this target. Each section's replies live in its own `<Name>Fixtures.swift`.
public enum HubFixtures {
    /// An engine that answers every command the sections call with canned results.
    public static func engine() -> QAEngineClient {
        let engine = QAEngineClient()
        overview(engine)
        activity(engine)
        storage(engine)
        cleanup(engine)
        monitor(engine)
        membrane(engine)
        apps(engine)
        chat(engine)
        settings(engine)
        // The live-event poll answers at once with nothing, so a picture never waits on an event
        // (the renderer starts no stream anyway; this keeps a hand-run one from hanging).
        engine.respond(to: "pulse.events.poll", json: #"{"events":[],"next":0,"dropped":false}"#)
        // Several sections read the same commands (status, apps_cached, cleanup_cached, ...); the
        // registration made last wins, so each command has one reply shape for every picture.
        return engine
    }

    /// A notch bridge that serves the canned notch state and records commands.
    public static func notch() -> any PulseNotchBridge { FixtureNotchBridge(state: notchState()) }
}

final class FixtureNotchBridge: PulseNotchBridge, @unchecked Sendable {
    private let lock = NSLock()
    private let stateData: Data?
    private var commands: [Data] = []

    init(state: Data?) { stateData = state }

    func state() async -> Data? { stateData }
    func command(_ json: Data) async throws { lock.withLock { commands.append(json) } }
    func stateUpdates() -> AsyncStream<Data> { AsyncStream { $0.finish() } }
}
