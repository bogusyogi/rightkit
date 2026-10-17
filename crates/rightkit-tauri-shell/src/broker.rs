//! Path broker: the renderer never names a file the user did not hand to the app.
//!
//! Donor: ViewRight `path_broker.rs` (granted file set, save-target grants with a bounded
//! write-target ring, one-shot recent-file tokens, stable per-file stream tokens, per-tab site
//! roots). Differences from the donor: errors are [`ShellError`] values, random ids come straight
//! from the OS entropy call with no extra crate, and the broker is a plain type any app can embed.
//!
//! Grants are made by Rust code (a file dialog result, a drag-drop, the launch argv, a recent-files
//! table). The renderer can only *use* grants: check a path, mint a stream id for a granted file,
//! trade a one-shot recent id. Nothing here lets the renderer grant an arbitrary path.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::error::ShellError;

/// Upper bound on live recent-file ids. Old ones are disposable (the renderer re-lists).
pub const KNOWN_TOKEN_CAP: usize = 256;
/// Ring of recently written save targets that `regrant_saved_target` may re-grant.
pub const WRITE_TARGET_RING_CAP: usize = 8;

#[derive(Default)]
struct KnownTokens {
    map: HashMap<String, PathBuf>,
    order: VecDeque<String>,
}

#[derive(Default)]
pub struct PathBroker {
    allowed: Mutex<HashSet<PathBuf>>,
    known: Mutex<KnownTokens>,
    stream_tokens: Mutex<HashMap<String, PathBuf>>,
    site_roots: Mutex<HashMap<String, PathBuf>>,
    recent_writes: Mutex<VecDeque<PathBuf>>,
    /// Name used in denial text ("through {name} first"); empty reads as "the app".
    app_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamTarget {
    pub path: PathBuf,
}

fn poisoned<T>(_: T) -> ShellError {
    ShellError::Other("path broker lock poisoned".into())
}

/// Display form of a normalised path (drops the Windows verbatim prefix).
pub fn path_to_string(path: &Path) -> String {
    let raw = path.to_string_lossy();
    #[cfg(windows)]
    {
        if let Some(stripped) = raw.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{stripped}");
        }
        if let Some(stripped) = raw.strip_prefix(r"\\?\") {
            return stripped.to_string();
        }
    }
    raw.into_owned()
}

fn display_matches(path: &Path, display: &str) -> bool {
    let normalized = path_to_string(path);
    // Windows and macOS volumes are case-insensitive by default, so a differently-cased display
    // string must still match (a deleted file cannot be re-resolved to its true case).
    #[cfg(any(windows, target_os = "macos"))]
    {
        normalized.to_lowercase() == display.to_lowercase()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        normalized == display
    }
}

pub fn normalize_existing_file(path: impl AsRef<Path>) -> Result<PathBuf, ShellError> {
    let path = path.as_ref();
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(ShellError::FileNotFound("path is not a file".into()));
    }
    Ok(std::fs::canonicalize(path)?)
}

/// Canonical form of a path that may not exist yet (a save target): the parent must exist.
pub fn normalize_write_target(path: impl AsRef<Path>) -> Result<PathBuf, ShellError> {
    let path = path.as_ref();
    if path.as_os_str().is_empty() {
        return Err(ShellError::Invalid("empty path".into()));
    }
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.is_dir() {
            return Err(ShellError::Invalid("path is a directory".into()));
        }
        return Ok(std::fs::canonicalize(path)?);
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = std::fs::canonicalize(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| ShellError::Invalid("path has no file name".into()))?;
    Ok(parent.join(name))
}

#[cfg(unix)]
fn fill_entropy(buf: &mut [u8]) -> std::io::Result<()> {
    extern "C" {
        fn getentropy(buf: *mut std::ffi::c_void, len: usize) -> std::ffi::c_int;
    }
    // SAFETY: `buf` is a valid writable slice of at most 256 bytes.
    if unsafe { getentropy(buf.as_mut_ptr().cast(), buf.len()) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn fill_entropy(buf: &mut [u8]) -> std::io::Result<()> {
    #[link(name = "advapi32")]
    extern "system" {
        #[link_name = "SystemFunction036"]
        fn RtlGenRandom(buf: *mut u8, len: u32) -> u8;
    }
    // SAFETY: `buf` is a valid writable slice for `len` bytes.
    if unsafe { RtlGenRandom(buf.as_mut_ptr(), buf.len() as u32) } != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn random_id(prefix: &str) -> Result<String, ShellError> {
    let mut bytes = [0_u8; 16];
    fill_entropy(&mut bytes)
        .map_err(|e| ShellError::Other(format!("entropy source unavailable: {e}")))?;
    let mut id = String::with_capacity(prefix.len() + 32);
    id.push_str(prefix);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(id, "{b:02x}");
    }
    Ok(id)
}

fn mint_unique<V>(map: &HashMap<String, V>, prefix: &str) -> Result<String, ShellError> {
    loop {
        let t = random_id(prefix)?;
        if !map.contains_key(&t) {
            return Ok(t);
        }
    }
}

/// Strip the macOS quarantine attribute from a file the user explicitly chose to open, so
/// Gatekeeper does not block later opens. Best effort; in-process `removexattr`, no fork.
#[cfg(target_os = "macos")]
fn clear_quarantine(path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    extern "C" {
        fn removexattr(
            path: *const std::ffi::c_char,
            name: *const std::ffi::c_char,
            options: i32,
        ) -> i32;
    }
    const XATTR_NOFOLLOW: i32 = 0x0001;
    let Ok(p) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return;
    };
    // SAFETY: both pointers are valid NUL-terminated strings for the duration of the call; the
    // result (ENOATTR when the file was never quarantined) is intentionally ignored.
    unsafe {
        removexattr(p.as_ptr(), c"com.apple.quarantine".as_ptr(), XATTR_NOFOLLOW);
    }
}

impl PathBroker {
    pub fn new() -> Self {
        Self::default()
    }

    /// A broker whose denial text names the app ("open it through {app_name} first"), taken from
    /// the shell's `app_name` config. An empty name reads as "the app".
    pub fn with_app_name(app_name: &str) -> Self {
        Self {
            app_name: app_name.trim().to_owned(),
            ..Self::default()
        }
    }

    fn app(&self) -> &str {
        if self.app_name.is_empty() {
            "the app"
        } else {
            &self.app_name
        }
    }

    /// Grant an existing file the user chose. Returns the display path.
    pub fn grant_file(&self, path: impl AsRef<Path>) -> Result<String, ShellError> {
        let normalized = normalize_existing_file(path)?;
        #[cfg(target_os = "macos")]
        clear_quarantine(&normalized);
        self.grant_normalized(normalized)
    }

    /// Grant a path the user picked in a save dialog (it need not exist).
    pub fn grant_save_target(&self, path: impl AsRef<Path>) -> Result<String, ShellError> {
        self.grant_normalized(normalize_write_target(path)?)
    }

    pub fn grant_normalized(&self, path: PathBuf) -> Result<String, ShellError> {
        let display = path_to_string(&path);
        self.allowed.lock().map_err(poisoned)?.insert(path);
        Ok(display)
    }

    pub fn is_granted_normalized(&self, path: &Path) -> Result<bool, ShellError> {
        Ok(self.allowed.lock().map_err(poisoned)?.contains(path))
    }

    /// Canonicalise `path` and require it to be a granted existing file.
    pub fn require_file(&self, path: &str) -> Result<String, ShellError> {
        let normalized = normalize_existing_file(path)?;
        if self.is_granted_normalized(&normalized)? {
            Ok(path_to_string(&normalized))
        } else {
            Err(ShellError::BrokerDenied(format!(
                "path is not authorized; open it through {} first",
                self.app()
            )))
        }
    }

    pub fn require_write_target(&self, path: &str) -> Result<String, ShellError> {
        let normalized = normalize_write_target(path)?;
        if self.is_granted_normalized(&normalized)? {
            Ok(path_to_string(&normalized))
        } else {
            Err(ShellError::BrokerDenied(format!(
                "path is not authorized; choose it through {} first",
                self.app()
            )))
        }
    }

    /// Record a path the app just wrote; evicts the oldest past [`WRITE_TARGET_RING_CAP`].
    pub fn record_write_target(&self, path: &Path) -> Result<(), ShellError> {
        let mut ring = self.recent_writes.lock().map_err(poisoned)?;
        ring.retain(|p| p != path);
        ring.push_back(path.to_path_buf());
        while ring.len() > WRITE_TARGET_RING_CAP {
            ring.pop_front();
        }
        Ok(())
    }

    pub fn was_recently_written(&self, path: &Path) -> Result<bool, ShellError> {
        Ok(self
            .recent_writes
            .lock()
            .map_err(poisoned)?
            .iter()
            .any(|p| p == path))
    }

    /// Re-grant a save-as target that was just written (so the renderer cannot ask for any
    /// existing path to be granted).
    pub fn regrant_saved_target(&self, path: &str) -> Result<String, ShellError> {
        let normalized = normalize_write_target(path)?;
        if !self.was_recently_written(&normalized)? {
            return Err(ShellError::BrokerDenied(format!(
                "path was not just written by the app; save it through {} first",
                self.app()
            )));
        }
        self.grant_normalized(normalized)
    }

    /// Mint a one-shot recent-file id for a stored path (re-canonicalised: older rows or platform
    /// spelling differences must not grant something later checks reject).
    pub fn register_known_path(&self, path: &str) -> Result<String, ShellError> {
        let normalized = normalize_existing_file(path)?;
        let mut known = self.known.lock().map_err(poisoned)?;
        let id = mint_unique(&known.map, "known:")?;
        known.map.insert(id.clone(), normalized);
        known.order.push_back(id.clone());
        while known.order.len() > KNOWN_TOKEN_CAP {
            if let Some(old) = known.order.pop_front() {
                known.map.remove(&old);
            }
        }
        Ok(id)
    }

    /// Trade a recent-file id for a grant. One shot: replaying it fails.
    pub fn grant_known(&self, id: &str) -> Result<String, ShellError> {
        let normalized = self
            .known
            .lock()
            .map_err(poisoned)?
            .map
            .remove(id)
            .ok_or_else(|| {
                ShellError::BrokerDenied("unknown or already-used recent-file id".into())
            })?;
        self.grant_normalized(normalized)
    }

    /// Revoke a grant (its last tab closed) and every stream id for that file, so old stream URLs
    /// die now and never revive on reopen. Matches by display string, so a file deleted after
    /// opening is still revoked.
    pub fn revoke_path(&self, display: &str) -> Result<(), ShellError> {
        let mut revoked = Vec::new();
        self.allowed.lock().map_err(poisoned)?.retain(|p| {
            let keep = !display_matches(p, display);
            if !keep {
                revoked.push(p.clone());
            }
            keep
        });
        if !revoked.is_empty() {
            self.stream_tokens
                .lock()
                .map_err(poisoned)?
                .retain(|_, p| !revoked.iter().any(|r| r == p));
        }
        Ok(())
    }

    /// Opaque URL id for a granted file. One stable live id per file: re-reading after an atomic
    /// edit overlaps in-flight range requests, and rotating would 403 the URL still loading.
    pub fn grant_stream(&self, path: &str) -> Result<String, ShellError> {
        let normalized = normalize_existing_file(path)?;
        let allowed = self.allowed.lock().map_err(poisoned)?;
        if !allowed.contains(&normalized) {
            return Err(ShellError::BrokerDenied(format!(
                "path is not authorized; open it through {} first",
                self.app()
            )));
        }
        let mut streams = self.stream_tokens.lock().map_err(poisoned)?;
        if let Some((id, _)) = streams.iter().find(|(_, p)| *p == &normalized) {
            return Ok(id.clone());
        }
        let id = mint_unique(&streams, "stream-")?;
        streams.insert(id.clone(), normalized);
        Ok(id)
    }

    pub fn resolve_stream(&self, id: &str) -> Result<StreamTarget, ShellError> {
        let path = self
            .stream_tokens
            .lock()
            .map_err(poisoned)?
            .get(id)
            .cloned()
            .ok_or_else(|| ShellError::BrokerDenied("unknown or revoked stream id".into()))?;
        Ok(StreamTarget { path })
    }

    /// Opaque id for a directory the preview may read subresources from. Authorises only
    /// containment-checked reads through [`PathBroker::resolve_in_site_root`], never general
    /// document access. One id per grant (not deduplicated by directory) so two tabs on the same
    /// project do not revoke each other.
    pub fn grant_site_root(&self, dir: impl AsRef<Path>) -> Result<String, ShellError> {
        let dir = std::fs::canonicalize(dir.as_ref())?;
        if !dir.is_dir() {
            return Err(ShellError::Invalid("site root is not a directory".into()));
        }
        let mut roots = self.site_roots.lock().map_err(poisoned)?;
        let id = mint_unique(&roots, "site-")?;
        roots.insert(id.clone(), dir);
        Ok(id)
    }

    pub fn resolve_site_root(&self, id: &str) -> Result<PathBuf, ShellError> {
        self.site_roots
            .lock()
            .map_err(poisoned)?
            .get(id)
            .cloned()
            .ok_or_else(|| ShellError::BrokerDenied("unknown or revoked site root id".into()))
    }

    /// Resolve `relative` under a site root; refuses anything that escapes it (`..`, symlinks).
    pub fn resolve_in_site_root(&self, id: &str, relative: &str) -> Result<PathBuf, ShellError> {
        let root = self.resolve_site_root(id)?;
        let rel = relative.trim_start_matches('/');
        let escapes = rel.contains('\0')
            || Path::new(rel).components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir
                        | std::path::Component::Prefix(_)
                        | std::path::Component::RootDir
                )
            });
        if escapes {
            return Err(ShellError::BrokerDenied(
                "path escapes the site root".into(),
            ));
        }
        let full = std::fs::canonicalize(root.join(rel))?;
        if !full.starts_with(&root) {
            return Err(ShellError::BrokerDenied(
                "path escapes the site root".into(),
            ));
        }
        Ok(full)
    }

    pub fn revoke_site_root(&self, id: &str) -> Result<(), ShellError> {
        self.site_roots.lock().map_err(poisoned)?.remove(id);
        Ok(())
    }
}
