use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::Path;

/// Lowercase hex sha256 of a file.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|e| format!("open {} for hashing: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 1 << 16];
    loop {
        let n = file.read(&mut buffer).map_err(|e| format!("hash {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}
