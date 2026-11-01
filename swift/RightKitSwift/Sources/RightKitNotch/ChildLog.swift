#if os(macOS)
import Foundation
import RightKitDiagnostics

/// Durable output for the children the notch supervises (the hub and Membrane).
///
/// Their stderr used to go to `/dev/null`, so a crash left nothing to read. It now
/// goes to `<Pulse state>/logs/<name>-stderr.log`, appended, with size-based
/// rotation done here before each spawn (live file plus two older generations,
/// 5 MB each). Lifecycle records (a respawn, a latch) are one JSON object per line
/// in `<Pulse state>/logs/<name>-respawns.jsonl`, appended on a utility queue.
///
/// Everything here is blocking file I/O and is nonisolated: call it from a
/// background task, never the main actor. It carries no message bodies or paths,
/// only what the caller passes in a record.
enum ChildLog {
    /// A stderr log rotates once it reaches this size.
    static let maxBytes: UInt64 = 5 * 1024 * 1024
    /// Older generations kept beside the live file (`.1`, `.2`).
    static let keptGenerations = 2
    /// A lifecycle record file rotates at this size (one older generation).
    static let recordMaxBytes: UInt64 = 1024 * 1024

    nonisolated static func logsDirectory(state: URL) -> URL {
        state.appendingPathComponent("logs", isDirectory: true)
    }

    /// Rotates `file` when it is full, then opens it for appending (created 0600 in a 0700
    /// folder). The caller hands the result to `Process.standardError`, and closes its own
    /// copy after `run()`. `nil` when the folder or file cannot be made: the child then
    /// runs with its stderr discarded, as before, and the caller records that.
    nonisolated static func openForAppend(_ file: URL) -> FileHandle? {
        let manager = FileManager.default
        do {
            try manager.createDirectory(at: file.deletingLastPathComponent(),
                                        withIntermediateDirectories: true,
                                        attributes: [.posixPermissions: 0o700])
        } catch { return nil }
        rotateIfNeeded(file, maxBytes: maxBytes, generations: keptGenerations)
        let descriptor = open(file.path, O_WRONLY | O_APPEND | O_CREAT | O_CLOEXEC, 0o600)
        guard descriptor >= 0 else { return nil }
        return FileHandle(fileDescriptor: descriptor, closeOnDealloc: true)
    }

    /// `file` becomes `file.1`, `file.1` becomes `file.2`, and the oldest is dropped.
    nonisolated static func rotateIfNeeded(_ file: URL, maxBytes: UInt64, generations: Int) {
        let manager = FileManager.default
        guard let size = (try? manager.attributesOfItem(atPath: file.path))?[.size] as? UInt64,
              size >= maxBytes else { return }
        func numbered(_ index: Int) -> URL { file.appendingPathExtension(String(index)) }
        try? manager.removeItem(at: numbered(generations))
        var index = generations - 1
        while index >= 1 {
            try? manager.moveItem(at: numbered(index), to: numbered(index + 1))
            index -= 1
        }
        try? manager.moveItem(at: file, to: numbered(1))
    }

    /// Appends one record to `file` off the calling thread, in call order. The store stamps
    /// it and keeps the file within `recordMaxBytes`. `fields` holds numbers and short fixed
    /// strings only, so nothing is redacted.
    nonisolated static func appendRecord(_ event: String, fields: [String: Int], to file: URL) {
        let store = JSONLDiagnosticStore(
            fileURL: file,
            options: StoreOptions(redaction: .none, maxBytes: Int(recordMaxBytes)))
        store.enqueue(DiagnosticEvent(name: event, fields: fields.mapValues { String($0) }))
    }
}
#endif
