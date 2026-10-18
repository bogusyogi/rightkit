import Foundation

#if os(macOS)
import Darwin

/// Durable output for supervised children: stderr appended to a file with size-based rotation done
/// before each spawn, and lifecycle records as one JSON object per line.
///
/// Everything here is blocking file I/O: call it from a background task, never the main actor.
/// Files are created 0600 in a 0700 folder. Records hold only what the caller passes: numbers and
/// short fixed strings, never message bodies or paths.
public enum RightKitChildLog {
    /// A stderr log rotates once it reaches this size.
    public static let maxBytes: UInt64 = 5 * 1024 * 1024
    /// Older generations kept beside the live file (`.1`, `.2`).
    public static let keptGenerations = 2
    /// A lifecycle record file rotates at this size (one older generation is kept).
    public static let recordMaxBytes: UInt64 = 1024 * 1024

    private static let recordQueue = DispatchQueue(label: "dev.orthic.rightkit.child-log", qos: .utility)

    /// `<state>/logs`.
    public static func logsDirectory(state: URL) -> URL {
        state.appendingPathComponent("logs", isDirectory: true)
    }

    /// Rotates `file` when it is full, then opens it for appending. The caller hands the result to
    /// `Process.standardError` and closes its own copy after `run()`. Nil when the folder or file
    /// cannot be made: the child then runs with its stderr discarded.
    public static func openForAppend(
        _ file: URL, maxBytes: UInt64 = RightKitChildLog.maxBytes, generations: Int = RightKitChildLog.keptGenerations,
    ) -> FileHandle? {
        do {
            try FileManager.default.createDirectory(
                at: file.deletingLastPathComponent(), withIntermediateDirectories: true,
                attributes: [.posixPermissions: 0o700])
        } catch { return nil }
        rotateIfNeeded(file, maxBytes: maxBytes, generations: generations)
        let descriptor = open(file.path, O_WRONLY | O_APPEND | O_CREAT | O_CLOEXEC, 0o600)
        guard descriptor >= 0 else { return nil }
        return FileHandle(fileDescriptor: descriptor, closeOnDealloc: true)
    }

    /// When `file` has reached `maxBytes`: `file` becomes `file.1`, `file.1` becomes `file.2`, and the
    /// oldest generation is dropped. Returns whether it rotated.
    @discardableResult
    public static func rotateIfNeeded(_ file: URL, maxBytes: UInt64, generations: Int) -> Bool {
        let manager = FileManager.default
        guard generations >= 1,
              let size = (try? manager.attributesOfItem(atPath: file.path))?[.size] as? UInt64,
              size >= maxBytes else { return false }
        func numbered(_ index: Int) -> URL { file.appendingPathExtension(String(index)) }
        try? manager.removeItem(at: numbered(generations))
        var index = generations - 1
        while index >= 1 {
            try? manager.moveItem(at: numbered(index), to: numbered(index + 1))
            index -= 1
        }
        try? manager.moveItem(at: file, to: numbered(1))
        return true
    }

    /// One JSON line for `event`, with `ts_ms` added and keys sorted. Pure.
    public static func recordLine(_ event: String, fields: [String: Int], nowMilliseconds: Int) -> Data? {
        var object: [String: Any] = ["event": event, "ts_ms": nowMilliseconds]
        for (name, value) in fields { object[name] = value }
        guard var line = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]) else { return nil }
        line.append(0x0A)
        return line
    }

    /// Appends one JSON line to `file` off the calling thread.
    public static func appendRecord(_ event: String, fields: [String: Int] = [:], to file: URL) {
        guard let payload = recordLine(event, fields: fields, nowMilliseconds: Int(Date().timeIntervalSince1970 * 1000))
        else { return }
        recordQueue.async {
            do {
                try FileManager.default.createDirectory(
                    at: file.deletingLastPathComponent(), withIntermediateDirectories: true,
                    attributes: [.posixPermissions: 0o700])
            } catch { return }
            rotateIfNeeded(file, maxBytes: recordMaxBytes, generations: 1)
            let descriptor = open(file.path, O_WRONLY | O_APPEND | O_CREAT | O_CLOEXEC, 0o600)
            guard descriptor >= 0 else { return }
            let handle = FileHandle(fileDescriptor: descriptor, closeOnDealloc: true)
            try? handle.write(contentsOf: payload)
            try? handle.close()
        }
    }
}
#endif
