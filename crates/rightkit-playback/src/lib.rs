//! Source-preserving compatibility playback preparation for Right Suite apps.
//!
//! Provenance: extracted from ScrapeRight `src-tauri/src/playback.rs`, decoupled
//! from its queue — the app supplies a [`PlaybackSource`] instead of a database
//! row, and FFmpeg resolution goes through `rightkit-media`.
//!
//! Containers are remuxed and audio is converted to AAC; video bytes are
//! copied. Unsupported video codecs remain explicit errors instead of silent
//! quality loss. Derivatives are cached by content fingerprint under a
//! caller-owned cache root; the source file is never modified.

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use rightkit_media::CancellationToken;
use rightkit_process::{OwnedCommand, WaitOutcome};

/// Audio containers the native webview players handle poorly but FFmpeg remuxes cleanly.
const AUDIO_EXTENSIONS: &[&str] = &["wav", "mp3", "m4a", "aac", "flac", "ogg", "opus"];
/// Known video containers win over any historical audio label.
const VIDEO_EXTENSIONS: &[&str] = &["mp4", "m4v", "mkv", "mov", "avi"];

const PREPARE_TIMEOUT: Duration = Duration::from_secs(600);
const PREPARE_POLL: Duration = Duration::from_millis(50);
const KILL_GRACE: Duration = Duration::from_secs(2);
const MAX_JOBS: usize = 4;
const MAX_PRE_CANCELLED: usize = 64;
const MIN_DERIVATIVE_BYTES: u64 = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaKind {
    Audio,
    Video,
}

impl MediaKind {
    /// Container-based detection: known video extensions win, then known audio
    /// extensions, then video by default so audio-only files are not remuxed
    /// into a video container they cannot fill.
    pub fn detect(source: &Path) -> Self {
        let ext = source
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase());
        match ext.as_deref() {
            Some(ext) if VIDEO_EXTENSIONS.contains(&ext) => Self::Video,
            Some(ext) if AUDIO_EXTENSIONS.contains(&ext) => Self::Audio,
            _ => Self::Video,
        }
    }

    fn output_extension(self) -> &'static str {
        match self {
            Self::Video => "mp4",
            Self::Audio => "m4a",
        }
    }
}

/// One media file an app wants a webview-compatible derivative of.
#[derive(Clone, Debug)]
pub struct PlaybackSource {
    /// Absolute path to the original media on disk (canonicalized by the host).
    pub path: PathBuf,
    /// `None` detects the kind from the container extension.
    pub kind: Option<MediaKind>,
}

impl PlaybackSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            kind: None,
        }
    }

    pub fn with_kind(mut self, kind: MediaKind) -> Self {
        self.kind = Some(kind);
        self
    }
}

#[derive(Debug)]
pub enum PlaybackError {
    Missing,
    Empty,
    Cancelled,
    Busy,
    Timeout,
    UnsupportedCodec,
    Runtime(String),
    Cache(String),
    InvalidRequest,
}

impl PlaybackError {
    /// Stable machine-readable codes shared with the frontend (`PLAYBACK_*`).
    pub fn code(&self) -> &'static str {
        match self {
            Self::Missing => "PLAYBACK_MISSING",
            Self::Empty => "PLAYBACK_CORRUPT",
            Self::Cancelled => "PLAYBACK_CANCELLED",
            Self::Busy => "PLAYBACK_BUSY",
            Self::Timeout => "PLAYBACK_TIMEOUT",
            Self::UnsupportedCodec => "PLAYBACK_UNSUPPORTED",
            Self::Runtime(_) => "PLAYBACK_RUNTIME",
            Self::Cache(_) => "PLAYBACK_CACHE",
            Self::InvalidRequest => "PLAYBACK_REQUEST",
        }
    }
}

impl fmt::Display for PlaybackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let detail = match self {
            Self::Missing => "saved media is no longer on disk".to_owned(),
            Self::Empty => "saved media is empty or unreadable".to_owned(),
            Self::Cancelled => "playback copy cancelled".to_owned(),
            Self::Busy => "another playback copy is being prepared".to_owned(),
            Self::Timeout => "compatibility preparation exceeded 10 minutes".to_owned(),
            Self::UnsupportedCodec => {
                "saved media is corrupt or its video codec cannot be remuxed for this OS".to_owned()
            }
            Self::Runtime(cause) | Self::Cache(cause) => cause.clone(),
            Self::InvalidRequest => "invalid playback request".to_owned(),
        };
        write!(f, "{}: {detail}", self.code())
    }
}

impl std::error::Error for PlaybackError {}

/// In-flight and early-arrived cancellation bookkeeping. Cancellation of a
/// request id may arrive before its preparation starts, so unknown ids are
/// remembered briefly.
#[derive(Default)]
pub struct PlaybackRegistry {
    state: Mutex<RegistryState>,
}

/// Active tokens and early cancels live under one lock so registration and
/// cancellation are a single atomic transition: a cancel either finds the
/// active token or is recorded for the `begin` that follows.
#[derive(Default)]
struct RegistryState {
    active: HashMap<String, CancellationToken>,
    pre_cancelled: VecDeque<String>,
}

impl PlaybackRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The state is only mutated in single, non-panicking steps, so a poisoned
    /// lock still holds consistent data and is recovered rather than propagated.
    fn lock_state(&self) -> MutexGuard<'_, RegistryState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Process-wide registry for apps that keep playback state in statics.
    pub fn shared() -> &'static Arc<Self> {
        static SHARED: OnceLock<Arc<PlaybackRegistry>> = OnceLock::new();
        SHARED.get_or_init(|| Arc::new(Self::new()))
    }

    pub fn valid_request_id(request_id: &str) -> bool {
        request_id.len() <= 100 && request_id.starts_with("playback-")
    }

    /// Best-effort cancellation; unknown ids are remembered so a preparation
    /// that starts after its cancel still stops before spawn.
    pub fn cancel(&self, request_id: &str) {
        if !Self::valid_request_id(request_id) {
            return;
        }
        let mut state = self.lock_state();
        if let Some(token) = state.active.get(request_id) {
            token.cancel();
        } else {
            state.pre_cancelled.push_back(request_id.to_owned());
            while state.pre_cancelled.len() > MAX_PRE_CANCELLED {
                state.pre_cancelled.pop_front();
            }
        }
    }

    /// Claim a request id or fail. The returned guard releases the id on drop.
    pub fn begin(self: &Arc<Self>, request_id: &str) -> Result<ActiveRequest, PlaybackError> {
        if !Self::valid_request_id(request_id) {
            return Err(PlaybackError::InvalidRequest);
        }
        let token = CancellationToken::new();
        {
            let mut state = self.lock_state();
            if let Some(i) = state.pre_cancelled.iter().position(|id| id == request_id) {
                state.pre_cancelled.remove(i);
                return Err(PlaybackError::Cancelled);
            }
            if state.active.contains_key(request_id) || state.active.len() >= MAX_JOBS {
                return Err(PlaybackError::Busy);
            }
            state.active.insert(request_id.to_owned(), token.clone());
        }
        Ok(ActiveRequest {
            registry: Arc::clone(self),
            request_id: request_id.to_owned(),
            token,
        })
    }
}

/// Claimed request id. Owns its cancellation token and releases the slot on
/// drop; safe to move into a blocking task.
pub struct ActiveRequest {
    registry: Arc<PlaybackRegistry>,
    request_id: String,
    token: CancellationToken,
}

impl ActiveRequest {
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }
}

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        self.registry.lock_state().active.remove(&self.request_id);
    }
}

fn fingerprint(source: &Path, video: bool) -> Result<String, PlaybackError> {
    let meta = source.metadata().map_err(|_| PlaybackError::Missing)?;
    if !meta.is_file() || meta.len() == 0 {
        return Err(PlaybackError::Empty);
    }
    let modified = meta
        .modified()
        .map_err(|e| PlaybackError::Runtime(e.to_string()))?
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|e| PlaybackError::Runtime(e.to_string()))?;
    Ok(fingerprint_of(
        &source.as_os_str().to_string_lossy(),
        meta.len(),
        modified.as_nanos(),
        video,
    ))
}

fn fingerprint_of(path: &str, len: u64, modified_nanos: u128, video: bool) -> String {
    let mut material = Vec::with_capacity(path.len() + 8 + 16 + 1);
    material.extend_from_slice(path.as_bytes());
    material.extend_from_slice(&len.to_le_bytes());
    material.extend_from_slice(&modified_nanos.to_le_bytes());
    material.push(u8::from(video));
    rightkit_fs::digest::sha256_bytes(&material)
}

/// Remux to a webview-safe container: video bytes are copied (`-c:v copy`),
/// audio becomes AAC 192k, and the result gets `+faststart`. The source is
/// only read, never written.
pub fn compatibility_args(source: &Path, target: &Path, kind: MediaKind) -> Vec<OsString> {
    let mut args = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-y".into(),
        "-i".into(),
        source.as_os_str().into(),
    ];
    match kind {
        MediaKind::Video => {
            args.extend(["-map", "0:v:0", "-map", "0:a:0?", "-c:v", "copy"].map(Into::into));
        }
        MediaKind::Audio => {
            args.extend(["-map", "0:a:0", "-vn"].map(Into::into));
        }
    }
    args.extend(["-c:a", "aac", "-b:a", "192k", "-movflags", "+faststart"].map(Into::into));
    args.push(target.as_os_str().into());
    args
}

struct Scratch {
    file: PathBuf,
    log: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.file);
        let _ = fs::remove_file(&self.log);
    }
}

/// Prepare (or reuse) a cached compatibility derivative of `source`. Returns
/// the derivative path under `cache_root`; callers decide how to expose it to
/// the webview (e.g. Tauri asset-protocol scoping).
///
/// `ffmpeg` is the binary resolved by the host, typically via
/// `rightkit-media`'s verified artifact resolution. Cancellation is checked
/// before spawn, during the run, and before the result is committed.
pub fn compatibility_copy(
    ffmpeg: &Path,
    source: &PlaybackSource,
    cache_root: &Path,
    cancel: &CancellationToken,
) -> Result<PathBuf, PlaybackError> {
    if cancel.is_cancelled() {
        return Err(PlaybackError::Cancelled);
    }
    let path = source
        .path
        .canonicalize()
        .map_err(|_| PlaybackError::Missing)?;
    let kind = source.kind.unwrap_or_else(|| MediaKind::detect(&path));
    let video = kind == MediaKind::Video;
    let key = fingerprint(&path, video)?;
    fs::create_dir_all(cache_root).map_err(|e| PlaybackError::Cache(e.to_string()))?;
    let ext = kind.output_extension();
    let output = cache_root.join(format!("{key}.{ext}"));
    if output
        .metadata()
        .is_ok_and(|meta| meta.len() > MIN_DERIVATIVE_BYTES)
    {
        return Ok(output);
    }
    static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let stem = format!("{key}-{}-{serial}", std::process::id());
    let scratch = Scratch {
        file: cache_root.join(format!("{stem}.partial.{ext}")),
        log: cache_root.join(format!("{stem}.log")),
    };
    if cancel.is_cancelled() {
        return Err(PlaybackError::Cancelled);
    }
    let log = fs::File::create(&scratch.log).map_err(|e| PlaybackError::Cache(e.to_string()))?;
    let mut command = OwnedCommand::new(ffmpeg.as_os_str());
    command
        .command_mut()
        .args(compatibility_args(&path, &scratch.file, kind))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    command.windows_hide();
    let mut child = command
        .spawn()
        .map_err(|e| PlaybackError::Runtime(e.to_string()))?;
    let start = Instant::now();
    let status = loop {
        if cancel.is_cancelled() || start.elapsed() > PREPARE_TIMEOUT {
            let status = child
                .wait_or_kill(KILL_GRACE)
                .map_err(|e| PlaybackError::Runtime(e.to_string()))?;
            let _ = match status {
                WaitOutcome::Exited(status) | WaitOutcome::Terminated(status) => status,
            };
            return Err(if cancel.is_cancelled() {
                PlaybackError::Cancelled
            } else {
                PlaybackError::Timeout
            });
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(PREPARE_POLL),
            Err(e) => {
                let _ = child.terminate_tree();
                return Err(PlaybackError::Runtime(e.to_string()));
            }
        }
    };
    if !status.success()
        || !scratch
            .file
            .metadata()
            .is_ok_and(|meta| meta.len() > MIN_DERIVATIVE_BYTES)
    {
        return Err(PlaybackError::UnsupportedCodec);
    }
    if cancel.is_cancelled() {
        return Err(PlaybackError::Cancelled);
    }
    // Parallel requests may finish the same immutable derivative. Keep first.
    if !output.is_file() {
        if let Err(error) = fs::rename(&scratch.file, &output) {
            if !output
                .metadata()
                .is_ok_and(|meta| meta.len() > MIN_DERIVATIVE_BYTES)
            {
                return Err(PlaybackError::Cache(error.to_string()));
            }
        }
    }
    Ok(output)
}

/// Resolve the FFmpeg binary the host should pass to [`compatibility_copy`].
/// Explicit override and bundled paths win; `development_path` requires the
/// caller's explicit opt-in. Ambient `PATH` lookup is intentionally absent —
/// apps that ship a verified artifact should use `rightkit_media::resolve_ffmpeg`
/// + `rightkit_media::verify_ffmpeg` instead and pass `.path()` here.
#[derive(Clone, Debug, Default)]
pub struct FfmpegCandidates {
    pub override_path: Option<PathBuf>,
    pub bundled_paths: Vec<PathBuf>,
    pub development_path: Option<PathBuf>,
    pub allow_development_path: bool,
}

impl FfmpegCandidates {
    pub fn resolve(&self) -> Result<PathBuf, PlaybackError> {
        if let Some(path) = self
            .override_path
            .iter()
            .chain(self.bundled_paths.iter())
            .find(|path| path.is_file())
        {
            return Ok(path.clone());
        }
        if let Some(path) = self.development_path.as_ref().filter(|path| path.is_file()) {
            if !self.allow_development_path {
                return Err(PlaybackError::Runtime(
                    "development FFmpeg path requires explicit opt-in".into(),
                ));
            }
            return Ok(path.clone());
        }
        Err(PlaybackError::Runtime("installed FFmpeg is missing".into()))
    }
}

#[cfg(feature = "tauri")]
pub mod tauri_commands {
    //! Optional Tauri command helpers. The app owns its media lookup: implement
    //! [`PlaybackHost`] and register it inside [`PlaybackState`] via
    //! `.manage()` so `prepare_media_playback`/`cancel_media_playback` can be
    //! registered as commands.

    use super::*;

    /// App-supplied bridge between a media id and on-disk state.
    pub trait PlaybackHost: Send + Sync + 'static {
        /// Map a media id to its source file and kind.
        fn source(&self, id: &str) -> Result<PlaybackSource, String>;
        /// Cache root for derivatives (e.g. `app_cache_dir()/playback-v1`).
        fn cache_dir(&self) -> Result<PathBuf, String>;
        /// Resolved FFmpeg binary (e.g. bundled runtime path or
        /// rightkit-media verified artifact).
        fn ffmpeg(&self) -> Result<PathBuf, String>;
        /// Expose the finished derivative to the webview (asset-protocol scope).
        fn allow_asset(&self, path: &Path) -> Result<(), String>;
    }

    pub struct PlaybackState {
        host: Arc<dyn PlaybackHost>,
        registry: Arc<PlaybackRegistry>,
    }

    impl PlaybackState {
        pub fn new(host: Arc<dyn PlaybackHost>) -> Self {
            Self {
                host,
                registry: Arc::new(PlaybackRegistry::new()),
            }
        }
    }

    #[tauri::command]
    pub fn cancel_media_playback(request_id: String, state: tauri::State<'_, PlaybackState>) {
        state.registry.cancel(&request_id);
    }

    #[tauri::command]
    pub async fn prepare_media_playback(
        id: String,
        request_id: String,
        state: tauri::State<'_, PlaybackState>,
    ) -> Result<String, String> {
        let source = state.host.source(&id)?;
        let ffmpeg = state.host.ffmpeg()?;
        let root = state.host.cache_dir()?;
        let request = state
            .registry
            .begin(&request_id)
            .map_err(|e| e.to_string())?;
        let path = tauri::async_runtime::spawn_blocking(move || {
            let _request = request;
            compatibility_copy(&ffmpeg, &source, &root, _request.token())
        })
        .await
        .map_err(|e| format!("PLAYBACK_PREPARE: {e}"))?
        .map_err(|e| e.to_string())?;
        state.host.allow_asset(&path)?;
        Ok(path.to_string_lossy().into_owned())
    }
}

#[cfg(test)]
mod fingerprint_tests {
    use super::fingerprint_of;

    #[test]
    fn fingerprint_matches_the_prior_sha256_layout() {
        // SHA-256 of path bytes || len (u64 LE) || mtime nanos (u128 LE) || video flag,
        // computed independently of this crate.
        assert_eq!(
            fingerprint_of("/a/b.mp4", 1234, 1_700_000_000_123_456_789, true),
            "39fa9637f8b9e7a278274cbd221931765587b7125f626096470edbc04611e5af"
        );
    }
}
