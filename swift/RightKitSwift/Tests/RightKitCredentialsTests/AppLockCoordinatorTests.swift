import Foundation
import Testing
#if canImport(LocalAuthentication)
import LocalAuthentication
#endif
@testable import RightKitCredentials

// PK16-2 (Pulse iOS AppLock): the lock state machine and the decision for each owner-check result.
// A scripted authenticator and a fixed clock stand in for the system prompt: no prompt is shown.

private final class ScriptedAuthenticator: OwnerAuthenticating, @unchecked Sendable {
    private let lock = NSLock()
    private var results: [OwnerAuthenticationResult]
    private var _calls: [String] = []
    private var holding = false
    private var waiter: CheckedContinuation<Void, Never>?
    private var _arrived = false

    init(_ results: [OwnerAuthenticationResult], hold: Bool = false) {
        self.results = results
        self.holding = hold
    }

    var calls: [String] { lock.withLock { _calls } }
    var arrived: Bool { lock.withLock { _arrived } }

    func authenticate(reason: String) async -> OwnerAuthenticationResult {
        lock.withLock { _calls.append(reason); _arrived = true }
        if lock.withLock({ holding }) {
            await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
                let resumeNow = lock.withLock { () -> Bool in
                    if !holding { return true }
                    waiter = continuation
                    return false
                }
                if resumeNow { continuation.resume() }
            }
        }
        return lock.withLock { results.count > 1 ? results.removeFirst() : results[0] }
    }

    func release() {
        let pending = lock.withLock { () -> CheckedContinuation<Void, Never>? in
            holding = false
            let pending = waiter
            waiter = nil
            return pending
        }
        pending?.resume()
    }
}

private final class FixedClock: @unchecked Sendable {
    private let lock = NSLock()
    private var _time = Date(timeIntervalSince1970: 1_000_000)
    var time: Date {
        get { lock.withLock { _time } }
        set { lock.withLock { _time = newValue } }
    }
    func advance(_ seconds: TimeInterval) { time = time.addingTimeInterval(seconds) }
}

@MainActor
private func coordinator(
    enabled: Bool = true, startsLocked: Bool? = nil, auth: ScriptedAuthenticator, clock: FixedClock = FixedClock()
) -> AppLockCoordinator {
    AppLockCoordinator(
        isEnabled: enabled, startsLocked: startsLocked, unlockReason: "Unlock Example",
        authenticator: auth, now: { clock.time }
    )
}

@MainActor
@Test func theLockStartsLockedOnlyWhenEnabled() {
    let auth = ScriptedAuthenticator([.verified])
    #expect(coordinator(enabled: true, auth: auth).isLocked)
    #expect(!coordinator(enabled: false, auth: auth).isLocked)
    #expect(!coordinator(enabled: true, startsLocked: false, auth: auth).isLocked, "a demo or test can start unlocked")
    #expect(coordinator(enabled: false, startsLocked: true, auth: auth).isLocked)
}

@MainActor
@Test func comingBackAfterTheGracePeriodLocksAndAsksAtOnce() async {
    let clock = FixedClock()
    let lock = coordinator(auth: ScriptedAuthenticator([.verified]), clock: clock)
    await lock.unlock()
    #expect(!lock.isLocked)

    lock.didEnterBackground()
    clock.advance(29)
    #expect(!lock.didBecomeActive(), "inside the 30 s grace the app stays open")
    #expect(!lock.isLocked)

    lock.didEnterBackground()
    clock.advance(30)
    #expect(lock.didBecomeActive(), "at the grace the app locks and reports that it should ask")
    #expect(lock.isLocked)
    #expect(lock.failure == nil)

    #expect(!lock.didBecomeActive(), "the clock is consumed by one return")
}

@MainActor
@Test func aDisabledOrAlreadyLockedAppNeverStartsTheClock() async {
    let clock = FixedClock()
    let disabled = coordinator(enabled: false, auth: ScriptedAuthenticator([.verified]), clock: clock)
    disabled.didEnterBackground()
    clock.advance(600)
    #expect(!disabled.didBecomeActive())

    let locked = coordinator(auth: ScriptedAuthenticator([.verified]), clock: clock)
    locked.didEnterBackground()
    clock.advance(600)
    #expect(!locked.didBecomeActive(), "already locked: nothing to report")

    let neverBackgrounded = coordinator(startsLocked: false, auth: ScriptedAuthenticator([.verified]), clock: clock)
    #expect(!neverBackgrounded.didBecomeActive())
}

@MainActor
@Test func backgroundingWhileThePromptIsUpStartsNoClock() async throws {
    let clock = FixedClock()
    let auth = ScriptedAuthenticator([.verified], hold: true)
    let lock = coordinator(auth: auth, clock: clock)
    let unlocking = Task { @MainActor in await lock.unlock() }
    while !auth.arrived || !lock.isAsking { try await Task.sleep(nanoseconds: 2_000_000) }
    lock.didEnterBackground() // the system sheet made the scene inactive
    auth.release()
    await unlocking.value
    #expect(!lock.isLocked)
    clock.advance(600)
    #expect(!lock.didBecomeActive())
}

@MainActor
@Test func eachOwnerCheckResultHasOneDecision() async {
    func run(_ result: OwnerAuthenticationResult) async -> AppLockCoordinator {
        let lock = coordinator(auth: ScriptedAuthenticator([result]))
        await lock.unlock()
        return lock
    }
    let verified = await run(.verified)
    #expect(!verified.isLocked && verified.failure == nil)
    let noPasscode = await run(.noPasscodeSet)
    #expect(!noPasscode.isLocked, "a device with no passcode has nothing to unlock with")
    let cancelled = await run(.cancelled)
    #expect(cancelled.isLocked && cancelled.failure == nil, "a dismissed prompt is not a failure")
    let failed = await run(.failed)
    #expect(failed.isLocked && failed.failure == .couldNotVerify)
    let unavailable = await run(.unavailable("no biometry"))
    #expect(unavailable.isLocked && unavailable.failure == .unavailable, "an unknown state is never a pass")
}

@MainActor
@Test func aLaterSuccessClearsTheFailure() async {
    let auth = ScriptedAuthenticator([.failed, .verified])
    let lock = coordinator(auth: auth)
    await lock.unlock()
    #expect(lock.isLocked && lock.failure == .couldNotVerify)
    await lock.unlock()
    #expect(!lock.isLocked && lock.failure == nil)
    #expect(auth.calls == ["Unlock Example", "Unlock Example"])
    #expect(!lock.isAsking)
}

@MainActor
@Test func aSecondUnlockWhilePromptingIsIgnoredAndADisabledLockNeverAsks() async throws {
    let held = ScriptedAuthenticator([.verified], hold: true)
    let lock = coordinator(auth: held)
    let first = Task { @MainActor in await lock.unlock() }
    while !held.arrived || !lock.isAsking { try await Task.sleep(nanoseconds: 2_000_000) }
    await lock.unlock() // returns at once: a prompt is already up
    #expect(held.calls.count == 1)
    held.release()
    await first.value
    #expect(!lock.isLocked)

    let off = ScriptedAuthenticator([.failed])
    let disabled = coordinator(enabled: false, startsLocked: true, auth: off)
    await disabled.unlock()
    #expect(!disabled.isLocked)
    #expect(off.calls.isEmpty, "a disabled lock opens without a prompt")
}

@MainActor
@Test func confirmOwnerPassesOnlyForAVerifiedOwnerOrADeviceWithNoPasscode() async {
    for (result, expected) in [
        (OwnerAuthenticationResult.verified, true), (.noPasscodeSet, true), (.cancelled, false), (.failed, false), (.unavailable("x"), false),
    ] {
        let auth = ScriptedAuthenticator([result])
        let lock = coordinator(auth: auth)
        #expect(await lock.confirmOwner(reason: "Turn off the lock") == expected)
        #expect(auth.calls == ["Turn off the lock"])
        #expect(!lock.isAsking)
        #expect(lock.isLocked, "confirming never changes the lock state itself")
    }
}

@MainActor
@Test func turningTheLockOffUnlocks() {
    let lock = coordinator(auth: ScriptedAuthenticator([.verified]))
    #expect(lock.isLocked)
    lock.setEnabled(false)
    #expect(!lock.isEnabled && !lock.isLocked)
    lock.setEnabled(true)
    #expect(lock.isEnabled && !lock.isLocked, "turning it back on does not lock until the next return")
}

@Test func biometryNamesAreApplesOwn() {
    #expect(BiometryKind.faceID.systemName == "Face ID")
    #expect(BiometryKind.touchID.systemName == "Touch ID")
    #expect(BiometryKind.opticID.systemName == "Optic ID")
    #expect(BiometryKind.none.systemName == "Passcode")
}

#if canImport(LocalAuthentication)
@Test func systemErrorsMapToTypedResults() {
    #expect(SystemOwnerAuthentication.result(for: LAError(.userCancel)) == .cancelled)
    #expect(SystemOwnerAuthentication.result(for: LAError(.appCancel)) == .cancelled)
    #expect(SystemOwnerAuthentication.result(for: LAError(.systemCancel)) == .cancelled)
    #expect(SystemOwnerAuthentication.result(for: LAError(.passcodeNotSet)) == .noPasscodeSet)
    #expect(SystemOwnerAuthentication.result(for: LAError(.authenticationFailed)) == .failed)
    #expect(SystemOwnerAuthentication.result(for: NSError(domain: "other", code: 1)) == .failed, "anything unknown is a failure, never a pass")

    let noPasscode = NSError(domain: LAError.errorDomain, code: LAError.Code.passcodeNotSet.rawValue)
    #expect(SystemOwnerAuthentication.result(forUnavailable: noPasscode) == .noPasscodeSet)
    let other = NSError(domain: LAError.errorDomain, code: LAError.Code.biometryNotAvailable.rawValue, userInfo: [NSLocalizedDescriptionKey: "no biometry"])
    #expect(SystemOwnerAuthentication.result(forUnavailable: other) == .unavailable("no biometry"))
    #expect(SystemOwnerAuthentication.result(forUnavailable: nil) == .unavailable("owner authentication is not available"))
    // The same code in another domain is not a missing passcode.
    #expect(SystemOwnerAuthentication.result(forUnavailable: NSError(domain: "other", code: LAError.Code.passcodeNotSet.rawValue)) != .noPasscodeSet)
}

/// PK16-1: `UserVerification` (Touch ID / Face ID / password) never throws and never passes on an error.
@Test func userVerificationMapsEverySystemEndToATypedOutcome() {
    #expect(UserVerification.outcome(for: LAError(.userCancel)) == .cancelled)
    #expect(UserVerification.outcome(for: LAError(.appCancel)) == .cancelled)
    #expect(UserVerification.outcome(for: LAError(.systemCancel)) == .cancelled)
    #expect(UserVerification.outcome(for: LAError(.authenticationFailed)) == .failed)
    #expect(UserVerification.outcome(for: NSError(domain: "other", code: 7)) == .failed, "a foreign error is a failure, never a pass")
    if case .unavailable = UserVerification.outcome(for: LAError(.passcodeNotSet)) {} else { Issue.record("a missing passcode is unavailable") }
    #if !os(watchOS)
    if case .unavailable = UserVerification.outcome(for: LAError(.biometryNotAvailable)) {} else { Issue.record("no biometry hardware is unavailable") }
    if case .unavailable = UserVerification.outcome(for: LAError(.biometryNotEnrolled)) {} else { Issue.record("no enrolled biometry is unavailable") }
    #endif
}
#endif
