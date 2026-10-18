import CRightKitEngine
import Foundation

/// The blocking calls into `rightkit-engine-native` (the C ABI of `rightkit-job-queue`'s `ffi`
/// feature, `ABI_VERSION` 1), one function per ABI entry point this package uses, with every
/// out-buffer freed through `rkp_buf_free` and every native status turned into an `EngineError`.
///
/// Every function here BLOCKS and must run on a thread of its own, never on the main thread or
/// the cooperative pool: `SupervisedEngine` runs them through `runOnThread`.
enum Native {
    // MARK: Errors

    /// The native status plus its message as an `EngineError`. `RKP_NATIVE` carries the engine's
    /// own `{code, message, retryable}` frame; the other statuses get stable codes.
    static func engineError(_ status: Int32, _ message: String, doing: String) -> EngineError {
        let text = message.isEmpty ? "\(doing) failed (native status \(status))" : message
        switch status {
        case RKP_NATIVE:
            if let data = message.data(using: .utf8),
               let engine = try? JSONDecoder().decode(EngineError.self, from: data) {
                return engine
            }
            return EngineError(code: "native_error", message: text, retryable: false)
        case RKP_INVALID_ARGUMENT:
            return EngineError(code: "invalid_argument", message: text, retryable: false)
        case RKP_NOT_READY:
            return EngineError(code: "not_ready", message: text, retryable: true)
        case RKP_DEADLINE:
            // The work may still be running (resume policy), so repeating it blindly could run
            // it twice: not retryable.
            return EngineError(code: "timeout", message: text, retryable: false)
        case RKP_SHUTTING_DOWN:
            return EngineError(code: "shutting_down", message: text, retryable: false)
        case RKP_INCOMPATIBLE:
            return EngineError(code: "incompatible", message: text, retryable: false)
        case RKP_STALE_HANDLE:
            return EngineError(code: "engine_stopped", message: text, retryable: false)
        case RKP_PANIC:
            return EngineError(code: "native_panic", message: text, retryable: false)
        case RKP_NOT_CONFIGURED:
            return EngineError(code: "not_configured", message: text, retryable: false)
        case RKP_STALE_GENERATION:
            return EngineError(code: "stale_generation", message: text, retryable: false)
        case RKP_TOO_MANY_HOSTS, RKP_TOO_MANY_CONNECTIONS:
            return EngineError(code: "too_many_engines", message: text, retryable: true)
        default:
            return EngineError(code: "native_error", message: text, retryable: false)
        }
    }

    private static func emptyError() -> RkpError {
        RkpError(code: 0, message: RkpBuf(ptr: nil, len: 0))
    }

    /// The error message text, then frees its buffer.
    private static func takeMessage(_ error: inout RkpError) -> String {
        defer { rkp_buf_free(&error.message) }
        guard let bytes = error.message.ptr, error.message.len > 0 else { return "" }
        return String(decoding: UnsafeBufferPointer(start: bytes, count: error.message.len), as: UTF8.self)
    }

    /// The result bytes as `Data` (copied), then frees the buffer.
    private static func takeData(_ buffer: inout RkpBuf) -> Data {
        defer { rkp_buf_free(&buffer) }
        guard let bytes = buffer.ptr, buffer.len > 0 else { return Data() }
        return Data(bytes: bytes, count: buffer.len)
    }

    private static func nativeFailure(_ status: Int32, _ error: inout RkpError, doing: String) -> EngineError {
        engineError(status, takeMessage(&error), doing: doing)
    }

    // MARK: Start

    /// Refuses a native library whose ABI is not the one this package's header was written for.
    static func checkVersions() throws {
        var versions = RkpVersions(abi: 0, contract: 0, schema: 0)
        let status = rkp_versions(&versions)
        guard status == RKP_OK else {
            throw engineError(status, "", doing: "rkp_versions")
        }
        guard versions.abi == RKP_ABI_VERSION else {
            throw EngineError(
                code: "incompatible_abi",
                message: "the native engine library reports ABI \(versions.abi); this package speaks ABI \(RKP_ABI_VERSION)",
                retryable: false
            )
        }
    }

    /// Installs the engine command (`rke_install_engine`).
    static func installEngine(
        path: String,
        arguments: [String],
        environmentKeys: [String],
        snapshotMethod: String,
        startBudgetMilliseconds: UInt32
    ) throws {
        var error = emptyError()
        let status: Int32 = withCStringArray(arguments) { argv in
            withCStringArray(environmentKeys) { envp in
                path.withCString { pathPointer in
                    snapshotMethod.withCString { methodPointer in
                        rke_install_engine(
                            pathPointer,
                            argv.baseAddress, argv.count,
                            envp.baseAddress, envp.count,
                            methodPointer,
                            startBudgetMilliseconds,
                            &error
                        )
                    }
                }
            }
        }
        guard status == RKP_OK else {
            throw nativeFailure(status, &error, doing: "rke_install_engine")
        }
    }

    static func registerSnapshot(_ snapshot: Data, generation: UInt64) throws {
        var error = emptyError()
        let status = snapshot.withUnsafeBytes { raw in
            rkp_register_snapshot(raw.baseAddress?.assumingMemoryBound(to: UInt8.self), raw.count, generation, &error)
        }
        guard status == RKP_OK else {
            throw nativeFailure(status, &error, doing: "rkp_register_snapshot")
        }
    }

    /// Starts the host and returns its handle once the engine is `Ready`.
    static func startHost() throws -> UInt64 {
        var host: UInt64 = 0
        var error = emptyError()
        let status = rkp_host_start(&host, &error)
        guard status == RKP_OK else {
            let engine = nativeFailure(status, &error, doing: "rkp_host_start")
            if status == RKP_DEADLINE {
                throw EngineError(code: "start_timeout", message: engine.message, retryable: true)
            }
            throw engine
        }
        return host
    }

    // MARK: Call

    /// The event callback handed to `rkp_host_call`. It runs on the native call thread, with bytes
    /// borrowed for this invocation only, so it copies them and returns at once: no lock wait, no
    /// user code, no re-entry into the ABI (ffi.rs forbids all three). `ctx` is an unretained
    /// `EventPump` that the calling thread keeps alive until `rkp_host_call` returns.
    static let eventCallback: RkpEventFn = { context, bytes, length in
        guard let context, let bytes else { return }
        let pump = Unmanaged<EventPump>.fromOpaque(context).takeUnretainedValue()
        pump.push(Data(bytes: bytes, count: length))
    }

    static func call(
        host: UInt64,
        method: String,
        params: Data,
        resume: EngineResumePolicy,
        timeoutMilliseconds: UInt64,
        pump: EventPump?
    ) -> Result<Data, EngineError> {
        var output = RkpBuf(ptr: nil, len: 0)
        var error = emptyError()
        let methodBytes = Array(method.utf8)
        // The pump is retained for exactly as long as the native call may run its callback.
        let context = pump.map { Unmanaged.passRetained($0) }
        defer { context?.release() }
        let status = methodBytes.withUnsafeBufferPointer { methodBuffer in
            params.withUnsafeBytes { paramsBuffer in
                rkp_host_call(
                    host,
                    methodBuffer.baseAddress, methodBuffer.count,
                    paramsBuffer.baseAddress?.assumingMemoryBound(to: UInt8.self), paramsBuffer.count,
                    resume == .never ? RKP_NEVER : RKP_RESUME,
                    timeoutMilliseconds,
                    pump == nil ? nil : eventCallback,
                    context?.toOpaque(),
                    &output,
                    &error
                )
            }
        }
        guard status == RKP_OK else {
            rkp_buf_free(&output)
            return .failure(nativeFailure(status, &error, doing: method))
        }
        _ = takeMessage(&error)
        return .success(takeData(&output))
    }

    // MARK: Lifecycle

    enum LifecyclePoll: Sendable {
        case changed(EngineLifecycle)
        /// The host handle is gone: released, so the engine is down for good.
        case released
        /// Nothing changed within the poll window.
        case unchanged
        case failed
    }

    static func lifecycleCode(_ lifecycle: EngineLifecycle) -> Int32 {
        switch lifecycle {
        case .booting: return Int32(RKP_BOOTING)
        case .ready: return Int32(RKP_READY)
        case .recovering: return Int32(RKP_RECOVERING)
        case .down: return Int32(RKP_DOWN)
        }
    }

    private static func lifecycle(code: Int32) -> EngineLifecycle? {
        switch code {
        case Int32(RKP_BOOTING): return .booting
        case Int32(RKP_READY): return .ready
        case Int32(RKP_RECOVERING): return .recovering
        case Int32(RKP_DOWN): return .down
        default: return nil
        }
    }

    /// Blocks up to `timeoutMilliseconds` for the lifecycle to differ from `since` (-1 = any).
    static func pollLifecycle(host: UInt64, since: Int32, timeoutMilliseconds: UInt64) -> LifecyclePoll {
        var code: Int32 = -1
        // No error buffer: a timeout is the normal outcome here and would allocate one each time.
        let status = rkp_host_lifecycle(host, since, timeoutMilliseconds, &code, nil)
        switch status {
        case RKP_OK:
            if code == -1 { return .released }
            return lifecycle(code: code).map { .changed($0) } ?? .failed
        case RKP_DEADLINE:
            return .unchanged
        default:
            return .failed
        }
    }

    // MARK: Shutdown and release

    static func shutdown(host: UInt64, budgetMilliseconds: UInt64) -> EngineShutdownReport {
        var outcome: UInt32 = 0
        var unreaped: UInt64 = 0
        var error = emptyError()
        let status = rkp_host_shutdown(host, budgetMilliseconds, &outcome, &unreaped, &error)
        guard status == RKP_OK else {
            _ = takeMessage(&error)
            // A host the library no longer knows has no engine; any other refusal is not a
            // confirmed stop, and an unconfirmed stop is never reported as success.
            if status == RKP_STALE_HANDLE { return EngineShutdownReport(outcome: .noEngine) }
            return EngineShutdownReport(outcome: .unconfirmed, unreaped: 1)
        }
        _ = takeMessage(&error)
        let mapped: EngineShutdownOutcome
        switch outcome {
        case RKP_SHUTDOWN_NO_ENGINE: mapped = .noEngine
        case RKP_SHUTDOWN_ALREADY: mapped = .alreadyShuttingDown
        case RKP_SHUTDOWN_EXITED: mapped = .exited
        case RKP_SHUTDOWN_KILLED: mapped = .killed
        default: mapped = .unconfirmed
        }
        return EngineShutdownReport(outcome: mapped, unreaped: Int(clamping: unreaped))
    }

    static func releaseHost(_ host: UInt64) {
        rkp_release_host(host)
    }

    // MARK: Helpers

    /// Calls `body` with a C `char *` array of `strings` (each duplicated, freed on return).
    private static func withCStringArray<R>(
        _ strings: [String],
        _ body: (UnsafeBufferPointer<UnsafePointer<CChar>?>) -> R
    ) -> R {
        var pointers: [UnsafePointer<CChar>?] = []
        pointers.reserveCapacity(strings.count)
        for string in strings {
            pointers.append(UnsafePointer(strdup(string)))
        }
        defer {
            for pointer in pointers {
                free(UnsafeMutablePointer(mutating: pointer))
            }
        }
        return pointers.withUnsafeBufferPointer(body)
    }
}
