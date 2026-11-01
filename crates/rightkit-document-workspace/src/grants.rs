//! The path-grant seam. The host app owns which paths the webview may read or write; the
//! workspace functions only ask it questions through [`PathGrants`].

use std::path::{Path, PathBuf};

/// Path grants as the host's broker exposes them. Every error is the broker's user-facing
/// message; the workspace maps it with [`crate::broker_error`].
pub trait PathGrants {
    /// Canonicalize an existing file path (no grant check).
    fn normalize_existing_file(&self, path: &str) -> Result<PathBuf, String>;
    /// The display spelling of a path (for example without a Windows verbatim prefix).
    fn path_to_string(&self, path: &Path) -> String;
    /// `Ok` when the normalized path is granted, else the broker's denial wording.
    fn require_granted_normalized(&self, path: &Path, what: &str) -> Result<(), String>;
    /// Resolve a granted, existing file and return its display path.
    fn require_existing_file(&self, path: &str) -> Result<String, String>;
    /// Resolve a path the webview may write and return its display path.
    fn require_write_target(&self, path: &str) -> Result<String, String>;
    /// Record a path that was just written, so it can be re-granted after a save.
    fn record_write_target(&self, path: &Path) -> Result<(), String>;
    /// Grant read access to an existing file and return its display path.
    fn grant_existing_file_path(&self, path: &Path) -> Result<String, String>;
    /// Grant every path that still resolves to an existing file; failures are dropped.
    fn grant_existing_files(&self, paths: &[PathBuf]) -> Vec<String> {
        paths
            .iter()
            .filter_map(|p| self.grant_existing_file_path(p).ok())
            .collect()
    }
    /// Exchange a stored (database) path for a one-shot token.
    fn register_known_stored_path(&self, path: &str) -> Result<String, String>;
    /// Mint (or return the stable) opaque URL token for a granted binary document.
    fn grant_stream_token(&self, display: &str) -> Result<String, String>;
    /// Revoke a previously granted path.
    fn revoke_path(&self, display: &str) -> Result<(), String>;
}

/// Authorize an existing file for reading: canonicalize it, require a live grant, and answer its
/// display path.
pub fn authorize_existing_for_read(path: &str, grants: &dyn PathGrants) -> Result<String, String> {
    let normalized = grants.normalize_existing_file(path)?;
    grants.require_granted_normalized(&normalized, "path")?;
    Ok(grants.path_to_string(&normalized))
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Mutex;

    /// An in-memory grant table for tests: exact canonical-path grants, no tokens.
    #[derive(Default)]
    pub struct MemoryGrants {
        granted: Mutex<HashSet<PathBuf>>,
    }

    impl PathGrants for MemoryGrants {
        fn normalize_existing_file(&self, path: &str) -> Result<PathBuf, String> {
            let canon = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
            if canon.is_file() {
                Ok(canon)
            } else {
                Err(format!("not a file: {path}"))
            }
        }
        fn path_to_string(&self, path: &Path) -> String {
            path.to_string_lossy().into_owned()
        }
        fn require_granted_normalized(&self, path: &Path, what: &str) -> Result<(), String> {
            if self.granted.lock().unwrap().contains(path) {
                Ok(())
            } else {
                Err(format!("{what} is not authorized"))
            }
        }
        fn require_existing_file(&self, path: &str) -> Result<String, String> {
            let normalized = self.normalize_existing_file(path)?;
            self.require_granted_normalized(&normalized, "path")?;
            Ok(self.path_to_string(&normalized))
        }
        fn require_write_target(&self, path: &str) -> Result<String, String> {
            self.require_existing_file(path)
        }
        fn record_write_target(&self, _path: &Path) -> Result<(), String> {
            Ok(())
        }
        fn grant_existing_file_path(&self, path: &Path) -> Result<String, String> {
            let canon = self.normalize_existing_file(&path.to_string_lossy())?;
            self.granted.lock().unwrap().insert(canon.clone());
            Ok(self.path_to_string(&canon))
        }
        fn register_known_stored_path(&self, path: &str) -> Result<String, String> {
            self.grant_existing_file_path(Path::new(path))
        }
        fn grant_stream_token(&self, display: &str) -> Result<String, String> {
            Ok(format!("stream:{display}"))
        }
        fn revoke_path(&self, display: &str) -> Result<(), String> {
            self.granted.lock().unwrap().remove(Path::new(display));
            Ok(())
        }
    }
}
