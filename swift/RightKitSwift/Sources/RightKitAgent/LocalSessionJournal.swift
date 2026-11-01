import Foundation

/// One durable journal record, mirroring `JournalEvent` in
/// `engine/crates/mobile_runtime/src/journal.rs`. Redacted by construction on the Rust side —
/// see that module's doc comment — so nothing here can ever carry a raw credential.
public struct JournalEvent: Decodable, Sendable, Equatable {
    public let sequence: UInt64
    public let nonce: String
    public let kind: JournalEventKind
}

/// Mirrors `JournalEventKind`'s `#[serde(rename_all = "camelCase", tag = "kind")]` wire shape:
/// `{"kind": "userMessage", "preview": "..."}` and so on. Hand-written rather than
/// Codable-synthesized because Swift has no direct equivalent of serde's externally-tagged
/// enum-with-fields representation.
public enum JournalEventKind: Decodable, Sendable, Equatable {
    case userMessage(preview: String)
    case protocolEvent(conduitSeq: Int64)
    case toolCall(toolName: String, ok: Bool)
    case credentialRefreshed(provider: String)
    case effectApplied(effectId: String)

    private enum CodingKeys: String, CodingKey {
        case kind, preview, conduitSeq, toolName, ok, provider, effectId
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "userMessage":
            self = .userMessage(preview: try container.decode(String.self, forKey: .preview))
        case "protocolEvent":
            self = .protocolEvent(conduitSeq: try container.decode(Int64.self, forKey: .conduitSeq))
        case "toolCall":
            self = .toolCall(
                toolName: try container.decode(String.self, forKey: .toolName),
                ok: try container.decode(Bool.self, forKey: .ok)
            )
        case "credentialRefreshed":
            self = .credentialRefreshed(provider: try container.decode(String.self, forKey: .provider))
        case "effectApplied":
            self = .effectApplied(effectId: try container.decode(String.self, forKey: .effectId))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "unknown journal event kind '\(kind)'"
            )
        }
    }
}

// MARK: - Direct C-symbol bindings
//
// Dispatch D / Lane D1 added `coderight_mobile_journal_repair/replay/cursor` (plus
// `_last_error`, `_append`, `_migrate_metadata`) to `MobileRuntime.h`, so this reader
// now calls the header-declared symbols directly instead of binding them by linker name.

/// Swift-side reader/paginator over the durable per-session journal (ENG-33..35). Lets a
/// session reopen exactly where it left off, and lets the UI scroll back through older history
/// in bounded pages instead of holding the whole journal in memory (atom 22).
///
/// GAP: Lane D3 added a bounded `replay_range(from, limit)` to `journal.rs`, but it is not
/// exposed through `coderight_mobile_journal_*` in `MobileRuntime.h` — only the
/// unbounded `coderight_mobile_journal_replay(cursor)` ("everything with `sequence > cursor`",
/// scanning from the start every call) is declared there. Until a `coderight_mobile_journal_
/// replay_range` entry point is added to the FFI/header, this type keeps materializing a
/// bounded page by requesting the suffix starting just below the page's floor sequence and
/// trimming to the page's ceiling client-side, then caching only that page — so app-side
/// memory stays bounded to the pages actually viewed even though the Rust-side disk read is
/// not yet windowed. Once that entry point exists, swap `loadPage(floor:ceiling:)`'s
/// implementation over to it; no caller of this type would need to change.
@MainActor
@Observable
public final class LocalSessionJournal {
    public let sessionId: String
    private let storageDir: String
    public let pageSize: Int

    /// Highest durably-committed sequence number as of the last `refreshTotalSequence()` call,
    /// or 0 for an empty/absent journal.
    public private(set) var totalSequence: UInt64 = 0
    /// True once `loadOlderPage()` has walked back to sequence 0 and there is nothing further
    /// back to load.
    public private(set) var reachedStart: Bool = false

    private var pageCache: [UInt64: [JournalEvent]] = [:]
    private var oldestLoadedFloor: UInt64?

    private static func cursorDefaultsKey(_ sessionId: String) -> String {
        "LocalSessionJournal.cursor.\(sessionId)"
    }

    public init(sessionId: String, storageDir: String, pageSize: Int = 50) {
        self.sessionId = sessionId
        self.storageDir = storageDir
        self.pageSize = max(1, pageSize)
    }

    /// Repair a torn/corrupt tail before this session's journal is touched this launch
    /// (ENG-35). Call once per launch before any other method on this type.
    public func repairIfNeeded() {
        _ = withSessionCStrings { storageDirC, sessionIdC in
            coderight_mobile_journal_repair(storageDirC, sessionIdC)
        }
    }

    /// The position a previous launch of this session persisted, or 0 for a session that has
    /// never saved one. Restoring this before the host resumes draining is what lets a session
    /// "reopen at its last cursor" instead of replaying everything from the beginning.
    public var savedCursor: Int64 {
        Int64(UserDefaults.standard.integer(forKey: Self.cursorDefaultsKey(sessionId)))
    }

    /// Persist the position this session should reopen at. Call whenever the host's own
    /// drain cursor (`coderight_mobile_runtime_cursor`) advances.
    public func saveCursor(_ cursor: Int64) {
        UserDefaults.standard.set(Int(cursor), forKey: Self.cursorDefaultsKey(sessionId))
    }

    /// Refresh `totalSequence` from the durable journal's current high-water mark.
    public func refreshTotalSequence() {
        let cursor = withSessionCStrings { storageDirC, sessionIdC in
            coderight_mobile_journal_cursor(storageDirC, sessionIdC)
        }
        totalSequence = cursor >= 0 ? UInt64(cursor) : 0
    }

    /// Load the newest page: the most recent `pageSize` records. Call this when a session
    /// first opens so the view starts near where the user left off rather than materializing
    /// the entire journal.
    public func loadLatestPage() -> [JournalEvent] {
        pageCache.removeAll()
        oldestLoadedFloor = nil
        reachedStart = false
        refreshTotalSequence()
        guard totalSequence > 0 else {
            reachedStart = true
            return []
        }
        let floor = totalSequence > UInt64(pageSize) ? totalSequence - UInt64(pageSize) : 0
        oldestLoadedFloor = floor
        reachedStart = floor == 0
        return loadPage(floor: floor, ceiling: totalSequence)
    }

    /// Load the next page further back in history than everything loaded so far ("scroll back
    /// for more"). Returns an empty array once `reachedStart` is true.
    public func loadOlderPage() -> [JournalEvent] {
        guard let floor = oldestLoadedFloor, floor > 0 else {
            reachedStart = true
            return []
        }
        let newFloor = floor > UInt64(pageSize) ? floor - UInt64(pageSize) : 0
        let page = loadPage(floor: newFloor, ceiling: floor)
        oldestLoadedFloor = newFloor
        reachedStart = newFloor == 0
        return page
    }

    /// Fetch and cache one bounded window `(floor, ceiling]`, oldest record first.
    private func loadPage(floor: UInt64, ceiling: UInt64) -> [JournalEvent] {
        if let cached = pageCache[floor] { return cached }
        let events = replay(after: floor).filter { $0.sequence <= ceiling }
        pageCache[floor] = events
        return events
    }

    private func replay(after cursor: UInt64) -> [JournalEvent] {
        let json: String? = withSessionCStrings { storageDirC, sessionIdC in
            guard let raw = coderight_mobile_journal_replay(storageDirC, sessionIdC, Int64(cursor)) else {
                return nil
            }
            defer { coderight_mobile_string_free(raw) }
            return String(cString: raw)
        }
        guard let json, let data = json.data(using: .utf8) else { return [] }
        return (try? JSONDecoder().decode([JournalEvent].self, from: data)) ?? []
    }

    private func withSessionCStrings<T>(_ body: (UnsafePointer<CChar>?, UnsafePointer<CChar>?) -> T) -> T {
        storageDir.withCString { storageDirC in
            sessionId.withCString { sessionIdC in
                body(storageDirC, sessionIdC)
            }
        }
    }
}
