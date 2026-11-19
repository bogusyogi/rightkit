//! Application icons for list rows, as PNG bytes or a `data:image/png;base64,...` URL.
//!
//! `AppIcons::data_url(process_name, exe_path?)` returns the icon of the application a row
//! names, or `None` when it cannot be resolved. "Not found" is never an error: the caller
//! falls back to its own placeholder.
//!
//! - Windows: the large shell icon of the executable (`SHGetFileInfoW`), rendered to BGRA
//!   through GDI and PNG-encoded in-process. When the row carries no `exe_path` (older
//!   rows), the image name is matched against running processes.
//! - macOS: `NSWorkspace iconForFile` on the app bundle path (or the running app whose
//!   name / bundle id matches), rendered into a bitmap this crate sizes itself (never an
//!   AppKit-made TIFF or PNG); see `mac.rs`.
//! - Other platforms: `None`.
//!
//! Windows resolution order when no `exe_path` is known (or it cannot be loaded):
//! persistent disk cache, running processes, `App Paths` registry, Start-menu shortcut
//! targets, then packaged (MSIX/AppX) apps via the per-user package repository +
//! `AppxManifest.xml` logos. Executables under `\WindowsApps\` prefer the manifest logo
//! over the shell icon.
//!
//! Results are cached in-process (bounded) and persisted as PNGs in
//! `<cache_dir>/<sha256(key)>.png` (keys: exe path and lowercased process name), so apps
//! that are not running keep their icon. Extraction is blocking: call it from a blocking
//! pool, never from a UI thread.
//!
//! The PNG encoder is intentionally dependency-free (stored deflate blocks): icons are at
//! most a few tens of KB raw. Every PNG the crate accepts or stores (package logos, cached files,
//! its own and the platform's output) goes through `limits::validate_png`, which inflates the
//! IDAT stream with a hard output cap equal to the exact decoded length the header implies
//! (`decode.rs`, using `miniz_oxide`).
//!
//! Untrusted inputs (package manifests, logo files, cache files, registry values, shell and
//! AppKit images) are read with byte caps, validated before anything is allocated for them, and
//! enumerated within an entry, depth and time budget; see `limits.rs` and the README limits
//! table. Any rejection is "no icon" and is reported as `icon_rejected` through the event sink.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

mod decode;
mod limits;
#[cfg(target_os = "macos")]
mod mac;
mod manifest;
mod png;
#[cfg(target_os = "windows")]
mod win;

pub use manifest::process_names_match;

/// Host inputs. [`IconConfig::new`] gives neutral defaults for everything but the
/// cache directory.
#[derive(Clone)]
pub struct IconConfig {
    /// Directory of the persistent PNG cache; `None` keeps results in memory only.
    pub cache_dir: Option<PathBuf>,
    /// How long a name-only miss (process not running right now) is remembered.
    /// Misses for a concrete `exe_path` are definitive and kept for the session.
    pub name_only_miss_ttl: Duration,
    /// In-memory entries kept; the memory cache is dropped when it grows past this.
    pub max_memory_entries: usize,
    /// Largest icon edge in pixels accepted anywhere: the Windows shell icon, a packaged logo,
    /// the macOS bitmap rep chosen, and a cached PNG. Clamped to `1..=1024`; width times height
    /// is additionally capped at 262,144 pixels. Larger images are "no icon".
    pub max_edge: i32,
    /// Windows: full executable path of a process id (host-supplied; the crate enumerates
    /// the ids). Ignored elsewhere.
    pub process_path_for_pid: fn(u32) -> Option<String>,
    /// Event sink `(kind, detail)`: `extract_panic` (a platform call panicked and was
    /// contained; detail is the process name), `disk_put_failed` (detail is the error) and
    /// `icon_rejected` (an input failed a limit or validation; detail is `<place>:<reason code>`,
    /// for example `manifest:oversize` or `cache:png_crc`, never a path or file contents).
    pub event_sink: fn(&str, &str),
}

impl IconConfig {
    /// `max_edge` as the unsigned limit every check uses.
    pub(crate) fn edge_limit(&self) -> u32 {
        self.max_edge.clamp(1, limits::HARD_MAX_EDGE as i32) as u32
    }

    pub fn new(cache_dir: Option<PathBuf>) -> Self {
        Self {
            cache_dir,
            name_only_miss_ttl: Duration::from_secs(30),
            max_memory_entries: 512,
            max_edge: 256,
            process_path_for_pid: |_| None,
            event_sink: |_, _| {},
        }
    }
}

struct CacheEntry {
    value: Option<Vec<u8>>,
    stored_at: Instant,
    /// True when the lookup had no concrete path, so a miss may be transient.
    name_only: bool,
}

/// Icon lookup with a bounded in-memory cache and a persistent PNG cache.
pub struct AppIcons {
    config: IconConfig,
    cache: Mutex<HashMap<String, CacheEntry>>,
}

impl AppIcons {
    pub fn new(config: IconConfig) -> Self {
        Self {
            config,
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn cache_get(&self, key: &str) -> Option<Option<Vec<u8>>> {
        let guard = self.cache.lock().ok()?;
        let entry = guard.get(key)?;
        if entry.value.is_none()
            && entry.name_only
            && entry.stored_at.elapsed() > self.config.name_only_miss_ttl
        {
            return None;
        }
        Some(entry.value.clone())
    }

    fn cache_put(&self, key: String, value: Option<Vec<u8>>, name_only: bool) {
        if let Ok(mut guard) = self.cache.lock() {
            if guard.len() >= self.config.max_memory_entries && !guard.contains_key(&key) {
                guard.clear();
            }
            guard.insert(
                key,
                CacheEntry {
                    value,
                    stored_at: Instant::now(),
                    name_only,
                },
            );
        }
    }

    /// `data:image/png;base64,...` for the application, or `None`. Blocking.
    pub fn data_url(&self, process_name: &str, exe_path: Option<&str>) -> Option<String> {
        self.png(process_name, exe_path)
            .map(|png| png_data_url(&png))
    }

    /// PNG bytes for the application, or `None`. Blocking.
    pub fn png(&self, process_name: &str, exe_path: Option<&str>) -> Option<Vec<u8>> {
        // Bound the host's strings before they reach a key, a hash, a registry call or a shell call.
        if !limits::input_str_ok(process_name) || !exe_path.is_none_or(limits::input_str_ok) {
            limits::report(self.config.event_sink, "input", limits::Reject::Oversize);
            return None;
        }
        let key = cache_key(process_name, exe_path)?;
        if let Some(hit) = self.cache_get(&key) {
            return hit;
        }
        let concrete = exe_path.map(str::trim).filter(|p| !p.is_empty());
        let result = self.resolve_png(process_name, concrete);
        self.cache_put(key, result.clone(), concrete.is_none());
        result
    }

    /// Disk cache, then platform resolution; successful resolutions are persisted
    /// under both the path key and the process-name key.
    fn resolve_png(&self, process_name: &str, concrete: Option<&str>) -> Option<Vec<u8>> {
        let dir = self.config.cache_dir.clone();
        let name_key = cache_key(process_name, None);
        if let Some(path) = concrete {
            let path_key = cache_key(process_name, Some(path))?;
            if let Some(png) = dir.as_deref().and_then(|d| self.disk_get(d, &path_key)) {
                return Some(png);
            }
            if let Some(png) = self.extract_icon_png(process_name, Some(path)) {
                self.persist(dir.as_deref(), &[Some(&path_key), name_key.as_ref()], &png);
                return Some(png);
            }
            // Path gone (moved / uninstalled): fall back to the name-keyed chain.
        }
        let name_key = name_key?;
        if let Some(png) = dir.as_deref().and_then(|d| self.disk_get(d, &name_key)) {
            return Some(png);
        }
        let png = self.extract_icon_png(process_name, None)?;
        self.persist(dir.as_deref(), &[Some(&name_key)], &png);
        Some(png)
    }

    fn persist(&self, dir: Option<&std::path::Path>, keys: &[Option<&String>], png: &[u8]) {
        if let Some(dir) = dir {
            for key in keys.iter().flatten() {
                if let Err(error) = disk_put(dir, key, png) {
                    (self.config.event_sink)("disk_put_failed", &error.to_string());
                }
            }
        }
    }

    /// Persistent-cache read: opened through `rightkit_fs::Roots` (no link followed, nothing
    /// outside the cache directory, a single hard link), size capped, then the whole PNG,
    /// its compressed data included, and its dimensions validated. Anything that fails is a miss (the caller re-extracts and the
    /// atomic write replaces the file under the same name).
    fn disk_get(&self, dir: &std::path::Path, key: &str) -> Option<Vec<u8>> {
        let sink = self.config.event_sink;
        let roots = rightkit_fs::Roots::with_roots([dir]).ok()?;
        let path = roots.roots().first()?.join(icon_cache_file_name(key));
        let options = limits::confined_read_options();
        let bytes =
            match limits::read_confined(&roots, &path, &options, limits::MAX_CACHE_PNG_BYTES) {
                Ok(bytes) => bytes,
                Err(reject) => {
                    limits::report(sink, "cache", reject);
                    return None;
                }
            };
        match limits::validate_png(
            &bytes,
            self.config.edge_limit(),
            limits::MAX_CACHE_PNG_BYTES,
        ) {
            Ok(_) => Some(bytes),
            Err(reject) => {
                limits::report(sink, "cache", reject);
                None
            }
        }
    }

    fn extract_icon_png(&self, process_name: &str, exe_path: Option<&str>) -> Option<Vec<u8>> {
        // Contain any platform-API panic: a bad icon must never take the app down.
        let found = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            platform_extract(&self.config, process_name, exe_path)
        })) {
            Ok(found) => found,
            Err(_) => {
                (self.config.event_sink)("extract_panic", process_name);
                return None;
            }
        }?;
        // One choke point for every platform and source: nothing leaves, or is persisted,
        // that does not pass the same validation a cache read applies.
        match limits::validate_png(
            &found,
            self.config.edge_limit(),
            limits::MAX_CACHE_PNG_BYTES,
        ) {
            Ok(_) => Some(found),
            Err(reject) => {
                limits::report(self.config.event_sink, "extract", reject);
                None
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn platform_extract(
    config: &IconConfig,
    process_name: &str,
    exe_path: Option<&str>,
) -> Option<Vec<u8>> {
    win::extract(config, process_name, exe_path)
}

#[cfg(target_os = "macos")]
fn platform_extract(
    config: &IconConfig,
    process_name: &str,
    exe_path: Option<&str>,
) -> Option<Vec<u8>> {
    mac::extract(config, process_name, exe_path)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn platform_extract(
    _config: &IconConfig,
    _process_name: &str,
    _exe_path: Option<&str>,
) -> Option<Vec<u8>> {
    None
}

/// Cache key: the executable / bundle path when present (case-folded on
/// Windows, whose paths are case-insensitive), else the lowercased process
/// name. `None` when neither is usable.
pub fn cache_key(process_name: &str, exe_path: Option<&str>) -> Option<String> {
    if let Some(path) = exe_path.map(str::trim).filter(|p| !p.is_empty()) {
        return Some(if cfg!(target_os = "windows") {
            format!("path:{}", path.to_lowercase())
        } else {
            format!("path:{path}")
        });
    }
    let name = process_name.trim();
    if name.is_empty() {
        return None;
    }
    Some(format!("name:{}", name.to_lowercase()))
}

/// Wrap PNG bytes as a data URL.
pub fn png_data_url(png: &[u8]) -> String {
    format!(
        "data:image/png;base64,{}",
        rightkit_crypto::base64_encode(png)
    )
}

/// Stable file name for a cache key: first 32 hex chars of SHA-256, `.png`.
pub fn icon_cache_file_name(key: &str) -> String {
    let digest = rightkit_fs::digest::sha256_bytes(key.as_bytes());
    format!("{}.png", &digest[..32])
}

fn disk_put(dir: &std::path::Path, key: &str, png: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let file = dir.join(icon_cache_file_name(key));
    let options = rightkit_fs::atomic::AtomicWriteOptions {
        create_parents: true,
        ..Default::default()
    };
    rightkit_fs::atomic::atomic_write_with_options(&file, options, |f| f.write_all(png)).map_err(
        |error| match error {
            rightkit_fs::FsError::Io { source, .. }
            | rightkit_fs::FsError::ReplacedButDirSyncFailed { source, .. } => source,
            other => std::io::Error::other(other.to_string()),
        },
    )
}
