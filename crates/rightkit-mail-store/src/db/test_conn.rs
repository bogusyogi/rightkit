use super::*;

/// Test-only connection: in-memory and unencrypted (SQLCipher with no PRAGMA key
/// behaves as plain SQLite), so unit tests need neither a key file nor the
/// Windows Credential Manager. Mirrors the real `open_at` schema path —
/// `migrate` then `ensure_runtime_schema` — so tests see the same columns
/// production does (e.g. the conditionally-added `auth_results`/`send_at`).
pub fn test_conn() -> Connection {
    semantic::vec_ext::ensure_registered();
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .expect("enable fk");
    migrate(&conn).expect("migrate");
    ensure_runtime_schema(&conn).expect("ensure runtime schema");
    conn
}
