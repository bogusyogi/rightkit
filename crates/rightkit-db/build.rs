//! Builds SQLCipher on its LibTomCrypt provider for the `sqlcipher-libtomcrypt` feature (rightkit#1447).
//!
//! The recipe is the one the sqlcipher-src README documents (every LibTomCrypt source as its own
//! translation unit with `LTC_SOURCE`, `sqlcipher.c` with `SQLITE_HAS_CODEC` and
//! `SQLCIPHER_CRYPTO_LIBTOMCRYPT`), plus the SQLite defines libsqlite3-sys passes for its own bundled
//! SQLCipher. It was proven on the Dell against a real OpenSSL-written HeardRight history database
//! (heardright `bakeoff/sqlcipher-ltc`): the database deciphers, passes `cipher_integrity_check`,
//! accepts writes and a WAL checkpoint, and a wrong key is refused with `SQLITE_NOTADB`.
//!
//! The objects go into one static library named `sqlcipher`, which is the library libsqlite3-sys links
//! when its `sqlcipher` feature is on without `bundled-sqlcipher`; this script only supplies the
//! search path to it. The sources are copied into `OUT_DIR` first: under a deep `CARGO_HOME` the
//! deepest LibTomCrypt paths pass Windows' 260-character MAX_PATH and cl.exe fails with C1083.

#[cfg(feature = "sqlcipher-libtomcrypt")]
use std::path::{Path, PathBuf};

#[cfg(feature = "sqlcipher-libtomcrypt")]
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create the SQLCipher source copy directory");
    for entry in std::fs::read_dir(from).expect("read the sqlcipher-src source directory") {
        let entry = entry.expect("read a sqlcipher-src directory entry");
        let target = to.join(entry.file_name());
        if entry
            .file_type()
            .expect("stat a sqlcipher-src entry")
            .is_dir()
        {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy a sqlcipher-src file");
        }
    }
}

#[cfg(feature = "sqlcipher-libtomcrypt")]
fn build_sqlcipher_libtomcrypt() {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let dir = out_dir.join("src");
    copy_tree(sqlcipher_src::source_dir(), &dir);
    let include = dir.join(sqlcipher_src::LIBTOMCRYPT_INCLUDE_DIR);

    let mut ltc = cc::Build::new();
    ltc.include(&include)
        .define("LTC_SOURCE", None)
        .warnings(false)
        .cargo_metadata(false);
    for source in sqlcipher_src::LIBTOMCRYPT_SOURCES {
        ltc.file(dir.join(source));
    }
    let ltc_objects = ltc.compile_intermediates();

    let mut sqlcipher = cc::Build::new();
    sqlcipher
        .file(dir.join(sqlcipher_src::SOURCE_FILE))
        .include(&dir)
        .include(&include)
        .define("SQLITE_CORE", None)
        .define("SQLITE_THREADSAFE", "1")
        .define("SQLITE_DEFAULT_FOREIGN_KEYS", "1")
        .define("SQLITE_ENABLE_FTS3", None)
        .define("SQLITE_ENABLE_FTS3_PARENTHESIS", None)
        .define("SQLITE_ENABLE_FTS5", None)
        .define("SQLITE_ENABLE_RTREE", None)
        .define("SQLITE_ENABLE_DBSTAT_VTAB", None)
        .define("SQLITE_ENABLE_STAT4", None)
        .define("SQLITE_ENABLE_JSON1", None)
        .define("SQLITE_USE_URI", None)
        .define("SQLITE_HAS_CODEC", None)
        .define("SQLCIPHER_CRYPTO_LIBTOMCRYPT", None)
        .define("SQLITE_TEMP_STORE", "2")
        .define("SQLITE_EXTRA_INIT", "sqlcipher_extra_init")
        .define("SQLITE_EXTRA_SHUTDOWN", "sqlcipher_extra_shutdown")
        .define("HAVE_STDINT_H", "1")
        .objects(ltc_objects)
        .warnings(false)
        .cargo_metadata(false);
    sqlcipher.compile("sqlcipher");

    // The library is named `sqlcipher` and libsqlite3-sys asks the linker for `sqlcipher`; only the
    // search path is ours to give. It is emitted here because a dependent's final link receives the
    // search paths of every build script in its graph.
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows") {
        // The LibTomCrypt random generator reads the OS entropy source through CryptoAPI.
        println!("cargo:rustc-link-lib=advapi32");
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "sqlcipher-libtomcrypt")]
    build_sqlcipher_libtomcrypt();
}
