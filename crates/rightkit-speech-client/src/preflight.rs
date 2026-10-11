//! Input checks the client can make before a path is sent to the service.
//!
//! These mirror only what the host enforces itself: the file must exist (the host's
//! `file not found`, code `E_ENGINE`), and the path must be UTF-8 to travel on the JSON
//! wire at all. The host has no file-size, extension or sample-rate limit, so none is
//! checked here. `API.md`, "Input contract", lists every host check and the error it gives.

use std::path::Path;

use crate::ClientError;

/// Check `path` before sending it with a transcription request.
///
/// - A path that is not valid UTF-8 returns [`ClientError::Unsupported`]. The JSON wire
///   carries UTF-8 only, so sending it would name a different path.
/// - An absolute path that does not exist returns `ClientError::Engine` with code
///   `E_ENGINE` and the host's own message, `file not found: <path>`.
/// - Anything else returns `Ok(())`. A relative path is resolved against the host's
///   working directory, which this process cannot see, so it is not checked. Readability,
///   duration and decoding are decided by the host and reported by it.
pub fn preflight_file(path: &Path) -> Result<(), ClientError> {
    if path.to_str().is_none() {
        return Err(ClientError::Unsupported(
            "the path is not valid UTF-8; the speech service's JSON wire carries UTF-8 paths only"
                .into(),
        ));
    }
    if !path.is_absolute() {
        return Ok(());
    }
    match std::fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(ClientError::Engine {
            code: "E_ENGINE".into(),
            message: format!("file not found: {}", path.display()),
        }),
        _ => Ok(()),
    }
}
