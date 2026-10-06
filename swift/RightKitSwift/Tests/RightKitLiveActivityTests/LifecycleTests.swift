import Testing

@testable import RightKitLiveActivity

private enum Phase: Hashable, Sendable { case idle, recording, processing, ready, failed }

private let rules = ActivityLifecycleRules<Phase>(
    idle: .idle, start: .recording, failure: .failed,
    terminal: [.ready, .failed], watchdog: [.recording, .processing],
    transitions: [.idle: [.recording], .recording: [.processing, .failed], .processing: [.ready, .failed], .ready: [.recording], .failed: [.recording]]
)

@Test func sessionBindingTerminalRestartWatchdogAndAdoption() {
    var machine = ActivityLifecycle(rules: rules)
    let began = machine.begin(sessionID: "a")
    let replaced = machine.begin(sessionID: "b")
    let skipped = machine.transition(to: .ready)
    let reaffirmed = machine.transition(to: .recording, sessionID: "a")
    let processing = machine.transition(to: .processing, sessionID: "a")
    #expect(began && !replaced, "an active activity cannot be replaced by begin")
    #expect(!skipped, "recording cannot skip processing")
    #expect(reaffirmed && processing, "coalesced reaffirmation is valid")

    let wrongWatchdog = machine.expireWatchdog(sessionID: "other")
    let watchdog = machine.expireWatchdog(sessionID: "a")
    #expect(!wrongWatchdog && watchdog && machine.phase == .failed && !machine.needsWatchdog)

    let restarted = machine.begin(sessionID: "b")
    let stale = machine.transition(to: .processing, sessionID: "a")
    #expect(restarted, "a terminal activity permits a new session")
    #expect(!stale, "an old session cannot mutate the new one")

    let adopted = machine.adopt(sessionID: "orphan", phase: .processing)
    let adoptedIdle = machine.adopt(sessionID: "x", phase: .idle)
    #expect(adopted && !adoptedIdle && machine.sessionID == "orphan")
    machine.reset()
    #expect(machine.phase == .idle && machine.sessionID == nil)
}

@Test func fenceInvalidatesTokensIssuedBeforeANewerOperation() {
    var fence = LifecycleFence()
    let old = fence.advance()
    let newer = fence.advance()
    #expect(!fence.isCurrent(old) && fence.isCurrent(newer))
}
