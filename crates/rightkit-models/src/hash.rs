//! File digests for model artifacts. The hashing lives in `rightkit-fs::digest`.

use std::fs;
use std::path::Path;

/// Lowercase hex sha256 of a file.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file =
        fs::File::open(path).map_err(|e| format!("open {} for hashing: {e}", path.display()))?;
    rightkit_fs::digest::sha256_reader(&mut file)
        .map_err(|e| format!("hash {}: {e}", path.display()))
}
