import Foundation
import SQLite3

// Read-only access to a rightkit-db database from Swift. The Rust `rightkit-db` crate is the only
// writer: it owns creation, journal mode, migrations, backup and recovery. This product mirrors
// the crate's `open_read_only` contract (crates/rightkit-db/src/lib.rs): SQLITE_OPEN_READ_ONLY,
// never CREATE, never PRAGMA journal_mode, never migrate or optimize, 5 s busy timeout,
// query_only verified, temp_store=MEMORY. System SQLite3 only.

public enum RightKitDBError: Error, Equatable, Sendable {
    /// The database file does not exist. A reader never creates it.
    case missing(path: String)
    /// `sqlite3_open_v2` failed (code is the SQLite result code).
    case openFailed(code: Int32, message: String)
    /// A setup pragma failed after open.
    case configurationFailed(code: Int32, message: String)
    /// The handle did not come up read-only (mirrors rightkit-db `ReadOnlyNotEffective`).
    case readOnlyNotEffective(String)
    /// Statement prepare/step/bind failed (includes SQLITE_BUSY after the timeout, and attempts
    /// to write through the read-only handle, which fail with SQLITE_READONLY).
    case statementFailed(code: Int32, message: String)
    /// A SQLite database file is present but not readable as a database (corrupt/encrypted).
    case notADatabase(String)
    case closed
}

public enum RightKitDBValue: Equatable, Sendable {
    case null
    case integer(Int64)
    case real(Double)
    case text(String)
    case blob(Data)

    public var text: String? { if case let .text(v) = self { v } else { nil } }
    public var integer: Int64? { if case let .integer(v) = self { v } else { nil } }
}

public struct RightKitDBOptions: Sendable, Equatable {
    /// Same default as rightkit-db `DbOptions::busy_timeout` (5 000 ms).
    public var busyTimeout: TimeInterval = 5
    /// Foreign databases only. When a plain read-only open fails (a WAL database needs its -shm
    /// sidecar, which exists only while the writer runs), retry with `immutable=1`. That ignores
    /// the write-ahead log, so it is safe only when no live writer can have unflushed frames.
    /// Default off: rightkit-db files are always opened WAL-aware.
    public var immutableFallback = false
    public init() {}
}

/// A read-only connection. Thread-safe: statements are serialized on a lock.
public final class RightKitReadOnlyDB: @unchecked Sendable {
    private var handle: OpaquePointer?
    private let lock = NSLock()
    public let path: String
    public let usedImmutableFallback: Bool

    public static let defaultBusyTimeoutMs: Int32 = 5_000

    public init(path: String, options: RightKitDBOptions = .init()) throws {
        guard FileManager.default.fileExists(atPath: path) else {
            throw RightKitDBError.missing(path: path)
        }
        self.path = path
        var db: OpaquePointer?
        var fellBack = false
        let flags = SQLITE_OPEN_READONLY | SQLITE_OPEN_FULLMUTEX
        var rc = sqlite3_open_v2(path, &db, flags, nil)
        if rc != SQLITE_OK, options.immutableFallback {
            let message = db.map { String(cString: sqlite3_errmsg($0)) } ?? ""
            _ = message
            sqlite3_close(db)
            db = nil
            rc = sqlite3_open_v2(Self.immutableURI(path), &db, flags | SQLITE_OPEN_URI, nil)
            fellBack = rc == SQLITE_OK
        }
        guard rc == SQLITE_OK, let db else {
            let message = db.map { String(cString: sqlite3_errmsg($0)) } ?? String(cString: sqlite3_errstr(rc))
            sqlite3_close(db)
            throw RightKitDBError.openFailed(code: rc, message: message)
        }
        self.handle = db
        self.usedImmutableFallback = fellBack
        do {
            try configure(options)
        } catch {
            sqlite3_close(db)
            self.handle = nil
            throw error
        }
    }

    deinit { close() }

    public func close() {
        lock.lock(); defer { lock.unlock() }
        if let handle { sqlite3_close(handle) }
        handle = nil
    }

    private func configure(_ options: RightKitDBOptions) throws {
        guard let db = handle else { throw RightKitDBError.closed }
        sqlite3_busy_timeout(db, Int32(options.busyTimeout * 1000))
        // Reading the schema is the first touch; it surfaces non-database/encrypted files here.
        var rc = sqlite3_exec(db, "SELECT count(*) FROM sqlite_master", nil, nil, nil)
        if rc == SQLITE_NOTADB || rc == SQLITE_CORRUPT {
            throw RightKitDBError.notADatabase(String(cString: sqlite3_errmsg(db)))
        }
        if rc != SQLITE_OK {
            throw RightKitDBError.configurationFailed(code: rc, message: String(cString: sqlite3_errmsg(db)))
        }
        for pragma in ["PRAGMA temp_store = MEMORY", "PRAGMA query_only = ON"] {
            rc = sqlite3_exec(db, pragma, nil, nil, nil)
            if rc != SQLITE_OK {
                throw RightKitDBError.configurationFailed(code: rc, message: String(cString: sqlite3_errmsg(db)))
            }
        }
        // Mirrors rightkit-db: read back, and require the main database to report read-only.
        let queryOnly = try query("PRAGMA query_only").first?.first?.integer
        guard queryOnly == 1 else {
            throw RightKitDBError.readOnlyNotEffective("PRAGMA query_only did not stay ON")
        }
        guard sqlite3_db_readonly(db, "main") == 1 else {
            throw RightKitDBError.readOnlyNotEffective("SQLITE_OPEN_READONLY handle reports a writable main database")
        }
    }

    /// Run one statement and return every row. Parameters bind positionally (`?`).
    @discardableResult
    public func query(_ sql: String, _ params: [RightKitDBValue] = []) throws -> [[RightKitDBValue]] {
        lock.lock(); defer { lock.unlock() }
        guard let db = handle else { throw RightKitDBError.closed }
        var stmt: OpaquePointer?
        var rc = sqlite3_prepare_v2(db, sql, -1, &stmt, nil)
        guard rc == SQLITE_OK, let stmt else { throw failure(db, rc) }
        defer { sqlite3_finalize(stmt) }
        let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)
        for (i, value) in params.enumerated() {
            let idx = Int32(i + 1)
            switch value {
            case .null: rc = sqlite3_bind_null(stmt, idx)
            case let .integer(v): rc = sqlite3_bind_int64(stmt, idx, v)
            case let .real(v): rc = sqlite3_bind_double(stmt, idx, v)
            case let .text(v): rc = sqlite3_bind_text(stmt, idx, v, -1, transient)
            case let .blob(v):
                rc = v.withUnsafeBytes { sqlite3_bind_blob(stmt, idx, $0.baseAddress, Int32(v.count), transient) }
            }
            guard rc == SQLITE_OK else { throw failure(db, rc) }
        }
        var rows: [[RightKitDBValue]] = []
        let columns = sqlite3_column_count(stmt)
        while true {
            rc = sqlite3_step(stmt)
            if rc == SQLITE_DONE { break }
            guard rc == SQLITE_ROW else { throw failure(db, rc) }
            var row: [RightKitDBValue] = []
            for c in 0..<columns {
                switch sqlite3_column_type(stmt, c) {
                case SQLITE_INTEGER: row.append(.integer(sqlite3_column_int64(stmt, c)))
                case SQLITE_FLOAT: row.append(.real(sqlite3_column_double(stmt, c)))
                case SQLITE_TEXT: row.append(.text(String(cString: sqlite3_column_text(stmt, c))))
                case SQLITE_BLOB:
                    let n = Int(sqlite3_column_bytes(stmt, c))
                    row.append(.blob(n == 0 ? Data() : Data(bytes: sqlite3_column_blob(stmt, c), count: n)))
                default: row.append(.null)
                }
            }
            rows.append(row)
        }
        return rows
    }

    /// `PRAGMA user_version`, where rightkit-db stores the schema version by default.
    public func userVersion() throws -> Int64 {
        try query("PRAGMA user_version").first?.first?.integer ?? 0
    }

    /// The journal mode in force (`wal`, `delete`, ...). Read only; never sets it.
    public func journalMode() throws -> String {
        try query("PRAGMA journal_mode").first?.first?.text ?? ""
    }

    private func failure(_ db: OpaquePointer, _ rc: Int32) -> RightKitDBError {
        .statementFailed(code: rc, message: String(cString: sqlite3_errmsg(db)))
    }

    private static func immutableURI(_ path: String) -> String {
        var allowed = CharacterSet.urlPathAllowed
        allowed.remove(charactersIn: "?#%")
        let encoded = path.addingPercentEncoding(withAllowedCharacters: allowed) ?? path
        return "file:\(encoded)?immutable=1"
    }
}
