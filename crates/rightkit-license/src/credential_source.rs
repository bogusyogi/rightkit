//! Configurable sources for the stored signed licence token (M13).
//!
//! The compatibility default is [`FileTokenSource`]: the engine reads
//! `<data_root>/<token_file>` as it always has. An app that keeps the signed
//! token in an OS credential store can opt in with [`CredentialTokenSource`]
//! (or any [`TokenSource`]) and drop the window->file bridge entirely; the
//! engine then never touches the token file. Which store an app uses is host
//! policy injected as a reader closure, exactly like the device source, so
//! this crate stays free of keyring/Tauri dependencies. This module only
//! locates token text; it classifies nothing about the signed artifact, and
//! signature, app, device and expiry are always verified by the engine after
//! the token is loaded.
//!
//! Source outcomes are deliberately narrow: `Ok(None)` means no licence is
//! stored (Free), `Err(_)` means the source failed and the check must fail
//! closed as unreadable, and `Ok(Some(_))` is bounded and trimmed before
//! verification so a malformed or oversized source cannot reach the verifier.

use std::fs;
use std::io::Read;
use std::path::PathBuf;

use super::{EngineEntitlementError, MAX_TOKEN_BYTES};

type CredentialReader = Box<dyn Fn() -> Result<Option<String>, String> + Send + Sync>;
/// What [`CredentialTokenSource::with_fence`] takes: acquire whatever excludes the licence
/// writers and return the guard that is held for the length of one read.
type SourceFence = Box<dyn Fn() -> Result<Box<dyn std::any::Any>, String> + Send + Sync>;

/// Where an engine verifier loads the stored signed token from.
pub trait TokenSource: Send + Sync {
    /// The stored token text: `Ok(None)` = nothing stored, `Err(_)` = the
    /// source could not be read (fail closed, never treated as "no licence").
    fn load_token(&self) -> Result<Option<String>, String>;
}

/// The compatibility default: a token file under the app data root. A missing
/// file is `Ok(None)`; any other failure (permissions, non-UTF-8) is `Err`.
pub struct FileTokenSource {
    path: PathBuf,
}

impl FileTokenSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl TokenSource for FileTokenSource {
    fn load_token(&self) -> Result<Option<String>, String> {
        let file = match fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let mut raw = String::new();
        file.take(MAX_TOKEN_BYTES + 1)
            .read_to_string(&mut raw)
            .map_err(|e| e.to_string())?;
        Ok(Some(raw))
    }
}

/// Opt-in token source backed by a host credential store. The reader returns
/// the stored signed token (e.g. an OS-store `get`): `Ok(None)` = no licence
/// stored, `Err(_)` = store unavailable/unreadable.
///
/// The reader may also return the stored record text itself (an M12 envelope or the older bare
/// `@rightkit/license` record, both JSON): the token is then taken out of it, a tombstone
/// (a deactivated licence) is no licence, and a record whose status is not active is no
/// licence. See [`crate::stored_signed_token`].
///
/// With the `client` feature, `CredentialTokenSource::from_record_store` builds a source that
/// also observes the native client's mutation fencing (the licence operation lock, a rotation
/// marker, unresolved state) and fails closed while a write may be in flight.
pub struct CredentialTokenSource {
    reader: CredentialReader,
    fence: Option<SourceFence>,
}

impl CredentialTokenSource {
    pub fn new(
        reader: impl Fn() -> Result<Option<String>, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            reader: Box::new(reader),
            fence: None,
        }
    }

    /// Run `fence` before every read and keep the guard it returns until the read is done. An
    /// `Err` from the fence is the read's `Err` (the check fails closed as unreadable). This is
    /// how a reader excludes a licence writer that is mid-write; the native client's own fence
    /// is built by `CredentialTokenSource::from_record_store` (feature `client`).
    pub fn with_fence(
        mut self,
        fence: impl Fn() -> Result<Box<dyn std::any::Any>, String> + Send + Sync + 'static,
    ) -> Self {
        self.fence = Some(Box::new(fence));
        self
    }
}

impl TokenSource for CredentialTokenSource {
    fn load_token(&self) -> Result<Option<String>, String> {
        // Held until the reader has returned.
        let _hold = match &self.fence {
            Some(fence) => Some(fence()?),
            None => None,
        };
        (self.reader)()
    }
}

/// Load + bound a token from any source, mapping outcomes onto the engine's
/// error vocabulary: missing -> NoLicense, source failure / oversized /
/// empty -> Unreadable.
pub(crate) fn load_bounded_token(
    source: &dyn TokenSource,
) -> Result<String, EngineEntitlementError> {
    let raw = source
        .load_token()
        .map_err(EngineEntitlementError::Unreadable)?
        .ok_or(EngineEntitlementError::NoLicense)?;
    if raw.len() as u64 > MAX_TOKEN_BYTES {
        return Err(EngineEntitlementError::Unreadable(
            "token too large".to_string(),
        ));
    }
    let token = raw.trim();
    if token.is_empty() {
        return Err(EngineEntitlementError::Unreadable("empty token".into()));
    }
    // A signed token is `payload.signature` and never starts with `{`; a source that hands over
    // the stored record instead is unwrapped here (envelope or bare record, tombstone = none).
    if token.starts_with('{') {
        let inner = crate::record_text::stored_signed_token(token)
            .map_err(EngineEntitlementError::Unreadable)?
            .ok_or(EngineEntitlementError::NoLicense)?;
        if inner.len() as u64 > MAX_TOKEN_BYTES {
            return Err(EngineEntitlementError::Unreadable(
                "token too large".to_string(),
            ));
        }
        return Ok(inner);
    }
    Ok(token.to_string())
}
