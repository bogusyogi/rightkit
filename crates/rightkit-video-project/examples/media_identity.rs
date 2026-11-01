//! Prints the identities RightKit's media catalogue records for a binary: the file's SHA-256
//! and size, and the content identity (per-slice digest) computed by `rightkit-ffmpeg` itself.
//! Usage: media_identity <file>

use std::path::PathBuf;

fn main() {
    let path = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: media_identity <file>"),
    );
    let size = std::fs::metadata(&path).expect("file metadata").len();
    println!("file: {}", path.display());
    println!("size: {size}");
    println!(
        "sha256_file: {}",
        rightkit_ffmpeg::sha256_file(&path).expect("sha256_file")
    );
    println!(
        "content_identity_file: {:?}",
        rightkit_ffmpeg::content_identity_file(&path).expect("content_identity_file")
    );
}
