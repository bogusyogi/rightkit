import Foundation
import Testing
@testable import RightKitPulse

final class FakeABI: PulseNativeABI, @unchecked Sendable {
    let lock = NSLock()
    var v = PulseVersions(abi: 1, contract: 1, schema: 1)
    var registered: PulseHostConfig?
    var order: [String] = []
    var snapGen: UInt64 = 7
    var callDelay: TimeInterval = 0
    var shutdownOutcome: PulseShutdownOutcome = .exited
    var releasedHosts = 0, releasedConns = 0, connects = 0
    var mainThreadCalls = 0

    func note(_ s: String) { lock.lock(); order.append(s); if Thread.isMainThread { mainThreadCalls += 1 }; lock.unlock() }
    func versions() -> PulseVersions { v }
    func registerSnapshot(_ c: PulseHostConfig) throws { note("register"); registered = c }
    func hostStart() throws -> PulseHostHandle { note("start"); return .init(raw: 1) }
    func hostSnapshot(_ h: PulseHostHandle) throws -> PulseSnapshot { .init(generation: snapGen, lifecycle: .ready, payload: Data()) }
    func hostCall(_ h: PulseHostHandle, method: String, params: Data, resume: PulseResumePolicy, deadline: Date,
                  onEvent: @Sendable (UnsafeRawBufferPointer) -> Void) throws -> Data {
        note("call")
        for i in 0..<3 { var b = UInt8(i); withUnsafeBytes(of: &b) { onEvent($0) } }
        if callDelay > 0 { Thread.sleep(forTimeInterval: callDelay) }
        return Data(method.utf8)
    }
    func hostLifecycle(_ h: PulseHostHandle, since: PulseLifecycle?, deadline: Date) throws -> PulseLifecycle? { .ready }
    func hostShutdown(_ h: PulseHostHandle, budget: TimeInterval) throws -> PulseShutdownReport {
        .init(outcome: shutdownOutcome, unreaped: shutdownOutcome == .unconfirmed ? 1 : 0)
    }
    func serviceConnect(_ h: PulseHostHandle, deadline: Date) throws -> PulseConnectionHandle { lock.lock(); connects += 1; lock.unlock(); return .init(raw: 9) }
    func serviceCall(_ c: PulseConnectionHandle, method: String, params: Data, deadline: Date) throws -> Data { Data("svc:\(method)".utf8) }
    func release(host: PulseHostHandle) { lock.lock(); releasedHosts += 1; lock.unlock() }
    func release(connection: PulseConnectionHandle) { lock.lock(); releasedConns += 1; lock.unlock() }
}

private let cfg = PulseHostConfig(initialSnapshot: Data("{}".utf8), generation: 7)

@Suite struct PulseHostTests {
    @Test func rejectsIncompatibleVersions() async {
        let abi = FakeABI(); abi.v = .init(abi: 2, contract: 1, schema: 1)
        await #expect(throws: PulseError.incompatible(abi.v)) { try await PulseHost.start(abi: abi, config: cfg) }
        #expect(abi.order.isEmpty)
    }

    @Test func registersSnapshotBeforeStartAndReconcilesGeneration() async throws {
        let abi = FakeABI()
        _ = try await PulseHost.start(abi: abi, config: cfg)
        #expect(abi.order == ["register", "start"])
        let stale = FakeABI(); stale.snapGen = 3
        await #expect(throws: PulseError.notReady) { try await PulseHost.start(abi: stale, config: cfg) }
    }

    @Test func callRunsOffMainAndCopiesEvents() async throws {
        let abi = FakeABI()
        let host = try await PulseHost.start(abi: abi, config: cfg)
        let r = try await host.call("job.run", params: Data(), resume: .resume, timeout: 5).result()
        #expect(String(decoding: r.payload, as: UTF8.self) == "job.run")
        #expect(r.events.count == 3 && r.droppedEvents == 0)
        #expect(abi.mainThreadCalls == 0)
    }

    @Test func cancelAndDeadlineDeliverExactlyOneTerminal() async throws {
        let abi = FakeABI(); abi.callDelay = 0.4
        let host = try await PulseHost.start(abi: abi, config: cfg)
        let c = try host.call("slow", params: Data(), resume: .resume, timeout: 5)
        c.cancel()
        await #expect(throws: PulseError.cancelled) { try await c.result() }
        let d = try host.call("slow", params: Data(), resume: .never, timeout: 0.05)
        await #expect(throws: PulseError.deadlineExceeded) { try await d.result() }
    }

    @Test func eventBufferIsBounded() {
        let b = PulseEventBuffer(maxEvents: 2, maxBytes: 4)
        for i in 0..<5 { var x = UInt8(i); withUnsafeBytes(of: &x) { b.append(copying: $0) } }
        #expect(b.drain().count == 2 && b.dropped == 3)
        let big = [UInt8](repeating: 0, count: 9)
        big.withUnsafeBytes { b.append(copying: $0) }
        #expect(b.drain().isEmpty && b.dropped == 4)
    }

    @Test func shutdownWaitsForQuiescenceThenReleasesOnce() async throws {
        let abi = FakeABI(); abi.callDelay = 0.2
        let host = try await PulseHost.start(abi: abi, config: cfg)
        _ = try await host.serviceCall("svc.ping", params: Data(), timeout: 5)
        let c = try host.call("slow", params: Data(), resume: .resume, timeout: 5)
        let rep = try await host.shutdown(budget: 3)
        #expect(rep.isClean)
        #expect(abi.releasedHosts == 1 && abi.releasedConns == 1)
        _ = try? await c.result()
        #expect(throws: PulseError.shuttingDown) { try host.call("x", params: Data(), resume: .never, timeout: 1) }
        #expect(try await host.shutdown(budget: 1).outcome == .alreadyShuttingDown)
        #expect(abi.releasedHosts == 1)
    }

    @Test func unconfirmedShutdownKeepsHandleAndIsNotClean() async throws {
        let abi = FakeABI(); abi.shutdownOutcome = .unconfirmed
        let host = try await PulseHost.start(abi: abi, config: cfg)
        let rep = try await host.shutdown(budget: 1)
        #expect(!rep.isClean && abi.releasedHosts == 0)
    }

    @Test func windowCloseKeepsHostAlive() async throws {
        let abi = FakeABI()
        let host = try await PulseHost.start(abi: abi, config: cfg)
        host.windowClosed()
        _ = try await host.call("job.status", params: Data(), resume: .resume, timeout: 5).result()
        #expect(abi.releasedHosts == 0)
    }
}
