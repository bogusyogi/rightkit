//! Static registration of the sqlite-vec extension (C5).
//!
//! sqlite-vec ships as a run-time-loadable SQLite extension, but this app's
//! SQLite is the vendored/bundled build inside `rusqlite`'s
//! `bundled-sqlcipher-vendored-openssl` feature — there is no separate
//! `.dll`/`.so` to `load_extension()` at runtime, and shipping one would add
//! a second SQLite build into the binary. Instead we register the
//! extension's C entry point via `sqlite3_auto_extension`, which SQLite
//! calls automatically on every `sqlite3_open()` from then on, in-process,
//! for the lifetime of the process. This is the pattern sqlite-vec's own
//! test suite uses (see the crate's `src/lib.rs` `#[test]
//! test_rusqlite_auto_extension`) and it works unmodified on a PRAGMA
//! key-ed SQLCipher connection — verified in this repo: `vec0` virtual
//! tables create/insert/MATCH-query correctly after `PRAGMA key`.
//!
//! MUST run before the FIRST `Connection::open` call of the process (writer,
//! reader, or any test connection) — `sqlite3_auto_extension` only affects
//! connections opened after it is called. `ensure_registered()` is
//! idempotent (`std::sync::Once`) and cheap, so every open path just calls
//! it unconditionally rather than relying on a single call site.

use rightkit_db::rusqlite::ffi::sqlite3_auto_extension;
use std::sync::Once;

static REGISTER_ONCE: Once = Once::new();

/// Register the sqlite-vec extension for every SQLite connection opened by
/// this process from now on. Safe to call many times (idempotent) and from
/// multiple threads (guarded by `Once`).
#[allow(clippy::missing_transmute_annotations)] // sqlite-vec exposes this C entry point as an opaque pointer.
pub fn ensure_registered() {
    REGISTER_ONCE.call_once(|| {
        // SAFETY: `sqlite3_vec_init` is the extension's documented C entry
        // point (`sqlite-vec` crate, `extern "C" fn sqlite3_vec_init()`).
        // `sqlite3_auto_extension` requires an `unsafe extern "C" fn() ->
        // c_int`-shaped pointer; the transmute matches sqlite-vec's own test
        // (`sqlite-vec-0.1.9/src/lib.rs::test_rusqlite_auto_extension`) and
        // rusqlite's documented `load_extension`-free registration pattern.
        unsafe {
            // Proven pattern (matches the C5 feasibility smoke): cast the C
            // entry point through a thin pointer to the shape
            // sqlite3_auto_extension expects.
            sqlite3_auto_extension(Some(std::mem::transmute(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_db::rusqlite::Connection;

    #[test]
    fn vec0_virtual_table_works_after_registration() {
        ensure_registered();
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch("CREATE VIRTUAL TABLE t USING vec0(embedding float[4]);")
            .expect("create vec0 table");
        let version: String = conn
            .query_row("select vec_version()", [], |r| r.get(0))
            .expect("vec_version()");
        assert!(
            version.starts_with('v'),
            "unexpected vec_version: {version}"
        );
    }
}
