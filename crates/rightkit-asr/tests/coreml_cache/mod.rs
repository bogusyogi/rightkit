//! CoreML keeps its compiled-model cache in `~/Library/Caches/<executable name>`.
//! Cargo gives every rebuilt test binary a new hash (`e2e_parakeet-9adf…`), so
//! each rebuild started a fresh multi-GB cache that nothing removed. Before a
//! CoreML load, delete this test's caches left by earlier binaries.

/// Remove `~/Library/Caches/<stem>-<hash>` for every hash but the running one.
pub fn prune_stale() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let (Some(home), Ok(exe)) = (std::env::var_os("HOME"), std::env::current_exe()) else { return };
        let Some(name) = exe.file_name().and_then(|n| n.to_str()).map(str::to_owned) else { return };
        let Some((stem, _)) = name.rsplit_once('-') else { return };
        let caches = std::path::Path::new(&home).join("Library/Caches");
        for entry in std::fs::read_dir(&caches).into_iter().flatten().flatten() {
            let file = entry.file_name();
            let Some(other) = file.to_str() else { continue };
            let stale = other != name
                && other.strip_prefix(stem).and_then(|rest| rest.strip_prefix('-')).is_some_and(|hash| {
                    hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit())
                });
            if stale {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    });
}
