//! Checked-in bindings must match the vendored ggml version and headers.
//!
//! Each `bindings-<target>.rs` carries a `// ggml-sys-stamp` line written by
//! `regenerate-bindings`. This test fails when a header or the pinned version
//! changes without regenerating, or when a stamp names the wrong target.

#[path = "../bindings_stamp.rs"]
mod bindings_stamp;

use std::fs;
use std::path::PathBuf;

/// Checked-in bindings that predate the stamp. Each must still be regenerated
/// on its own host before it can be stamped (Windows: GenRight, see VERSIONS.md).
const PENDING_UNSTAMPED: &[&str] = &["bindings-x86_64-pc-windows-msvc.rs"];

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Version from the `**Reported version**` line of VERSIONS.md (e.g. `0.9.8`).
fn versions_md_version(versions: &str) -> String {
    let line = versions
        .lines()
        .find(|line| line.contains("**Reported version**"))
        .expect("VERSIONS.md has a **Reported version** line");
    line.rsplit("**")
        .nth(1)
        .expect("version wrapped in ** in VERSIONS.md")
        .trim()
        .to_string()
}

fn pinned_ggml_version() -> (String, String) {
    let dir = crate_dir();
    let versions = fs::read_to_string(dir.join("VERSIONS.md")).expect("read VERSIONS.md");
    let cmake = fs::read_to_string(dir.join("vendor/ggml/CMakeLists.txt"))
        .expect("read vendored ggml CMakeLists.txt");
    let vendored = bindings_stamp::ggml_version_from_cmake(&cmake)
        .expect("vendored ggml CMakeLists.txt declares GGML_VERSION_*");
    (versions_md_version(&versions), vendored)
}

fn bindings_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(crate_dir())
        .expect("read crate dir")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            name.starts_with("bindings-") && name.ends_with(".rs")
        })
        .collect();
    files.sort();
    files
}

#[test]
fn versions_md_matches_vendored_ggml() {
    let (documented, vendored) = pinned_ggml_version();
    assert_eq!(
        documented, vendored,
        "VERSIONS.md reported version must equal vendor/ggml/CMakeLists.txt"
    );
}

#[test]
fn checked_in_bindings_match_vendored_headers() {
    let dir = crate_dir();
    let (ggml, _) = pinned_ggml_version();
    let headers = bindings_stamp::ggml_headers_digest(&dir.join("vendor/ggml/include"))
        .expect("digest vendored ggml headers");

    let files = bindings_files();
    assert!(
        !files.is_empty(),
        "no checked-in bindings-<target>.rs found"
    );

    let mut stale = Vec::new();
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let target = name
            .strip_prefix("bindings-")
            .and_then(|rest| rest.strip_suffix(".rs"))
            .expect("bindings file name");
        let contents = fs::read_to_string(path).expect("read bindings file");

        if PENDING_UNSTAMPED.contains(&name.as_str()) {
            assert!(
                bindings_stamp::parse_stamp(&contents).is_none(),
                "{name} is listed as pending but already carries a stamp; remove it from PENDING_UNSTAMPED"
            );
            continue;
        }

        match bindings_stamp::parse_stamp(&contents) {
            None => stale.push(format!("{name}: no ggml-sys-stamp line; regenerate it")),
            Some(stamp) => {
                if stamp.target != target {
                    stale.push(format!("{name}: stamped for target {}", stamp.target));
                }
                if stamp.ggml != ggml {
                    stale.push(format!("{name}: ggml {} != pinned {ggml}", stamp.ggml));
                }
                if stamp.headers != headers {
                    stale.push(format!(
                        "{name}: headers {} != vendored {headers}; regenerate",
                        stamp.headers
                    ));
                }
            }
        }
    }
    assert!(
        stale.is_empty(),
        "stale checked-in bindings:\n{}",
        stale.join("\n")
    );

    for pending in PENDING_UNSTAMPED {
        assert!(
            dir.join(pending).is_file(),
            "PENDING_UNSTAMPED entry {pending} no longer exists"
        );
    }
}
