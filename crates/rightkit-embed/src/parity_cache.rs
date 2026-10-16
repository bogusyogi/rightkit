//! Opt-in cache of the E40 load-time vector parity verdict.
//!
//! The parity check builds the five graphs on the CPU provider and runs ten embeddings through
//! both providers, which dominates an `Auto` load. Its outcome depends only on the model files,
//! the ONNX Runtime build and the adapter, so a verdict stored under a key of those three is
//! reused until one of them changes. The caller names the directory
//! (`EmbedderConfig::with_parity_cache_dir`); nothing is written anywhere by default.
//!
//! One small JSON file per model directory, `e40-parity-<16 hex>.json`, replaced atomically:
//!
//! ```json
//! { "schema": 1,
//!   "key": { "model": "...", "runtime": "...", "adapter": "...", "floor": "0.995" },
//!   "verdict": { "passed": true, "lowest_cosine": 0.9998, "at_tokens": 190, "pairs": 10,
//!                "detail": "", "timestamp_unix": 1760000000 } }
//! ```
//!
//! Anything that does not parse, does not match the key or contradicts itself is a miss, never
//! an error: the check just runs again.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

const SCHEMA: u64 = 1;
/// A stored verdict, pass or fail, is trusted for 7 days, which bounds how long a GPU driver
/// change (not part of the key) can go unnoticed.
pub(crate) const MAX_AGE_SECS: u64 = 7 * 24 * 60 * 60;
/// Tolerated clock skew for a verdict stamped in the future.
const FUTURE_SKEW_SECS: u64 = 60 * 60;

/// What a verdict is valid for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParityKey {
    /// File names, sizes and modification times of the bundle files.
    pub model: String,
    /// ONNX Runtime library path, size and mtime, the sibling `DirectML.dll` stamp and ORT's
    /// build string (which carries the version).
    pub runtime: String,
    /// The provider name. ORT exposes no cheap adapter identity, so a GPU or driver change is
    /// not seen by the key.
    pub adapter: String,
    /// The parity floor the verdict was judged against.
    pub floor: String,
}

/// A stored parity outcome.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ParityVerdict {
    pub passed: bool,
    /// Lowest cosine; `None` when the failure was not a low cosine (non-finite output, ...).
    pub lowest_cosine: Option<f32>,
    /// Token count of the prompt that produced the lowest cosine (0 when unknown).
    pub at_tokens: usize,
    /// Pairs compared.
    pub pairs: usize,
    /// Failure text (empty for a pass).
    pub detail: String,
    pub timestamp_unix: u64,
}

/// Result of looking a verdict up.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Lookup {
    Hit(ParityVerdict),
    /// Why there is nothing usable.
    Miss(&'static str),
}

pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// `name:size:mtime_ns` of a file, or `name:absent`.
fn file_stamp(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |elapsed| elapsed.as_nanos());
            format!("{name}:{}:{mtime}", meta.len())
        }
        _ => format!("{name}:absent"),
    }
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

impl ParityKey {
    /// Key for `model_dir` (the `artifacts` files by name, size and mtime; the content hash of
    /// `Embedder::pipeline_id` is not used because hashing about a gigabyte at load would cost
    /// as much as the check saves), the runtime library and `provider`.
    pub(crate) fn build(
        model_dir: &Path,
        artifacts: &[&str],
        runtime_path: &Path,
        ort_build_info: &str,
        provider: &str,
        floor: f32,
    ) -> Self {
        let model = artifacts
            .iter()
            .map(|name| file_stamp(&model_dir.join(name)))
            .collect::<Vec<_>>()
            .join(";");
        let runtime_dir = runtime_path.parent().unwrap_or_else(|| Path::new(""));
        let runtime = format!(
            "{}|{}|{}|{}",
            canonical(runtime_path).display(),
            file_stamp(runtime_path),
            file_stamp(&runtime_dir.join("DirectML.dll")),
            ort_build_info
        );
        Self {
            model,
            runtime,
            adapter: provider.to_owned(),
            floor: format!("{floor}"),
        }
    }
}

/// The cache file for `model_dir` inside `cache_dir`.
pub(crate) fn cache_file(cache_dir: &Path, model_dir: &Path) -> PathBuf {
    let digest =
        rightkit_fs::digest::sha256_bytes(canonical(model_dir).to_string_lossy().as_bytes());
    cache_dir.join(format!("e40-parity-{}.json", &digest[..16]))
}

fn encode(key: &ParityKey, verdict: &ParityVerdict) -> Value {
    json!({
        "schema": SCHEMA,
        "key": {
            "model": key.model,
            "runtime": key.runtime,
            "adapter": key.adapter,
            "floor": key.floor,
        },
        "verdict": {
            "passed": verdict.passed,
            "lowest_cosine": verdict.lowest_cosine.map(f64::from),
            "at_tokens": verdict.at_tokens,
            "pairs": verdict.pairs,
            "detail": verdict.detail,
            "timestamp_unix": verdict.timestamp_unix,
        },
    })
}

fn decode(doc: &Value) -> Option<(ParityKey, ParityVerdict)> {
    if doc["schema"].as_u64()? != SCHEMA {
        return None;
    }
    let key = &doc["key"];
    let key = ParityKey {
        model: key["model"].as_str()?.to_owned(),
        runtime: key["runtime"].as_str()?.to_owned(),
        adapter: key["adapter"].as_str()?.to_owned(),
        floor: key["floor"].as_str()?.to_owned(),
    };
    let verdict = &doc["verdict"];
    let lowest_cosine = match &verdict["lowest_cosine"] {
        Value::Null => None,
        number => Some(number.as_f64()? as f32),
    };
    let verdict = ParityVerdict {
        passed: verdict["passed"].as_bool()?,
        lowest_cosine,
        at_tokens: usize::try_from(verdict["at_tokens"].as_u64()?).ok()?,
        pairs: usize::try_from(verdict["pairs"].as_u64()?).ok()?,
        detail: verdict["detail"].as_str()?.to_owned(),
        timestamp_unix: verdict["timestamp_unix"].as_u64()?,
    };
    Some((key, verdict))
}

/// A pass must carry a finite cosine at or above the floor of its own key and at least one
/// compared pair; a file that says otherwise was edited or damaged.
fn consistent(key: &ParityKey, verdict: &ParityVerdict) -> bool {
    if !verdict.passed {
        return true;
    }
    let Ok(floor) = key.floor.parse::<f32>() else {
        return false;
    };
    verdict.pairs > 0
        && verdict
            .lowest_cosine
            .is_some_and(|cosine| cosine.is_finite() && cosine >= floor)
}

fn parse(text: &str, key: &ParityKey, now: u64) -> Lookup {
    let Ok(doc) = serde_json::from_str::<Value>(text) else {
        return Lookup::Miss("cached verdict is corrupt");
    };
    let Some((stored_key, verdict)) = decode(&doc) else {
        return Lookup::Miss("cached verdict is corrupt");
    };
    if stored_key != *key {
        return Lookup::Miss("cached verdict is for a different model, runtime or adapter");
    }
    if !consistent(&stored_key, &verdict) {
        return Lookup::Miss("cached verdict is corrupt");
    }
    // Older than the limit, or stamped in the future beyond clock skew: do not trust it.
    if now.saturating_sub(verdict.timestamp_unix) > MAX_AGE_SECS
        || verdict.timestamp_unix > now.saturating_add(FUTURE_SKEW_SECS)
    {
        return Lookup::Miss("cached verdict has expired");
    }
    Lookup::Hit(verdict)
}

/// The verdict stored for `model_dir` in `cache_dir` if it matches `key` and is at most
/// [`MAX_AGE_SECS`] old at `now` (Unix seconds).
pub(crate) fn read(cache_dir: &Path, model_dir: &Path, key: &ParityKey, now: u64) -> Lookup {
    match std::fs::read_to_string(cache_file(cache_dir, model_dir)) {
        Ok(text) => parse(&text, key, now),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Lookup::Miss("no cached verdict")
        }
        Err(_) => Lookup::Miss("cached verdict is unreadable"),
    }
}

/// Stores `verdict` for `model_dir` (replacing any earlier one) through a temporary file in the
/// same directory and a rename, creating `cache_dir` if needed.
pub(crate) fn write(
    cache_dir: &Path,
    model_dir: &Path,
    key: &ParityKey,
    verdict: &ParityVerdict,
) -> std::io::Result<()> {
    std::fs::create_dir_all(cache_dir)?;
    let path = cache_file(cache_dir, model_dir);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let temp = path.with_extension(format!("tmp-{}-{nonce}", std::process::id()));
    let body =
        serde_json::to_string_pretty(&encode(key, verdict)).map_err(std::io::Error::other)?;
    std::fs::write(&temp, body)?;
    std::fs::rename(&temp, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A moment shortly after the test verdicts' timestamps (1_760_000_000 and ..001).
    const NOW: u64 = 1_760_000_100;

    #[test]
    fn a_verdict_expires_after_seven_days() {
        let (cache, model) = dirs();
        write(cache.path(), model.path(), &key(), &pass()).unwrap();
        let stamp = pass().timestamp_unix;
        let expired = Lookup::Miss("cached verdict has expired");
        // Exactly 7 days old still counts; one second more does not.
        assert_eq!(
            read(cache.path(), model.path(), &key(), stamp + MAX_AGE_SECS),
            Lookup::Hit(pass())
        );
        assert_eq!(
            read(cache.path(), model.path(), &key(), stamp + MAX_AGE_SECS + 1),
            expired
        );
        // A stored failure expires the same way.
        write(cache.path(), model.path(), &key(), &fail()).unwrap();
        let fail_stamp = fail().timestamp_unix;
        assert_eq!(
            read(
                cache.path(),
                model.path(),
                &key(),
                fail_stamp + MAX_AGE_SECS
            ),
            Lookup::Hit(fail())
        );
        assert_eq!(
            read(
                cache.path(),
                model.path(),
                &key(),
                fail_stamp + MAX_AGE_SECS + 1
            ),
            expired
        );
        // A stamp far in the future (clock set back, or edited) is not trusted either; a
        // little skew is tolerated.
        assert_eq!(
            read(
                cache.path(),
                model.path(),
                &key(),
                fail_stamp - FUTURE_SKEW_SECS
            ),
            Lookup::Hit(fail())
        );
        assert_eq!(
            read(
                cache.path(),
                model.path(),
                &key(),
                fail_stamp - FUTURE_SKEW_SECS - 1
            ),
            expired
        );
    }

    fn key() -> ParityKey {
        ParityKey {
            model: "model_32.onnx:10:1;model.onnx_data:20:2".into(),
            runtime: "C:/ort/onnxruntime.dll|onnxruntime.dll:5:6|DirectML.dll:7:8|ORT 1.24.4"
                .into(),
            adapter: "directml".into(),
            floor: "0.995".into(),
        }
    }

    fn pass() -> ParityVerdict {
        ParityVerdict {
            passed: true,
            // 1 - 2^-10 (0.9990234375) is exactly representable in f32, so the JSON round trip is exact.
            lowest_cosine: Some(1.0 - 1.0 / 1024.0),
            at_tokens: 190,
            pairs: 10,
            detail: String::new(),
            timestamp_unix: 1_760_000_000,
        }
    }

    fn fail() -> ParityVerdict {
        ParityVerdict {
            passed: false,
            lowest_cosine: Some(0.58),
            at_tokens: 0,
            pairs: 10,
            detail: "cosine 0.580 at ~20 tokens, floor 0.995".into(),
            timestamp_unix: 1_760_000_001,
        }
    }

    /// Two scratch directories: the cache and a model directory (only its path is used).
    fn dirs() -> (tempfile::TempDir, tempfile::TempDir) {
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap())
    }

    #[test]
    fn miss_when_nothing_is_stored() {
        let (cache, model) = dirs();
        assert_eq!(
            read(cache.path(), model.path(), &key(), NOW),
            Lookup::Miss("no cached verdict")
        );
        // A cache directory that does not exist yet is a miss, not an error.
        assert_eq!(
            read(&cache.path().join("absent"), model.path(), &key(), NOW),
            Lookup::Miss("no cached verdict")
        );
    }

    #[test]
    fn a_stored_pass_and_a_stored_fail_are_read_back() {
        let (cache, model) = dirs();
        write(cache.path(), model.path(), &key(), &pass()).unwrap();
        assert_eq!(
            read(cache.path(), model.path(), &key(), NOW),
            Lookup::Hit(pass())
        );
        write(cache.path(), model.path(), &key(), &fail()).unwrap();
        assert_eq!(
            read(cache.path(), model.path(), &key(), NOW),
            Lookup::Hit(fail())
        );
    }

    #[test]
    fn a_hand_written_verdict_file_is_a_hit() {
        let (cache, model) = dirs();
        let doc = json!({
            "schema": 1,
            "key": {
                "model": key().model,
                "runtime": key().runtime,
                "adapter": key().adapter,
                "floor": key().floor,
            },
            "verdict": {
                "passed": true,
                "lowest_cosine": 0.999_023_437_5,
                "at_tokens": 190,
                "pairs": 10,
                "detail": "",
                "timestamp_unix": 1_760_000_000u64,
            },
        });
        std::fs::write(
            cache_file(cache.path(), model.path()),
            serde_json::to_vec_pretty(&doc).unwrap(),
        )
        .unwrap();
        assert_eq!(
            read(cache.path(), model.path(), &key(), NOW),
            Lookup::Hit(pass())
        );
    }

    #[test]
    fn any_changed_key_part_is_a_miss() {
        let (cache, model) = dirs();
        write(cache.path(), model.path(), &key(), &pass()).unwrap();
        let mut changed = vec![key(), key(), key(), key()];
        changed[0].model.push_str(";tokenizer.json:1:1");
        changed[1].runtime = "C:/other/onnxruntime.dll|x|y|ORT 1.25.0".into();
        changed[2].adapter = "cpu".into();
        changed[3].floor = "0.99".into();
        for other in changed {
            assert_eq!(
                read(cache.path(), model.path(), &other, NOW),
                Lookup::Miss("cached verdict is for a different model, runtime or adapter"),
                "{other:?}"
            );
        }
        // The stored verdict is untouched and still valid for the original key.
        assert_eq!(
            read(cache.path(), model.path(), &key(), NOW),
            Lookup::Hit(pass())
        );
    }

    #[test]
    fn corrupt_or_contradictory_files_are_misses() {
        let (cache, model) = dirs();
        let path = cache_file(cache.path(), model.path());
        let corrupt = Lookup::Miss("cached verdict is corrupt");
        for text in [
            "",
            "not json",
            "{\"schema\": 1, \"key\": ",
            "[]",
            "{\"schema\": 2}",
            "{\"schema\": 1, \"key\": {}, \"verdict\": {}}",
        ] {
            std::fs::write(&path, text).unwrap();
            assert_eq!(
                read(cache.path(), model.path(), &key(), NOW),
                corrupt,
                "{text:?}"
            );
        }
        // A pass that does not meet its own floor, has no cosine or compared nothing.
        let mut doc = encode(&key(), &pass());
        doc["verdict"]["lowest_cosine"] = json!(0.5);
        std::fs::write(&path, doc.to_string()).unwrap();
        assert_eq!(read(cache.path(), model.path(), &key(), NOW), corrupt);
        doc["verdict"]["lowest_cosine"] = Value::Null;
        std::fs::write(&path, doc.to_string()).unwrap();
        assert_eq!(read(cache.path(), model.path(), &key(), NOW), corrupt);
        let mut doc = encode(&key(), &pass());
        doc["verdict"]["pairs"] = json!(0);
        std::fs::write(&path, doc.to_string()).unwrap();
        assert_eq!(read(cache.path(), model.path(), &key(), NOW), corrupt);
        // Bytes that are not UTF-8 cannot be read as text.
        std::fs::write(&path, [0xff, 0xfe, 0x00, 0x80]).unwrap();
        assert_eq!(
            read(cache.path(), model.path(), &key(), NOW),
            Lookup::Miss("cached verdict is unreadable")
        );
        // A good write replaces the damage.
        write(cache.path(), model.path(), &key(), &pass()).unwrap();
        assert_eq!(
            read(cache.path(), model.path(), &key(), NOW),
            Lookup::Hit(pass())
        );
    }

    #[test]
    fn write_leaves_one_file_per_model_directory_and_no_temp_files() {
        let (cache, model) = dirs();
        let other_model = tempfile::tempdir().unwrap();
        write(cache.path(), model.path(), &key(), &pass()).unwrap();
        write(cache.path(), model.path(), &key(), &fail()).unwrap();
        write(cache.path(), other_model.path(), &key(), &pass()).unwrap();
        assert_ne!(
            cache_file(cache.path(), model.path()),
            cache_file(cache.path(), other_model.path())
        );
        let names: Vec<String> = std::fs::read_dir(cache.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names
            .iter()
            .all(|name| name.starts_with("e40-parity-") && name.ends_with(".json")));
    }

    #[test]
    fn the_key_follows_file_size_and_runtime_stamps() {
        let model = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let dll = runtime.path().join("onnxruntime.dll");
        std::fs::write(model.path().join("a.onnx"), b"one").unwrap();
        std::fs::write(&dll, b"dll").unwrap();
        let build = |info: &str| {
            ParityKey::build(
                model.path(),
                &["a.onnx", "b.npy"],
                &dll,
                info,
                "directml",
                0.995,
            )
        };
        let first = build("ORT 1.24.4");
        assert!(first.model.contains("a.onnx:3:"), "{}", first.model);
        assert!(first.model.contains("b.npy:absent"), "{}", first.model);
        assert!(
            first.runtime.contains("DirectML.dll:absent"),
            "{}",
            first.runtime
        );
        assert_eq!(first.adapter, "directml");
        assert_eq!(first.floor, "0.995");
        assert_eq!(first, build("ORT 1.24.4"));
        assert_ne!(first, build("ORT 1.25.0"));
        std::fs::write(model.path().join("a.onnx"), b"longer").unwrap();
        assert_ne!(first, build("ORT 1.24.4"));
        let changed = build("ORT 1.24.4");
        std::fs::write(runtime.path().join("DirectML.dll"), b"dml").unwrap();
        assert_ne!(changed, build("ORT 1.24.4"));
    }
}
