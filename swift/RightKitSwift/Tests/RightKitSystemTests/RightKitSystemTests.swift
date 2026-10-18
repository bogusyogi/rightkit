import Foundation
import XCTest
@testable import RightKitSystem

#if os(macOS)
/// Pure logic and process-table reads only. Nothing here touches Notification Center, a login item
/// or a privacy grant, so it runs unattended.
final class RightKitSystemTests: XCTestCase {
    // MARK: Processes

    func testDockAppsFilterAndSort() {
        let apps = [
            RightKitRunningApp(pid: 1, name: "Zed", bundlePath: "/Applications/Zed.app"),
            RightKitRunningApp(pid: 2, name: "Agent", bundlePath: "/x/Agent.app", isRegular: false),
            RightKitRunningApp(pid: 3, name: "Alpha", bundlePath: "/Applications/Alpha.app"),
            RightKitRunningApp(pid: 4, name: "NoBundle", bundlePath: nil),
            RightKitRunningApp(pid: 5, name: "Me", bundlePath: "/Applications/Me.app"),
        ]
        XCTAssertEqual(RightKitProcesses.dockApps(from: apps, excluding: 5).map(\.name), ["Alpha", "Zed"])
    }

    func testSameProcessToleranceWindow() {
        let start = Date(timeIntervalSince1970: 1_000_000)
        XCTAssertTrue(RightKitProcesses.isSameProcess(actualStart: start, expectedStart: start.addingTimeInterval(120), tolerance: 300))
        XCTAssertFalse(RightKitProcesses.isSameProcess(actualStart: start, expectedStart: start.addingTimeInterval(600), tolerance: 300))
    }

    func testOwnProcessIsAliveAndHasAStartTime() throws {
        let pid = ProcessInfo.processInfo.processIdentifier
        XCTAssertTrue(RightKitProcesses.exists(pid: pid))
        let start = try XCTUnwrap(RightKitProcesses.startTime(pid: pid))
        XCTAssertLessThan(abs(start.timeIntervalSinceNow), 60 * 60 * 24 * 30)
        XCTAssertTrue(RightKitProcesses.isAlive(pid: pid, startedAt: start))
        XCTAssertFalse(RightKitProcesses.isAlive(pid: pid, startedAt: Date(timeIntervalSince1970: 0)))
        XCTAssertTrue(RightKitProcesses.isAlive(pid: pid, startedAt: nil))
    }

    func testKernelTableFindsThisProcessByExecutableSuffix() {
        let name = URL(fileURLWithPath: CommandLine.arguments[0]).lastPathComponent
        let me = ProcessInfo.processInfo.processIdentifier
        // The executable name is a suffix of the path of the process running this check, if readable.
        let found = RightKitProcesses.processes(withExecutableSuffix: name)
        if let own = found.first(where: { $0.pid == me }) {
            XCTAssertEqual(own.parentPID, getppid())
        }
    }

    // MARK: Bounded process

    func testBoundedRunCapturesOutputAndStatus() {
        let result = RightKitBoundedProcess.run(
            executable: URL(fileURLWithPath: "/bin/sh"), arguments: ["-c", "echo out; echo err 1>&2; exit 3"], timeout: 10)
        XCTAssertEqual(result.stdout, "out")
        XCTAssertEqual(result.stderr, "err")
        XCTAssertEqual(result.status, 3)
        XCTAssertFalse(result.timedOut)
        XCTAssertFalse(result.succeeded)
    }

    func testBoundedRunCapsOutput() {
        let result = RightKitBoundedProcess.run(
            executable: URL(fileURLWithPath: "/bin/sh"), arguments: ["-c", "head -c 100000 /dev/zero | tr '\\0' x"], timeout: 10, outputCap: 1000)
        XCTAssertEqual(result.stdout.count, 1000)
        XCTAssertEqual(result.status, 0)
    }

    func testBoundedRunStopsATimedOutChild() {
        let started = Date()
        let result = RightKitBoundedProcess.run(
            executable: URL(fileURLWithPath: "/bin/sleep"), arguments: ["30"], timeout: 0.5, drainWait: 0.5)
        XCTAssertTrue(result.timedOut)
        XCTAssertLessThan(Date().timeIntervalSince(started), 10)
    }

    func testBoundedRunReportsLaunchFailure() {
        let result = RightKitBoundedProcess.run(executable: URL(fileURLWithPath: "/nonexistent/tool"))
        XCTAssertNotNil(result.launchError)
        XCTAssertEqual(result.status, -1)
    }

    // MARK: Child log

    private func scratch() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("rksys-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    func testRotationShiftsGenerationsAndDropsTheOldest() throws {
        let dir = try scratch()
        let file = dir.appendingPathComponent("child.log")
        try Data("gen0".utf8).write(to: file)
        try Data("gen1".utf8).write(to: file.appendingPathExtension("1"))
        try Data("gen2".utf8).write(to: file.appendingPathExtension("2"))
        XCTAssertFalse(RightKitChildLog.rotateIfNeeded(file, maxBytes: 100, generations: 2))
        XCTAssertTrue(RightKitChildLog.rotateIfNeeded(file, maxBytes: 4, generations: 2))
        XCTAssertFalse(FileManager.default.fileExists(atPath: file.path))
        XCTAssertEqual(try String(contentsOf: file.appendingPathExtension("1")), "gen0")
        XCTAssertEqual(try String(contentsOf: file.appendingPathExtension("2")), "gen1")
    }

    func testOpenForAppendCreatesPrivateFileAndAppends() throws {
        let dir = try scratch()
        let file = dir.appendingPathComponent("logs/child.log")
        let first = try XCTUnwrap(RightKitChildLog.openForAppend(file))
        try first.write(contentsOf: Data("a".utf8)); try first.close()
        let second = try XCTUnwrap(RightKitChildLog.openForAppend(file))
        try second.write(contentsOf: Data("b".utf8)); try second.close()
        XCTAssertEqual(try String(contentsOf: file), "ab")
        let attributes = try FileManager.default.attributesOfItem(atPath: file.path)
        XCTAssertEqual((attributes[.posixPermissions] as? Int) ?? 0, 0o600)
    }

    func testRecordLineIsSortedJSONWithTimestamp() throws {
        let data = try XCTUnwrap(RightKitChildLog.recordLine("x.spawned", fields: ["pid": 7, "a": 1], nowMilliseconds: 42))
        XCTAssertEqual(String(decoding: data, as: UTF8.self), "{\"a\":1,\"event\":\"x.spawned\",\"pid\":7,\"ts_ms\":42}\n")
    }

    // MARK: Restart policy

    func testBackoffThenLatch() {
        let policy = RightKitRestartPolicy()
        var state = RightKitRestartState()
        let t0 = Date(timeIntervalSince1970: 1_000)
        XCTAssertTrue(state.canLaunch(now: t0))
        XCTAssertEqual(state.recordExit(now: t0, status: 1, policy: policy), .respawnScheduled(afterSeconds: 1))
        XCTAssertFalse(state.canLaunch(now: t0.addingTimeInterval(0.5)))
        XCTAssertTrue(state.canLaunch(now: t0.addingTimeInterval(1)))
        XCTAssertEqual(state.recordExit(now: t0.addingTimeInterval(2), status: 1, policy: policy), .respawnScheduled(afterSeconds: 2))
        XCTAssertEqual(state.recordExit(now: t0.addingTimeInterval(5), status: 1, policy: policy), .respawnScheduled(afterSeconds: 4))
        XCTAssertEqual(state.recordExit(now: t0.addingTimeInterval(10), status: 1, policy: policy), .respawnScheduled(afterSeconds: 8))
        XCTAssertEqual(
            state.recordExit(now: t0.addingTimeInterval(20), status: 9, policy: policy),
            .latched(reason: "exited 5 times in 60 s (last status 9)"))
        XCTAssertFalse(state.canLaunch(now: t0.addingTimeInterval(1000)))
        XCTAssertEqual(state.recordExit(now: t0.addingTimeInterval(21), status: 1, policy: policy), .alreadyLatched)
        state.retry()
        XCTAssertTrue(state.canLaunch(now: t0))
        XCTAssertTrue(state.exits.isEmpty)
    }

    func testExitsOutsideTheWindowDoNotCountAndBackoffResets() {
        let policy = RightKitRestartPolicy()
        var state = RightKitRestartState()
        let t0 = Date(timeIntervalSince1970: 1_000)
        _ = state.recordExit(now: t0, status: 1, policy: policy)
        _ = state.recordExit(now: t0.addingTimeInterval(1), status: 1, policy: policy)
        XCTAssertEqual(state.recordExit(now: t0.addingTimeInterval(500), status: 1, policy: policy), .respawnScheduled(afterSeconds: 1))
        XCTAssertEqual(state.exits.count, 1)
    }

    func testBackoffRepeatsItsLastDelay() {
        let policy = RightKitRestartPolicy(backoff: [1, 3], latchExits: 100, window: 1000)
        var state = RightKitRestartState()
        var last: RightKitRestartState.Outcome?
        for index in 0..<5 { last = state.recordExit(now: Date(timeIntervalSince1970: Double(index)), status: 1, policy: policy) }
        XCTAssertEqual(last, .respawnScheduled(afterSeconds: 3))
    }

    func testStatusText() {
        XCTAssertEqual(RightKitSupervisedChild.Status.latched("why").text, "latched: why")
        XCTAssertEqual(RightKitSupervisedChild.Status.notInstalled.text, "not installed")
    }

    // MARK: Banners

    func testBannerIdentifierIsPerThreadAndSecond() {
        let banner = RightKitBanner(title: "t", body: "b", thread: "session|x", identifierPrefix: "channel")
        XCTAssertEqual(banner.identifier(now: Date(timeIntervalSince1970: 1234.9)), "channel|session|x|1234")
        XCTAssertEqual(RightKitBanner(title: "t", body: "b").identifier(now: Date(timeIntervalSince1970: 5)), "banner||5")
    }

    func testTestActionPerAuthorization() {
        XCTAssertEqual(RightKitBannerTestAction.action(for: .notDetermined), .requestThenPost)
        XCTAssertEqual(RightKitBannerTestAction.action(for: .denied), .openSettings)
        XCTAssertEqual(RightKitBannerTestAction.action(for: .authorized), .post)
        XCTAssertEqual(RightKitBannerTestAction.action(for: .provisional), .post)
        XCTAssertEqual(RightKitBannerTestAction.action(for: .unavailable), .unavailable)
        XCTAssertTrue(RightKitNotificationAuthorization.provisional.canDeliver)
        XCTAssertFalse(RightKitNotificationAuthorization.denied.canDeliver)
    }

    func testSettingsURLCarriesTheBundleIdentifier() {
        XCTAssertEqual(
            RightKitBanners.notificationSettingsURL(bundleIdentifier: "dev.example.app")?.absoluteString,
            "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=dev.example.app")
    }

    // MARK: Threshold tracker

    func testFirstReadingOnlyRecords() {
        var tracker = RightKitThresholdTracker()
        XCTAssertEqual(tracker.observe(id: "a", percent: 91), [])
        XCTAssertEqual(tracker.observe(id: "a", percent: 95), [])
        XCTAssertEqual(tracker.observe(id: "a", percent: 100), [100])
    }

    func testJumpCrossesEveryThresholdOnceAndResetsBelowTheFirst() {
        var tracker = RightKitThresholdTracker()
        XCTAssertEqual(tracker.observe(id: "a", percent: 10), [])
        XCTAssertEqual(tracker.observe(id: "a", percent: 100), [80, 100])
        XCTAssertEqual(tracker.observe(id: "a", percent: 100), [])
        XCTAssertEqual(tracker.observe(id: "a", percent: 5), [])
        XCTAssertEqual(tracker.observe(id: "a", percent: 85), [80])
    }

    func testStaleReadingForgetsAndMutedStillUpdates() {
        var tracker = RightKitThresholdTracker()
        _ = tracker.observe(id: "a", percent: 10)
        XCTAssertEqual(tracker.observe(id: "a", percent: 90, isStale: true), [])
        XCTAssertEqual(tracker.observe(id: "a", percent: 10), [])
        XCTAssertEqual(tracker.observe(id: "a", percent: 90, muted: true), [])
        XCTAssertEqual(tracker.observe(id: "a", percent: 90), [])
        XCTAssertEqual(tracker.observe(id: "a", percent: nil), [])
    }

    func testIdsAreIndependent() {
        var tracker = RightKitThresholdTracker()
        _ = tracker.observe(id: "a", percent: 10)
        XCTAssertEqual(tracker.observe(id: "b", percent: 90), [])
        XCTAssertEqual(tracker.observe(id: "a", percent: 90), [80])
    }

    // MARK: Login item

    func testRegisterOutcomeTrustsStatusOverTheThrow() {
        XCTAssertEqual(
            RightKitLoginItemOutcome.afterRegister(threw: true, status: .requiresApproval),
            RightKitLoginItemOutcome(reportsError: false, opensSettings: true, isEnabled: false))
        XCTAssertEqual(
            RightKitLoginItemOutcome.afterRegister(threw: true, status: .enabled),
            RightKitLoginItemOutcome(reportsError: false, opensSettings: false, isEnabled: true))
        XCTAssertTrue(RightKitLoginItemOutcome.afterRegister(threw: true, status: .notFound).reportsError)
        XCTAssertFalse(RightKitLoginItemOutcome.afterRegister(threw: false, status: .notRegistered).reportsError)
    }

    func testInstallPathComparisonResolvesSymlinks() throws {
        let dir = try scratch()
        let real = dir.appendingPathComponent("Real.app")
        try FileManager.default.createDirectory(at: real, withIntermediateDirectories: true)
        let link = dir.appendingPathComponent("Link.app")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: real)
        XCTAssertTrue(RightKitLoginItem.isInstalled(bundleURL: link, at: real.path))
        XCTAssertFalse(RightKitLoginItem.isInstalled(bundleURL: real, at: dir.appendingPathComponent("Other.app").path))
        XCTAssertTrue(RightKitLoginItem(requiredInstallPath: nil).isAtRequiredInstallPath)
        XCTAssertThrowsError(try RightKitLoginItem(requiredInstallPath: "/Applications/Nope.app").setEnabled(true))
    }

    // MARK: Permissions

    func testClassifyErrno() {
        XCTAssertEqual(RightKitPermissionProbes.classify(errno: EACCES), .needsApproval)
        XCTAssertEqual(RightKitPermissionProbes.classify(errno: EPERM), .needsApproval)
        XCTAssertNil(RightKitPermissionProbes.classify(errno: ENOENT))
    }

    func testFullDiskAccessOverAnInjectedHome() throws {
        let home = try scratch()
        XCTAssertEqual(RightKitPermissionProbes.fullDiskAccess(home: home, probePaths: ["missing"]), .unknown)
        try Data("x".utf8).write(to: home.appendingPathComponent("probe"))
        XCTAssertEqual(RightKitPermissionProbes.fullDiskAccess(home: home, probePaths: ["missing", "probe"]), .granted)
    }

    func testParsePluginkitOutput() {
        let id = "dev.example.finder"
        XCTAssertEqual(RightKitPermissionProbes.parseExtensionStatus(pluginkitOutput: "+    \(id)(1.0)\n", identifier: id), .granted)
        XCTAssertEqual(RightKitPermissionProbes.parseExtensionStatus(pluginkitOutput: "-    \(id)(1.0)\n", identifier: id), .off)
        XCTAssertEqual(RightKitPermissionProbes.parseExtensionStatus(pluginkitOutput: "", identifier: id), .unknown)
        XCTAssertEqual(RightKitPermissionProbes.parseExtensionStatus(pluginkitOutput: "+    other.ext(1)\n", identifier: id), .unknown)
    }

    func testPrivacyPaneURLsTryModernFirst() {
        let urls = RightKitPrivacyPane.accessibility.urls.map(\.absoluteString)
        XCTAssertEqual(urls.count, 2)
        XCTAssertTrue(urls[0].contains("PrivacySecurity.extension?Privacy_Accessibility"))
        XCTAssertTrue(urls[1].hasSuffix("preference.security?Privacy_Accessibility"))
    }

    func testLoginStatusMapsToPermissionStatus() {
        XCTAssertEqual(RightKitPermissionProbes.status(of: .enabled), .granted)
        XCTAssertEqual(RightKitPermissionProbes.status(of: .requiresApproval), .needsApproval)
        XCTAssertEqual(RightKitPermissionProbes.status(of: .notRegistered), .off)
        XCTAssertEqual(RightKitPermissionProbes.status(of: .notFound), .unknown)
    }

    // MARK: Runtime guard

    func testUnderTestDecision() {
        XCTAssertFalse(RightKitRuntime.evaluate(environment: [:]))
        XCTAssertTrue(RightKitRuntime.evaluate(environment: ["XCTestBundlePath": "/x"]))
        XCTAssertTrue(RightKitRuntime.evaluate(environment: [:], hasXCTestCaseClass: true))
        XCTAssertFalse(RightKitRuntime.evaluate(environment: ["APP_RENDER": "0"], extraFlags: ["APP_RENDER"]))
        XCTAssertTrue(RightKitRuntime.evaluate(environment: ["APP_RENDER": "1"], extraFlags: ["APP_RENDER"]))
    }

    func testThisProcessIsUnderTestAndRefusesRealData() {
        XCTAssertTrue(RightKitRuntime.isUnderTest)
        XCTAssertThrowsError(try RightKitRuntime.requireNotUnderTest("the real home")) {
            XCTAssertEqual($0 as? RightKitRuntimeError, .refusedUnderTest("the real home"))
        }
        XCTAssertTrue(FileManager.default.fileExists(atPath: RightKitRuntime.testScratch.path))
    }
}
#endif
