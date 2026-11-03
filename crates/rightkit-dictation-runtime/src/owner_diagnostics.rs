use serde_json::Value;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;

// Test-mode only (release switch rule S1): a release build turns capture on through the on-disk
// marker below and never reads, or contains, these names.
/// Absolute directory to write owner diagnostics into. Raw WAV capture is by far
/// the heaviest thing this app writes, and the default lives on the boot volume;
/// pointing it at an external disk keeps long debugging sessions from filling it.
/// Marker file in the app-data root that turns capture on and KEEPS it on.
///
/// An env var alone lives only in the process the owner launched from a
/// terminal, so a relaunch, in-app update, or open-from-Applications would
/// silently drop capture. A marker file survives all of those: presence = on,
/// deleting it = off. Still owner-only: nothing in the UI creates it.
///
/// An optional absolute path inside the file overrides the destination
/// directory, so a large capture run can be aimed at another disk without
/// re-launching from a shell.
const OWNER_DIAGNOSTICS_MARKER: &str = "owner-diagnostics.enabled";
const SAMPLE_RATE: u32 = 16_000;

enum WriteJob {
    Audio {
        session_id: String,
        captured_at_ms: u64,
        samples: Vec<f32>,
    },
    Event(Value),
}

static WRITER_TX: OnceLock<Sender<WriteJob>> = OnceLock::new();

pub fn enabled() -> bool {
    legacy_enabled()
        || crate::settings::diagnostic_audio_capture()
        || crate::settings::diagnostic_unredacted_logs()
}

fn legacy_enabled() -> bool {
    enabled_from(rightkit_dictation_core::dev_var_os!(OWNER_DIAGNOSTICS_ENV).as_deref())
        || marker_path().is_file()
}

/// Path of the persistent opt-in marker.
fn marker_path() -> PathBuf {
    crate::settings::app_data_root().join(OWNER_DIAGNOSTICS_MARKER)
}

/// Destination override recorded inside the marker file, if any. Absolute paths
/// only, for the same reason the env override demands one: the sidecar's working
/// directory is not the owner's shell.
fn marker_dir_override() -> Option<PathBuf> {
    let body = fs::read_to_string(marker_path()).ok()?;
    let line = body.lines().find(|l| !l.trim().is_empty())?.trim();
    let path = PathBuf::from(line);
    path.is_absolute().then_some(path)
}

fn enabled_from(value: Option<&OsStr>) -> bool {
    value.and_then(OsStr::to_str).is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        )
    })
}

fn root() -> PathBuf {
    // Precedence: explicit env var for THIS process, then the on-disk marker,
    // then the default. The env var is a deliberate per-launch choice (tests,
    // one-off captures to a scratch dir) and must not be silently hijacked by
    // ambient machine state — the marker's job is to survive relaunches that
    // carry no environment, not to override launches that set one.
    resolve_root(
        rightkit_dictation_core::dev_var_os!(OWNER_DIAGNOSTICS_DIR_ENV).as_deref(),
        || {
            marker_dir_override()
                .unwrap_or_else(|| crate::settings::app_data_root().join("diagnostic-captures"))
        },
    )
}

/// Resolve the diagnostics directory from an optional override.
///
/// The override must be an ABSOLUTE path: this runs in the sidecar, whose working
/// directory is not the user's shell, so a relative path would land somewhere the
/// user did not intend and did not expect. Anything blank or relative falls back
/// to the default rather than guessing.
fn resolve_root(override_dir: Option<&OsStr>, default: impl FnOnce() -> PathBuf) -> PathBuf {
    let candidate = override_dir
        .map(Path::new)
        .filter(|path| !path.as_os_str().is_empty());
    match candidate {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => {
            tracing::warn!(
                path = %path.display(),
                "diagnostics directory override must be an absolute path; using the default location"
            );
            default()
        }
        None => default(),
    }
}

fn writer() -> &'static Sender<WriteJob> {
    WRITER_TX.get_or_init(|| {
        let (tx, rx) = channel::<WriteJob>();
        let spawned = std::thread::Builder::new()
            .name("hr-owner-diagnostics".to_string())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let result = match job {
                        WriteJob::Audio {
                            session_id,
                            captured_at_ms,
                            samples,
                        } => write_wav_atomic(&root(), &session_id, captured_at_ms, &samples)
                            .map(|_| ()),
                        WriteJob::Event(payload) => append_jsonl(&root(), &payload),
                    };
                    if let Err(error) = result {
                        tracing::warn!(%error, "owner diagnostics write failed");
                    }
                }
            })
            .is_ok();
        if !spawned {
            tracing::warn!("owner diagnostics writer failed to spawn");
        }
        tx
    })
}

pub fn capture_audio_parts(session_id: &str, samples: &[f32], tail: &[f32]) {
    if !legacy_enabled() && !crate::settings::diagnostic_audio_capture() {
        return;
    }
    let mut complete = Vec::with_capacity(samples.len() + tail.len());
    complete.extend_from_slice(samples);
    complete.extend_from_slice(tail);
    let _ = writer().send(WriteJob::Audio {
        session_id: session_id.to_string(),
        captured_at_ms: now_ms(),
        samples: complete,
    });
}

pub fn record_event(mut payload: Value) {
    if !legacy_enabled() && !crate::settings::diagnostic_unredacted_logs() {
        return;
    }
    if let Some(object) = payload.as_object_mut() {
        object
            .entry("ts_ms".to_string())
            .or_insert_with(|| Value::from(now_ms()));
    }
    let _ = writer().send(WriteJob::Event(payload));
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn sanitized_session_id(session_id: &str) -> String {
    session_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn write_wav_atomic(
    root: &Path,
    session_id: &str,
    captured_at_ms: u64,
    samples: &[f32],
) -> Result<PathBuf, String> {
    let audio_dir = root.join("audio");
    fs::create_dir_all(&audio_dir).map_err(|error| error.to_string())?;
    let stem = format!(
        "{captured_at_ms}-{}-{}",
        std::process::id(),
        sanitized_session_id(session_id)
    );
    let final_path = audio_dir.join(format!("{stem}.wav"));
    let temp_path = audio_dir.join(format!(".{stem}.wav.tmp"));
    rightkit_audio::wav::write_wav_mono_i16(&temp_path, SAMPLE_RATE, samples)
        .map_err(|error| error.to_string())?;
    rightkit_fs::atomic::publish_file(&temp_path, &final_path)
        .map_err(|error| error.to_string())?;
    Ok(final_path)
}

fn append_jsonl(root: &Path, payload: &Value) -> Result<(), String> {
    fs::create_dir_all(root).map_err(|error| error.to_string())?;
    let path = root.join("transcripts.jsonl");
    let mut line = serde_json::to_vec(payload).map_err(|error| error.to_string())?;
    line.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.write_all(&line).map_err(|error| error.to_string())
}
