import Foundation

// Swift side of the proposed C ABI over `rightkit-jobs` (design: READINESS "LEGION CHOSEN Pulse
// Swift facade"). The ABI is a seam: a native binding implements `PulseNativeABI`; Rust owns
// allocation and release of every handle, Swift only holds opaque tokens. No engine-kit crate and
// no Pulse supervisor live here. Native linking and version qualification remain open.

public enum PulseCompat {
    /// ABI of this facade, contract version of `rightkit-jobs` (wire.rs CONTRACT_VERSION), and the
    /// Pulse schema this facade was written against.
    public static let abiVersion: UInt32 = 1
    public static let contractVersion: UInt32 = 1
    public static let schemaVersion: UInt32 = 1
}

/// Opaque handles; the integer is meaningful only to the native side.
public struct PulseHostHandle: Hashable, Sendable { public let raw: UInt64; public init(raw: UInt64) { self.raw = raw } }
public struct PulseConnectionHandle: Hashable, Sendable { public let raw: UInt64; public init(raw: UInt64) { self.raw = raw } }

public struct PulseVersions: Equatable, Sendable {
    public var abi: UInt32, contract: UInt32, schema: UInt32
    public init(abi: UInt32, contract: UInt32, schema: UInt32) { self.abi = abi; self.contract = contract; self.schema = schema }
}

public enum PulseLifecycle: String, Sendable, Equatable { case booting, ready, recovering, down }

public enum PulseShutdownOutcome: String, Sendable, Comparable {
    case noEngine, alreadyShuttingDown, exited, killed, unconfirmed
    private var rank: Int { [.noEngine: 0, .alreadyShuttingDown: 1, .exited: 2, .killed: 3, .unconfirmed: 4][self]! }
    public static func < (a: Self, b: Self) -> Bool { a.rank < b.rank }
}

public struct PulseShutdownReport: Sendable, Equatable {
    public var outcome: PulseShutdownOutcome
    public var unreaped: Int
    public init(outcome: PulseShutdownOutcome, unreaped: Int = 0) { self.outcome = outcome; self.unreaped = unreaped }
    /// Only a reaped, self-exited (or absent) engine is clean; `unconfirmed` is never success.
    public var isClean: Bool { unreaped == 0 && (outcome == .exited || outcome == .noEngine) }
}

/// Resume: the job survives window disconnect and may be re-attached. Never: it dies with its caller.
public enum PulseResumePolicy: String, Sendable { case resume, never }

public struct PulseHostConfig: Sendable {
    public var initialSnapshot: Data      // complete initialization snapshot, registered before start
    public var generation: UInt64
    public init(initialSnapshot: Data, generation: UInt64) { self.initialSnapshot = initialSnapshot; self.generation = generation }
}

public struct PulseSnapshot: Sendable, Equatable {
    public var generation: UInt64
    public var lifecycle: PulseLifecycle
    public var payload: Data
    public init(generation: UInt64, lifecycle: PulseLifecycle, payload: Data) {
        self.generation = generation; self.lifecycle = lifecycle; self.payload = payload
    }
}

public enum PulseError: Error, Equatable, Sendable {
    case incompatible(PulseVersions)
    case notReady
    case deadlineExceeded
    case cancelled
    case shuttingDown
    case native(code: Int, message: String)
}

/// The proposed C ABI as Swift sees it. Every call is blocking; the facade never invokes it on the
/// MainActor. `onEvent` bytes are borrowed for the callback only: the facade copies them.
public protocol PulseNativeABI: Sendable {
    func versions() -> PulseVersions
    func registerSnapshot(_ config: PulseHostConfig) throws
    func hostStart() throws -> PulseHostHandle
    func hostSnapshot(_ host: PulseHostHandle) throws -> PulseSnapshot
    func hostCall(_ host: PulseHostHandle, method: String, params: Data, resume: PulseResumePolicy,
                  deadline: Date, onEvent: @Sendable (UnsafeRawBufferPointer) -> Void) throws -> Data
    /// Blocks until the lifecycle changes from `since`; returns nil when the host is released.
    func hostLifecycle(_ host: PulseHostHandle, since: PulseLifecycle?, deadline: Date) throws -> PulseLifecycle?
    func hostShutdown(_ host: PulseHostHandle, budget: TimeInterval) throws -> PulseShutdownReport
    func serviceConnect(_ host: PulseHostHandle, deadline: Date) throws -> PulseConnectionHandle
    func serviceCall(_ conn: PulseConnectionHandle, method: String, params: Data, deadline: Date) throws -> Data
    /// Rust frees the allocation; releasing twice is a no-op on the native side.
    func release(host: PulseHostHandle)
    func release(connection: PulseConnectionHandle)
}
