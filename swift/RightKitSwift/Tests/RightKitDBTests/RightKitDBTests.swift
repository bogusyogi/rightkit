import Foundation
import SQLite3
import Testing
@testable import RightKitDB

@Suite struct RightKitDBTests {
    private func tempPath() -> String {
        NSTemporaryDirectory() + "rkdb-\(UUID().uuidString).sqlite"
    }

    private func makeWriter(_ path: String, wal: Bool = true) -> OpaquePointer {
        var db: OpaquePointer?
        #expect(sqlite3_open_v2(path, &db, SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, nil) == SQLITE_OK)
        if wal { sqlite3_exec(db, "PRAGMA journal_mode=WAL", nil, nil, nil) }
        sqlite3_exec(db, "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT, b BLOB); PRAGMA user_version=3;", nil, nil, nil)
        return db!
    }

    @Test func readsAndNeverWrites() throws {
        let path = tempPath(); defer { for s in ["", "-wal", "-shm"] { try? FileManager.default.removeItem(atPath: path + s) } }
        let w = makeWriter(path)
        sqlite3_exec(w, "INSERT INTO t(name,b) VALUES('a', x'0102')", nil, nil, nil)
        let r = try RightKitReadOnlyDB(path: path)
        #expect(try r.userVersion() == 3)
        #expect(try r.journalMode() == "wal")
        let rows = try r.query("SELECT name, b FROM t WHERE name = ?", [.text("a")])
        #expect(rows == [[.text("a"), .blob(Data([1, 2]))]])
        #expect(throws: RightKitDBError.self) { try r.query("INSERT INTO t(name) VALUES('x')") }
        #expect(throws: RightKitDBError.self) { try r.query("CREATE TABLE z(x)") }
        sqlite3_close(w)
    }

    @Test func seesUncheckpointedWalWrites() throws {
        let path = tempPath(); defer { for s in ["", "-wal", "-shm"] { try? FileManager.default.removeItem(atPath: path + s) } }
        let w = makeWriter(path)
        sqlite3_exec(w, "PRAGMA wal_autocheckpoint=0", nil, nil, nil)
        let r = try RightKitReadOnlyDB(path: path)
        sqlite3_exec(w, "INSERT INTO t(name) VALUES('live')", nil, nil, nil)
        #expect(try r.query("SELECT name FROM t") == [[.text("live")]])
        sqlite3_close(w)
    }

    @Test func missingFileIsNotCreated() {
        let path = tempPath()
        #expect(throws: RightKitDBError.missing(path: path)) { try RightKitReadOnlyDB(path: path) }
        #expect(!FileManager.default.fileExists(atPath: path))
    }

    @Test func nonDatabaseIsTyped() throws {
        let path = tempPath(); defer { try? FileManager.default.removeItem(atPath: path) }
        try Data(repeating: 0x41, count: 4096).write(to: URL(fileURLWithPath: path))
        do { _ = try RightKitReadOnlyDB(path: path); Issue.record("expected failure") }
        catch let e as RightKitDBError { if case .notADatabase = e {} else { Issue.record("wrong error \(e)") } }
    }

    @Test func closedHandleThrows() throws {
        let path = tempPath(); defer { for s in ["", "-wal", "-shm"] { try? FileManager.default.removeItem(atPath: path + s) } }
        sqlite3_close(makeWriter(path))
        let r = try RightKitReadOnlyDB(path: path)
        r.close()
        #expect(throws: RightKitDBError.closed) { try r.query("SELECT 1") }
    }
}
