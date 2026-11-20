//! C surface for streaming keyword spotting on [`crate::kws`] (feature `kws-ffi`).
//!
//! The entry points mirror the Sherpa-ONNX C API calls a two-stage keyword cascade
//! makes (`SherpaOnnxCreateKeywordSpotter`, `SherpaOnnxCreateKeywordStream`,
//! `SherpaOnnxOnlineStreamAcceptWaveform`, `SherpaOnnxIsKeywordStreamReady`,
//! `SherpaOnnxDecodeKeywordStream`, `SherpaOnnxGetKeywordResultAsJson`), so a cascade
//! policy ports onto this runtime call for call. Every spotter uses one CPU thread,
//! no trailing blanks, keyword score 1.0 and no automatic trailing-blank reset. The
//! beam width, keyword threshold and active-path expiry are caller input to
//! `rightkit_asr_kws_spotter_create`.
//!
//! ONNX Runtime is bound once per process with `rightkit_asr_kws_runtime_init`
//! before any spotter is created: on iOS from a statically linked
//! `onnxruntime.xcframework`, elsewhere from a runtime library path.
//!
//! No entry point unwinds into its caller: each one catches panics and reports them
//! as a failed call. Each handle guards its state with a lock, but callers still
//! serialise use of one spotter and its streams, as with Sherpa. Strings returned by
//! this library (results, error messages) are released with
//! `rightkit_asr_string_free`. The C declarations are in `include/rightkit_asr_kws.h`.

use std::any::Any;
use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::kws::{
    KeywordResult, KeywordSpotter, KeywordSpotterConfig, KeywordStream, Keywords,
};

/// Model files inside the published zipformer KWS payload directory.
pub const ENCODER_FILE: &str = "encoder-epoch-13-avg-2-chunk-16-left-64.int8.onnx";
pub const DECODER_FILE: &str = "decoder-epoch-13-avg-2-chunk-16-left-64.onnx";
pub const JOINER_FILE: &str = "joiner-epoch-13-avg-2-chunk-16-left-64.int8.onnx";
pub const TOKENS_FILE: &str = "tokens.txt";

/// The only rate the feature extractor accepts; the cascade feeds 16 kHz mono.
const SAMPLE_RATE: u32 = 16_000;
// Fixed settings of every spotter created here.
const THREADS: usize = 1;
const NUM_TRAILING_BLANKS: i32 = 0;
const KEYWORDS_SCORE: f32 = 1.0;

/// Opaque spotter handle: the three model sessions and the keyword graph.
pub struct RightKitAsrKwsSpotter(Mutex<KeywordSpotter>);

/// Opaque stream handle: one audio stream's features, encoder cache and beam.
pub struct RightKitAsrKwsStream(Mutex<KeywordStream>);

#[derive(Debug, Clone, PartialEq, Eq)]
enum RuntimeSource {
    /// ONNX Runtime linked into the process image (iOS `onnxruntime.xcframework`).
    Static,
    /// ONNX Runtime loaded from this library file.
    Library(PathBuf),
}

/// The ONNX Runtime binding this process committed. Set only after a binding
/// succeeds. A static API installed before environment failure is tracked separately;
/// that irreversible installation permits only a retry of the same static source.
static RUNTIME: Mutex<Option<RuntimeSource>> = Mutex::new(None);

// Separate from RUNTIME: installing ort's API is irreversible even if creating
// its environment subsequently fails. Access is serialized by the RUNTIME lock.
#[cfg(target_os = "ios")]
static STATIC_API_INSTALLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Bind ONNX Runtime for every spotter in this process. NULL asks for the process
/// default: the runtime already bound here, else the one statically linked into the
/// process (iOS only). Otherwise pass the absolute path of an ONNX Runtime shared
/// library; repeating it succeeds without rebinding, and a different library fails
/// because a loaded runtime cannot be replaced.
///
/// Returns 0 on success. On failure returns -1 and, when `error_out` is not null,
/// stores a message released with `rightkit_asr_string_free`.
///
/// # Safety
///
/// `ort_library_path` must be null or a valid NUL-terminated C string, and
/// `error_out` must be null or point to a writable `char *`, both valid for the call.
#[no_mangle]
pub unsafe extern "C" fn rightkit_asr_kws_runtime_init(
    ort_library_path: *const c_char,
    error_out: *mut *mut c_char,
) -> i32 {
    unsafe { clear_error(error_out) };
    let bound = guarded(|| {
        let requested = if ort_library_path.is_null() {
            None
        } else {
            Some(PathBuf::from(unsafe {
                utf8_arg(ort_library_path, "ort_library_path")
            }?))
        };
        bind_runtime(requested)
    });
    match bound {
        Ok(()) => 0,
        Err(message) => {
            unsafe { write_error(error_out, &message) };
            -1
        }
    }
}

/// Create a spotter over `model_dir` (encoder, decoder, joiner and `tokens.txt` with
/// the payload's file names) and the keyword list `keywords_file` (relative to
/// `model_dir` unless absolute). `keywords_threshold` applies to keyword lines with
/// no `#threshold`; `max_active_paths` is the beam width (at least 1);
/// `active_path_ttl_frames` drops a beam path that stays active that many encoder
/// frames (40 ms each), and 0 or a negative value disables the expiry.
///
/// Returns null on failure and, when `error_out` is not null, stores a message
/// released with `rightkit_asr_string_free`. Release the spotter with
/// `rightkit_asr_kws_spotter_destroy`.
///
/// # Safety
///
/// `model_dir` and `keywords_file` must be null or valid NUL-terminated C strings,
/// and `error_out` must be null or point to a writable `char *`, all valid for the call.
#[no_mangle]
pub unsafe extern "C" fn rightkit_asr_kws_spotter_create(
    model_dir: *const c_char,
    keywords_file: *const c_char,
    keywords_threshold: f32,
    max_active_paths: i32,
    active_path_ttl_frames: i32,
    error_out: *mut *mut c_char,
) -> *mut RightKitAsrKwsSpotter {
    unsafe { clear_error(error_out) };
    let created = guarded(|| {
        let model_dir = PathBuf::from(unsafe { utf8_arg(model_dir, "model_dir") }?);
        let keywords = model_dir.join(unsafe { utf8_arg(keywords_file, "keywords_file") }?);
        let config = spotter_config(
            &model_dir,
            keywords,
            keywords_threshold,
            max_active_paths,
            active_path_ttl_frames,
        )?;
        runtime_ready()?;
        KeywordSpotter::new(&config).map_err(|error| error.to_string())
    });
    match created {
        Ok(spotter) => Box::into_raw(Box::new(RightKitAsrKwsSpotter(Mutex::new(spotter)))),
        Err(message) => {
            unsafe { write_error(error_out, &message) };
            null_mut()
        }
    }
}

/// Release a spotter. Null is ignored.
///
/// # Safety
///
/// `spotter` must be null or a pointer returned by `rightkit_asr_kws_spotter_create`
/// that is not used again and not released twice.
#[no_mangle]
pub unsafe extern "C" fn rightkit_asr_kws_spotter_destroy(
    spotter: *mut RightKitAsrKwsSpotter,
) {
    if !spotter.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(unsafe { Box::from_raw(spotter) })));
    }
}

/// Create a fresh stream for `spotter` (Sherpa `CreateKeywordStream`). Returns null
/// on failure. Release it with `rightkit_asr_kws_stream_destroy`.
///
/// # Safety
///
/// `spotter` must be null or a live pointer from `rightkit_asr_kws_spotter_create`.
#[no_mangle]
pub unsafe extern "C" fn rightkit_asr_kws_stream_create(
    spotter: *mut RightKitAsrKwsSpotter,
) -> *mut RightKitAsrKwsStream {
    guarded(|| {
        let spotter = unsafe { handle(spotter, "spotter") }?;
        Ok(lock(&spotter.0)?.create_stream())
    })
    .map_or(null_mut(), |stream| {
        Box::into_raw(Box::new(RightKitAsrKwsStream(Mutex::new(stream))))
    })
}

/// Release a stream. Null is ignored.
///
/// # Safety
///
/// `stream` must be null or a pointer returned by `rightkit_asr_kws_stream_create`
/// that is not used again and not released twice.
#[no_mangle]
pub unsafe extern "C" fn rightkit_asr_kws_stream_destroy(stream: *mut RightKitAsrKwsStream) {
    if !stream.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(unsafe { Box::from_raw(stream) })));
    }
}

/// Append `count` mono 16 kHz samples in `[-1, 1]` (Sherpa `AcceptWaveform` at
/// 16 kHz). Zero samples is a no-op. Returns 0 on success, -1 on failure.
///
/// # Safety
///
/// `stream` must be null or a live stream pointer, and `samples` must be null (only
/// with `count == 0`) or point to `count` readable `f32` values.
#[no_mangle]
pub unsafe extern "C" fn rightkit_asr_kws_stream_accept(
    stream: *mut RightKitAsrKwsStream,
    samples: *const f32,
    count: i32,
) -> i32 {
    status(guarded(|| {
        let stream = unsafe { handle(stream, "stream") }?;
        let count = usize::try_from(count).map_err(|_| format!("negative sample count {count}"))?;
        let samples = if count == 0 {
            &[][..]
        } else if samples.is_null() {
            return Err("samples is null".into());
        } else {
            unsafe { std::slice::from_raw_parts(samples, count) }
        };
        lock(&stream.0)?
            .accept_waveform(SAMPLE_RATE, samples)
            .map_err(|error| error.to_string())
    }))
}

/// 1 when `stream` holds a full encoder chunk beyond what was decoded, 0 when not,
/// -1 on failure (Sherpa `IsKeywordStreamReady`).
///
/// # Safety
///
/// `spotter` and `stream` must each be null or a live pointer from this library.
#[no_mangle]
pub unsafe extern "C" fn rightkit_asr_kws_stream_is_ready(
    spotter: *mut RightKitAsrKwsSpotter,
    stream: *mut RightKitAsrKwsStream,
) -> i32 {
    match guarded(|| {
        let (spotter, stream) = unsafe { (handle(spotter, "spotter")?, handle(stream, "stream")?) };
        let spotter = lock(&spotter.0)?;
        let stream = lock(&stream.0)?;
        Ok(spotter.is_ready(&stream))
    }) {
        Ok(true) => 1,
        Ok(false) => 0,
        Err(_) => -1,
    }
}

/// Decode one encoder chunk (Sherpa `DecodeKeywordStream`). Call while
/// `rightkit_asr_kws_stream_is_ready` returns 1. Returns 0 on success, -1 on
/// failure, including a stream created by another spotter.
///
/// # Safety
///
/// `spotter` and `stream` must each be null or a live pointer from this library.
#[no_mangle]
pub unsafe extern "C" fn rightkit_asr_kws_stream_decode(
    spotter: *mut RightKitAsrKwsSpotter,
    stream: *mut RightKitAsrKwsStream,
) -> i32 {
    status(guarded(|| {
        let (spotter, stream) = unsafe { (handle(spotter, "spotter")?, handle(stream, "stream")?) };
        let mut spotter = lock(&spotter.0)?;
        let mut stream = lock(&stream.0)?;
        spotter
            .decode(&mut stream)
            .map_err(|error| error.to_string())
    }))
}

/// The newest detection as Sherpa `GetKeywordResultAsJson` reports it:
/// `{"keyword": "...", "timestamps": [0.28, ...], "tokens": [...]}` with timestamps
/// in seconds printed to two decimals. A detection is reported once; later calls
/// return an empty `keyword` until a newer detection. Returns null on failure;
/// release the string with `rightkit_asr_string_free`.
///
/// # Safety
///
/// `spotter` and `stream` must each be null or a live pointer from this library.
#[no_mangle]
pub unsafe extern "C" fn rightkit_asr_kws_stream_result_json(
    spotter: *mut RightKitAsrKwsSpotter,
    stream: *mut RightKitAsrKwsStream,
) -> *mut c_char {
    guarded(|| {
        let (spotter, stream) = unsafe { (handle(spotter, "spotter")?, handle(stream, "stream")?) };
        let spotter = lock(&spotter.0)?;
        let mut stream = lock(&stream.0)?;
        Ok(sherpa_result_json(&spotter.result(&mut stream)))
    })
    .ok()
    .and_then(|json| CString::new(json).ok())
    .map_or(null_mut(), CString::into_raw)
}

/// Clear the stream's beam and encoder cache (Sherpa `ResetKeywordStream`); buffered
/// features stay and timestamps restart from zero. Returns 0 on success, -1 on failure.
///
/// # Safety
///
/// `spotter` and `stream` must each be null or a live pointer from this library.
#[no_mangle]
pub unsafe extern "C" fn rightkit_asr_kws_stream_reset(
    spotter: *mut RightKitAsrKwsSpotter,
    stream: *mut RightKitAsrKwsStream,
) -> i32 {
    status(guarded(|| {
        let (spotter, stream) = unsafe { (handle(spotter, "spotter")?, handle(stream, "stream")?) };
        let spotter = lock(&spotter.0)?;
        let mut stream = lock(&stream.0)?;
        spotter.reset(&mut stream);
        Ok(())
    }))
}

/// The spotter configuration over the payload in `model_dir`.
fn spotter_config(
    model_dir: &Path,
    keywords: PathBuf,
    keywords_threshold: f32,
    max_active_paths: i32,
    active_path_ttl_frames: i32,
) -> Result<KeywordSpotterConfig, String> {
    if !keywords_threshold.is_finite() {
        return Err(format!(
            "keywords_threshold must be finite, got {keywords_threshold}"
        ));
    }
    let max_active_paths = usize::try_from(max_active_paths)
        .ok()
        .filter(|paths| *paths > 0)
        .ok_or_else(|| format!("max_active_paths must be at least 1, got {max_active_paths}"))?;
    let mut config = KeywordSpotterConfig::new(
        model_dir.join(ENCODER_FILE),
        model_dir.join(DECODER_FILE),
        model_dir.join(JOINER_FILE),
        model_dir.join(TOKENS_FILE),
        Keywords::File(keywords),
    );
    config.threads = THREADS;
    config.max_active_paths = max_active_paths;
    config.num_trailing_blanks = NUM_TRAILING_BLANKS;
    config.keywords_score = KEYWORDS_SCORE;
    config.keywords_threshold = keywords_threshold;
    config.active_path_ttl_frames = (active_path_ttl_frames > 0).then_some(active_path_ttl_frames);
    config.trailing_blank_reset = false;
    Ok(config)
}

/// Sherpa-ONNX `KeywordResult::AsJsonString` for the fields a cascade reads.
/// Timestamps keep Sherpa's `std::fixed << std::setprecision(2)` text, so a caller
/// converting seconds to sample offsets sees the same values Sherpa's JSON carried.
fn sherpa_result_json(result: &KeywordResult) -> String {
    let timestamps: Vec<String> = result
        .timestamps
        .iter()
        .map(|seconds| format!("{seconds:.2}"))
        .collect();
    format!(
        "{{\"keyword\": {}, \"timestamps\": [{}], \"tokens\": {}}}",
        serde_json::Value::from(result.keyword.as_str()),
        timestamps.join(", "),
        serde_json::Value::from(result.tokens.clone()),
    )
}

fn bind_runtime(requested: Option<PathBuf>) -> Result<(), String> {
    let mut bound = RUNTIME.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(existing) = bound.as_ref() {
        return match (existing, &requested) {
            (_, None) => Ok(()),
            (RuntimeSource::Library(loaded), Some(path)) if same_file(loaded, path) => Ok(()),
            _ => Err(format!(
                "ONNX Runtime is already bound to {}; it cannot be replaced",
                describe(existing)
            )),
        };
    }
    let source = match requested {
        Some(path) => {
            #[cfg(target_os = "ios")]
            if STATIC_API_INSTALLED.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(
                    "ONNX Runtime is already bound to the statically linked runtime; it cannot be replaced"
                        .into(),
                );
            }
            bind_library(&path)?;
            RuntimeSource::Library(path)
        }
        None => {
            bind_static()?;
            RuntimeSource::Static
        }
    };
    *bound = Some(source);
    Ok(())
}

fn runtime_ready() -> Result<(), String> {
    if RUNTIME
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .is_some()
    {
        Ok(())
    } else {
        Err("ONNX Runtime is not bound; call rightkit_asr_kws_runtime_init first".into())
    }
}

fn describe(source: &RuntimeSource) -> String {
    match source {
        RuntimeSource::Static => "the statically linked runtime".into(),
        RuntimeSource::Library(path) => path.display().to_string(),
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Load the runtime library through rightkit_onnx's checked binding: an absolute
/// regular file that exports `OrtGetApiBase` at the pinned API version or newer.
fn bind_library(path: &Path) -> Result<(), String> {
    let selection =
        rightkit_onnx::configure_runtime([rightkit_onnx::explicit_file_candidate(path)])
            .map_err(|error| format!("bind ONNX Runtime {}: {error}", path.display()))?;
    rightkit_onnx::init_environment(&selection, &rightkit_onnx::EnvironmentOptions::default())
        .map(|_| ())
        .map_err(|error| format!("initialise ONNX Runtime {}: {error}", path.display()))
}

/// iOS cannot load a separate ONNX Runtime library: the app links the static
/// `onnxruntime.xcframework` (which Sherpa uses too). Hand ort that runtime's API
/// table so it never tries to load one, then build rightkit_onnx's process
/// environment (telemetry off, per-session thread pools) on it.
#[cfg(target_os = "ios")]
fn bind_static() -> Result<(), String> {
    use rightkit_onnx::ort;

    // SAFETY: `OrtGetApiBase` is ONNX Runtime's C entry point, resolved at link time
    // against the static runtime. It takes no arguments and returns a pointer to a
    // table with static lifetime.
    let base = unsafe { ort::sys::OrtGetApiBase() };
    if base.is_null() {
        return Err("linked ONNX Runtime returned no API base".into());
    }
    // SAFETY: `base` is non-null and points at the runtime's static `OrtApiBase`;
    // `GetVersionString` returns a static NUL-terminated string.
    let version = unsafe { CStr::from_ptr(((*base).GetVersionString)()) }
        .to_str()
        .map_err(|_| "linked ONNX Runtime version string is not UTF-8".to_string())?;
    // The same floor ort applies when it loads a library itself.
    let minor = version
        .split('.')
        .nth(1)
        .and_then(|minor| minor.parse::<u32>().ok())
        .unwrap_or(0);
    if minor < ort::MINOR_VERSION {
        return Err(format!(
            "linked ONNX Runtime {version} is older than the 1.{}.x this build requires",
            ort::MINOR_VERSION
        ));
    }
    // SAFETY: as above; `GetApi` returns null for an unsupported version.
    let api = unsafe { ((*base).GetApi)(ort::sys::ORT_API_VERSION) };
    if api.is_null() {
        return Err(format!(
            "linked ONNX Runtime {version} does not serve API version {}",
            ort::sys::ORT_API_VERSION
        ));
    }
    // Only a successful installation here proves which runtime ort holds.
    // set_api(false) can mean another user installed an unrelated API table.
    // Keep this proof before environment creation so a failed environment call
    // may retry this same static binding, but cannot switch to a library path.
    if !STATIC_API_INSTALLED.load(std::sync::atomic::Ordering::Relaxed) {
        // SAFETY: `api` is non-null and points at the linked runtime's static
        // table for the requested version, at least as long as ort's `OrtApi`.
        if !ort::set_api(unsafe { (*api).clone() }) {
            return Err(
                "ONNX Runtime already holds an API binding not installed by rightkit_asr_kws_runtime_init; refusing the static binding"
                    .into(),
            );
        }
        STATIC_API_INSTALLED.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    rightkit_onnx::ort_environment()
        .map(|_| ())
        .map_err(|error| format!("ONNX Runtime {version} environment: {error}"))
}

#[cfg(not(target_os = "ios"))]
fn bind_static() -> Result<(), String> {
    Err("no statically linked ONNX Runtime on this target; pass the runtime library path".into())
}

fn guarded<T>(body: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(result) => result,
        Err(payload) => Err(format!("panic: {}", panic_message(payload.as_ref()))),
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown payload".into())
}

/// Release a string returned by this library (error messages and result JSON).
/// Null is ignored.
///
/// # Safety
///
/// `value` must be null or a pointer this library returned that is not used again and
/// not released twice. Free it only with this function, never with `free`.
#[no_mangle]
pub unsafe extern "C" fn rightkit_asr_string_free(value: *mut c_char) {
    if !value.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(unsafe { CString::from_raw(value) })));
    }
}

fn status(result: Result<(), String>) -> i32 {
    if result.is_ok() {
        0
    } else {
        -1
    }
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, String> {
    mutex
        .lock()
        .map_err(|_| "keyword spotter state was poisoned by an earlier panic".to_string())
}

/// # Safety
///
/// `pointer` must be null or point to a live `T`.
unsafe fn handle<'a, T>(pointer: *mut T, name: &str) -> Result<&'a T, String> {
    unsafe { pointer.as_ref() }.ok_or_else(|| format!("{name} is null"))
}

/// # Safety
///
/// `value` must be null or a valid NUL-terminated C string that outlives `'a`.
unsafe fn utf8_arg<'a>(value: *const c_char, name: &str) -> Result<&'a str, String> {
    if value.is_null() {
        return Err(format!("{name} is null"));
    }
    unsafe { CStr::from_ptr(value) }
        .to_str()
        .map_err(|_| format!("{name} is not UTF-8"))
}

/// # Safety
///
/// `error_out` must be null or point to a writable `char *`.
unsafe fn clear_error(error_out: *mut *mut c_char) {
    if !error_out.is_null() {
        unsafe { *error_out = null_mut() };
    }
}

/// # Safety
///
/// `error_out` must be null or point to a writable `char *`.
unsafe fn write_error(error_out: *mut *mut c_char, message: &str) {
    if !error_out.is_null() {
        let message = CString::new(message.replace('\0', " ")).unwrap_or_default();
        unsafe { *error_out = message.into_raw() };
    }
}
