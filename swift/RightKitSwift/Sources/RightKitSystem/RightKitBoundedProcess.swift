import Foundation

#if os(macOS)

/// The outcome of one bounded run.
public struct RightKitProcessResult: Equatable, Sendable {
    public var stdout: String
    public var stderr: String
    /// Exit status, or -1 when the process never started.
    public var status: Int32
    public var timedOut: Bool
    /// Set when the process could not be launched.
    public var launchError: String?

    public var succeeded: Bool { launchError == nil && !timedOut && status == 0 }
}

/// Runs a child process with a time limit and capped output. Each stream is read as it arrives, the
/// first `outputCap` bytes kept and the rest drained so the child never blocks on a full pipe, and
/// reading stops `drainWait` seconds after the child exits so a grandchild that holds a pipe open
/// cannot hold the result. After `timeout` the child gets SIGTERM, and SIGKILL two seconds later.
public enum RightKitBoundedProcess {
    private final class Capture: @unchecked Sendable {
        private let lock = NSLock()
        private var kept = Data()
        private let cap: Int
        let finished = DispatchSemaphore(value: 0)

        init(cap: Int) { self.cap = cap }

        func append(_ chunk: Data) {
            lock.lock(); defer { lock.unlock() }
            kept.append(chunk.prefix(max(0, cap - kept.count)))
        }

        var text: String {
            lock.lock(); defer { lock.unlock() }
            return String(decoding: kept, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        }

        func attach(to pipe: Pipe) {
            pipe.fileHandleForReading.readabilityHandler = { [self] handle in
                let chunk = handle.availableData
                if chunk.isEmpty {
                    handle.readabilityHandler = nil
                    finished.signal()
                } else {
                    append(chunk)
                }
            }
        }
    }

    private final class Flag: @unchecked Sendable {
        private let lock = NSLock()
        private var value = false
        func set() { lock.lock(); value = true; lock.unlock() }
        var isSet: Bool { lock.lock(); defer { lock.unlock() }; return value }
    }

    private struct ProcessBox: @unchecked Sendable { let process: Process }

    /// Blocking. Call off the main thread, or use the async overload.
    public static func run(
        executable: URL,
        arguments: [String] = [],
        environment: [String: String]? = nil,
        currentDirectory: URL? = nil,
        timeout: TimeInterval = 30,
        outputCap: Int = 64 * 1024,
        drainWait: TimeInterval = 2,
    ) -> RightKitProcessResult {
        let process = Process()
        process.executableURL = executable
        process.arguments = arguments
        if let environment { process.environment = environment }
        if let currentDirectory { process.currentDirectoryURL = currentDirectory }
        let outPipe = Pipe()
        let errPipe = Pipe()
        process.standardOutput = outPipe
        process.standardError = errPipe
        process.standardInput = FileHandle.nullDevice
        let out = Capture(cap: outputCap)
        let err = Capture(cap: outputCap)
        out.attach(to: outPipe)
        err.attach(to: errPipe)
        do {
            try process.run()
        } catch {
            outPipe.fileHandleForReading.readabilityHandler = nil
            errPipe.fileHandleForReading.readabilityHandler = nil
            return RightKitProcessResult(
                stdout: "", stderr: "", status: -1, timedOut: false, launchError: error.localizedDescription)
        }
        let timedOut = Flag()
        let box = ProcessBox(process: process)
        let stop = DispatchWorkItem {
            guard box.process.isRunning else { return }
            timedOut.set()
            box.process.terminate()
            DispatchQueue.global().asyncAfter(deadline: .now() + 2) {
                if box.process.isRunning { kill(box.process.processIdentifier, SIGKILL) }
            }
        }
        DispatchQueue.global().asyncAfter(deadline: .now() + timeout, execute: stop)
        process.waitUntilExit()
        stop.cancel()
        let deadline = DispatchTime.now() + drainWait
        _ = out.finished.wait(timeout: deadline)
        _ = err.finished.wait(timeout: deadline)
        outPipe.fileHandleForReading.readabilityHandler = nil
        errPipe.fileHandleForReading.readabilityHandler = nil
        try? outPipe.fileHandleForReading.close()
        try? errPipe.fileHandleForReading.close()
        return RightKitProcessResult(
            stdout: out.text, stderr: err.text, status: process.terminationStatus,
            timedOut: timedOut.isSet, launchError: nil)
    }

    public static func run(
        executable: URL,
        arguments: [String] = [],
        environment: [String: String]? = nil,
        currentDirectory: URL? = nil,
        timeout: TimeInterval = 30,
        outputCap: Int = 64 * 1024,
        drainWait: TimeInterval = 2,
    ) async -> RightKitProcessResult {
        await withCheckedContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                continuation.resume(returning: run(
                    executable: executable, arguments: arguments, environment: environment,
                    currentDirectory: currentDirectory, timeout: timeout, outputCap: outputCap,
                    drainWait: drainWait))
            }
        }
    }
}
#endif
