// Dynamic FFI binding against the already-staged `whisper.dll` (built by the
// local Vulkan whisper.cpp under `.wcpp_vulkan/`, staged into the packaged
// app next to `whisper-cli.exe` — see `right-release.config.mjs` and
// `scripts/build-windows-nsis.mjs`, both of which already list `whisper.dll`
// as a required runtime file today). This gives model-resident decode: the
// GGUF is loaded once into a `whisper_context` and reused across utterances,
// instead of `whisper-cli.exe` reloading the ~1.6GB model on every call.
//
// Loaded with `libloading` (already a workspace dependency, used the same
// way for `onnxruntime.dll` in `main.rs::configure_ort_dylib`) rather than
// linked at build time: no `.lib` import library is staged alongside the
// DLL, and dynamic loading matches the existing ORT pattern exactly.
//
// ABI CONTRACT (read before touching struct layouts below):
// `whisper_full_params` is a large, non-opaque struct passed *and returned*
// by value across the C boundary. The layouts in this file were verified
// field-by-field against the v1.9.3 `include/whisper.h`; a mismatched layout
// silently corrupts fields rather than erroring. `load` enforces the pin at
// runtime via `whisper_version()` and refuses any other DLL, falling back to
// the known-good CLI path. When upgrading whisper.cpp: rebuild the DLL from
// the new tag, re-diff this file against that tag's `whisper.h`, then bump
// `PINNED_WHISPER_VERSION`.
//
// Layout evidence (2026-10-29, ORT-12): ggml-org/whisper.cpp tag v1.9.3
// `include/whisper.h` (sha256 a7d19f7f…76dd) compiled with clang
// `--target=x86_64-pc-windows-msvc`: `whisper_context_params` is 48 bytes,
// `whisper_full_params` 304 bytes, `whisper_vad_params` 24 bytes. The
// `layout_pins` const block below asserts every field offset against it.
#![cfg(target_os = "windows")]

use libloading::os::windows::{
    Library, Symbol, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
};
use std::ffi::{c_char, c_float, c_int, c_void, CStr, CString};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

#[repr(C)]
#[derive(Clone, Copy)]
struct WhisperContext {
    _private: [u8; 0],
}

type WhisperSamplingStrategy = c_int;
const WHISPER_SAMPLING_GREEDY: WhisperSamplingStrategy = 0;

/// Mirrors `whisper_aheads` (v1.9.3 include/whisper.h:111-114).
#[repr(C)]
#[derive(Clone, Copy)]
struct WhisperAheads {
    n_heads: usize,
    heads: *const c_void,
}

/// Mirrors `whisper_context_params` (v1.9.3 include/whisper.h:116-129).
/// Always obtained from the DLL's own `whisper_context_default_params()` and
/// only then selectively overridden, so the untouched fields are exactly what
/// `whisper_init_from_file` used (it is literally
/// `whisper_init_from_file_with_params(path, whisper_context_default_params())`,
/// src/whisper.cpp:3804-3806 at v1.9.3). v1.9.3 defaults: use_gpu = true,
/// flash_attn = true, gpu_device = 0 (src/whisper.cpp:3616-3632).
#[repr(C)]
#[derive(Clone, Copy)]
struct WhisperContextParams {
    use_gpu: bool,
    flash_attn: bool,
    /// Index among ggml backend devices of type GPU or IGPU, in ggml
    /// registry order (src/whisper.cpp:1290-1327), NOT a DXGI adapter index.
    gpu_device: c_int,
    dtw_token_timestamps: bool,
    dtw_aheads_preset: c_int,
    dtw_n_top: c_int,
    dtw_aheads: WhisperAheads,
    dtw_mem_size: usize,
}

/// The whisper.cpp release the struct layouts in this file were verified
/// against, and the only version the staged `whisper.dll` is built from
/// (`.wcpp_vulkan/whisper.cpp` @ v1.9.3). Bump both together.
const PINNED_WHISPER_VERSION: &str = "1.9.3";

// Mirrors `whisper_full_params` from `whisper.h`. Field order matters; do not
// reorder without checking upstream whisper.cpp. `bool` in C is one byte
// here (matches `_Bool`/`<stdbool.h>` on the MSVC/Clang toolchains
// whisper.cpp is built with), `enum` is a 4-byte `c_int`.
#[repr(C)]
struct WhisperFullParams {
    strategy: WhisperSamplingStrategy,

    n_threads: c_int,
    n_max_text_ctx: c_int,
    offset_ms: c_int,
    duration_ms: c_int,

    translate: bool,
    no_context: bool,
    no_timestamps: bool,
    single_segment: bool,
    print_special: bool,
    print_progress: bool,
    print_realtime: bool,
    print_timestamps: bool,

    token_timestamps: bool,
    thold_pt: c_float,
    thold_ptsum: c_float,
    max_len: c_int,
    split_on_word: bool,
    max_tokens: c_int,

    debug_mode: bool,
    audio_ctx: c_int,

    tdrz_enable: bool,

    suppress_regex: *const c_char,

    initial_prompt: *const c_char,
    carry_initial_prompt: bool,
    prompt_tokens: *const c_int,
    prompt_n_tokens: c_int,

    language: *const c_char,
    detect_language: bool,

    suppress_blank: bool,
    suppress_nst: bool,

    temperature: c_float,
    max_initial_ts: c_float,
    length_penalty: c_float,

    temperature_inc: c_float,
    entropy_thold: c_float,
    logprob_thold: c_float,
    no_speech_thold: c_float,

    greedy_best_of: c_int,

    beam_search_beam_size: c_int,
    beam_search_patience: c_float,

    new_segment_callback: *const c_void,
    new_segment_callback_user_data: *mut c_void,

    progress_callback: *const c_void,
    progress_callback_user_data: *mut c_void,

    encoder_begin_callback: *const c_void,
    encoder_begin_callback_user_data: *mut c_void,

    abort_callback: Option<extern "C" fn(*mut c_void) -> bool>,
    abort_callback_user_data: *mut c_void,

    logits_filter_callback: *const c_void,
    logits_filter_callback_user_data: *mut c_void,

    grammar_rules: *const *const c_void,
    n_grammar_rules: usize,
    i_start_rule: usize,
    grammar_penalty: c_float,

    vad: bool,
    vad_model_path: *const c_char,
    vad_params: WhisperVadParams,
}

/// Mirrors `whisper_vad_params` (v1.9.3). Carried inside `whisper_full_params`
/// by value, so it must be present even though we never enable VAD.
#[repr(C)]
struct WhisperVadParams {
    threshold: c_float,
    min_speech_duration_ms: c_int,
    min_silence_duration_ms: c_int,
    max_speech_duration_s: c_float,
    speech_pad_ms: c_int,
    samples_overlap: c_float,
}

/// Compile-time ABI pins: offsets measured from the v1.9.3 header under the
/// MSVC x64 ABI (see module doc). A failed assert here means this file no
/// longer mirrors the C structs and must be re-diffed before shipping.
#[cfg(target_pointer_width = "64")]
const _: () = {
    use std::mem::{offset_of, size_of};
    assert!(size_of::<WhisperContextParams>() == 48);
    assert!(offset_of!(WhisperContextParams, flash_attn) == 1);
    assert!(offset_of!(WhisperContextParams, gpu_device) == 4);
    assert!(offset_of!(WhisperContextParams, dtw_token_timestamps) == 8);
    assert!(offset_of!(WhisperContextParams, dtw_aheads_preset) == 12);
    assert!(offset_of!(WhisperContextParams, dtw_n_top) == 16);
    assert!(offset_of!(WhisperContextParams, dtw_aheads) == 24);
    assert!(offset_of!(WhisperContextParams, dtw_mem_size) == 40);
    assert!(size_of::<WhisperVadParams>() == 24);
    assert!(size_of::<WhisperFullParams>() == 304);
    assert!(offset_of!(WhisperFullParams, n_threads) == 4);
    assert!(offset_of!(WhisperFullParams, translate) == 20);
    assert!(offset_of!(WhisperFullParams, print_timestamps) == 27);
    assert!(offset_of!(WhisperFullParams, token_timestamps) == 28);
    assert!(offset_of!(WhisperFullParams, thold_pt) == 32);
    assert!(offset_of!(WhisperFullParams, max_len) == 40);
    assert!(offset_of!(WhisperFullParams, split_on_word) == 44);
    assert!(offset_of!(WhisperFullParams, max_tokens) == 48);
    assert!(offset_of!(WhisperFullParams, debug_mode) == 52);
    assert!(offset_of!(WhisperFullParams, audio_ctx) == 56);
    assert!(offset_of!(WhisperFullParams, tdrz_enable) == 60);
    assert!(offset_of!(WhisperFullParams, suppress_regex) == 64);
    assert!(offset_of!(WhisperFullParams, initial_prompt) == 72);
    assert!(offset_of!(WhisperFullParams, carry_initial_prompt) == 80);
    assert!(offset_of!(WhisperFullParams, prompt_tokens) == 88);
    assert!(offset_of!(WhisperFullParams, prompt_n_tokens) == 96);
    assert!(offset_of!(WhisperFullParams, language) == 104);
    assert!(offset_of!(WhisperFullParams, detect_language) == 112);
    assert!(offset_of!(WhisperFullParams, suppress_nst) == 114);
    assert!(offset_of!(WhisperFullParams, temperature) == 116);
    assert!(offset_of!(WhisperFullParams, no_speech_thold) == 140);
    assert!(offset_of!(WhisperFullParams, greedy_best_of) == 144);
    assert!(offset_of!(WhisperFullParams, beam_search_beam_size) == 148);
    assert!(offset_of!(WhisperFullParams, beam_search_patience) == 152);
    assert!(offset_of!(WhisperFullParams, new_segment_callback) == 160);
    assert!(offset_of!(WhisperFullParams, abort_callback) == 208);
    assert!(offset_of!(WhisperFullParams, abort_callback_user_data) == 216);
    assert!(offset_of!(WhisperFullParams, logits_filter_callback_user_data) == 232);
    assert!(offset_of!(WhisperFullParams, grammar_rules) == 240);
    assert!(offset_of!(WhisperFullParams, i_start_rule) == 256);
    assert!(offset_of!(WhisperFullParams, grammar_penalty) == 264);
    assert!(offset_of!(WhisperFullParams, vad) == 268);
    assert!(offset_of!(WhisperFullParams, vad_model_path) == 272);
    assert!(offset_of!(WhisperFullParams, vad_params) == 280);
};

/// Test-mode overrides for the Windows whisper.cpp context and decode
/// (ORT-12). Every field `None` = today's behaviour exactly.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct WhisperWinTuning {
    /// `RIGHTKIT_WHISPER_WIN_LEGACY_INIT=1`: use `whisper_init_from_file` (the
    /// pre-ORT-12 call) instead of `_with_params(default_params())`. A/B only;
    /// both build the same context.
    legacy_init: bool,
    /// `RIGHTKIT_WHISPER_WIN_GPU_DEVICE=<n>|calibrated`.
    gpu_device: Option<GpuDeviceChoice>,
    /// `RIGHTKIT_WHISPER_WIN_FLASH_ATTN=0|1` (v1.9.3 default: 1).
    flash_attn: Option<bool>,
    /// `RIGHTKIT_WHISPER_WIN_THREADS=<n>` (default: logical CPUs clamped 1..=8).
    threads: Option<c_int>,
    /// `RIGHTKIT_WHISPER_WIN_AUDIO_CTX=<n>` (default: whisper.cpp's 0 = full 1500).
    /// Changes output: WER-gate it before any default change.
    audio_ctx: Option<c_int>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum GpuDeviceChoice {
    Index(c_int),
    /// The ggml GPU whose description equals the calibrated DirectML
    /// adapter's DXGI name (the only bridge between the two numberings).
    Calibrated,
}

impl WhisperWinTuning {
    fn from_env() -> Self {
        fn var(name: &str) -> Option<String> {
            match name {
                "RIGHTKIT_WHISPER_WIN_LEGACY_INIT" => {
                    rightkit_dictation_core::dev_var!("RIGHTKIT_WHISPER_WIN_LEGACY_INIT").ok()
                }
                "RIGHTKIT_WHISPER_WIN_GPU_DEVICE" => {
                    rightkit_dictation_core::dev_var!("RIGHTKIT_WHISPER_WIN_GPU_DEVICE").ok()
                }
                "RIGHTKIT_WHISPER_WIN_FLASH_ATTN" => {
                    rightkit_dictation_core::dev_var!("RIGHTKIT_WHISPER_WIN_FLASH_ATTN").ok()
                }
                "RIGHTKIT_WHISPER_WIN_THREADS" => {
                    rightkit_dictation_core::dev_var!("RIGHTKIT_WHISPER_WIN_THREADS").ok()
                }
                "RIGHTKIT_WHISPER_WIN_AUDIO_CTX" => {
                    rightkit_dictation_core::dev_var!("RIGHTKIT_WHISPER_WIN_AUDIO_CTX").ok()
                }
                _ => None,
            }
        }
        Self::parse(var)
    }

    /// Pure parser (unit-tested). Malformed values are ignored with a warning
    /// so a typo never changes decoding silently.
    fn parse(var: impl Fn(&str) -> Option<String>) -> Self {
        fn positive(name: &str, raw: Option<String>) -> Option<c_int> {
            let raw = raw?;
            match raw.trim().parse::<c_int>() {
                Ok(n) if n > 0 => Some(n),
                _ => {
                    tracing::warn!(switch = name, value = %raw, "ignoring invalid whisper switch");
                    None
                }
            }
        }
        let legacy_init = var("RIGHTKIT_WHISPER_WIN_LEGACY_INIT")
            .as_deref()
            .map(str::trim)
            == Some("1");
        let gpu_device = var("RIGHTKIT_WHISPER_WIN_GPU_DEVICE").and_then(|raw| {
            let value = raw.trim();
            if value.eq_ignore_ascii_case("calibrated") {
                Some(GpuDeviceChoice::Calibrated)
            } else {
                match value.parse::<c_int>() {
                    Ok(n) if n >= 0 => Some(GpuDeviceChoice::Index(n)),
                    _ => {
                        tracing::warn!(value = %raw, "ignoring invalid RIGHTKIT_WHISPER_WIN_GPU_DEVICE");
                        None
                    }
                }
            }
        });
        let flash_attn = var("RIGHTKIT_WHISPER_WIN_FLASH_ATTN").and_then(|raw| match raw.trim() {
            "1" => Some(true),
            "0" => Some(false),
            _ => {
                tracing::warn!(value = %raw, "ignoring invalid RIGHTKIT_WHISPER_WIN_FLASH_ATTN");
                None
            }
        });
        Self {
            legacy_init,
            gpu_device,
            flash_attn,
            threads: positive(
                "RIGHTKIT_WHISPER_WIN_THREADS",
                var("RIGHTKIT_WHISPER_WIN_THREADS"),
            ),
            audio_ctx: positive(
                "RIGHTKIT_WHISPER_WIN_AUDIO_CTX",
                var("RIGHTKIT_WHISPER_WIN_AUDIO_CTX"),
            ),
        }
    }

    fn changes_context(&self) -> bool {
        self.gpu_device.is_some() || self.flash_attn.is_some()
    }

    /// Apply context overrides onto the DLL's own defaults. `calibrated`
    /// resolves through `resolve_calibrated` and keeps the default device
    /// when no unique match exists.
    fn apply_context(
        &self,
        mut params: WhisperContextParams,
        resolve_calibrated: impl FnOnce() -> Option<c_int>,
    ) -> WhisperContextParams {
        match &self.gpu_device {
            Some(GpuDeviceChoice::Index(n)) => params.gpu_device = *n,
            Some(GpuDeviceChoice::Calibrated) => {
                if let Some(n) = resolve_calibrated() {
                    params.gpu_device = n;
                }
            }
            None => {}
        }
        if let Some(flash_attn) = self.flash_attn {
            params.flash_attn = flash_attn;
        }
        params
    }
}

/// Today's decode thread count: logical CPUs clamped to 1..=8.
fn default_threads() -> c_int {
    std::thread::available_parallelism()
        .map(|n| n.get() as c_int)
        .unwrap_or(4)
        .clamp(1, 8)
}

/// whisper.cpp's GPU index for the device named `wanted`, counting only GPU
/// and IGPU devices in registry order exactly as `whisper_backend_init_gpu`
/// does. `None` unless exactly one device matches.
fn ggml_gpu_index_by_name(devices: &[(c_int, String)], wanted: &str) -> Option<c_int> {
    let mut gpu_index = 0;
    let mut found = None;
    let mut matches = 0;
    for (kind, description) in devices {
        if *kind == GGML_BACKEND_DEVICE_TYPE_GPU || *kind == GGML_BACKEND_DEVICE_TYPE_IGPU {
            if description.trim() == wanted.trim() {
                matches += 1;
                found = Some(gpu_index);
            }
            gpu_index += 1;
        }
    }
    (matches == 1).then_some(found).flatten()
}

/// Enumerate ggml backend devices from the staged ggml DLLs (already loaded
/// by `whisper.dll`, so this only bumps their reference counts).
fn ggml_devices(dll_dir: &Path) -> Result<Vec<(c_int, String)>, String> {
    let open = |name: &str| {
        // SAFETY: staged sibling DLLs already mapped by whisper.dll's imports.
        unsafe {
            Library::load_with_flags(
                dll_dir.join(name),
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
            )
        }
        .ok()
    };
    let libs: Vec<Library> = ["ggml.dll", "ggml-base.dll"]
        .into_iter()
        .filter_map(open)
        .collect();
    fn find<T: Copy>(libs: &[Library], name: &[u8]) -> Option<T> {
        // SAFETY: signatures mirror ggml-backend.h at the pinned tag.
        libs.iter()
            .find_map(|lib| unsafe { lib.get::<T>(name) }.ok().map(|s| *s))
    }
    let count: FnGgmlDevCount =
        find(&libs, b"ggml_backend_dev_count\0").ok_or("ggml_backend_dev_count missing")?;
    let get: FnGgmlDevGet =
        find(&libs, b"ggml_backend_dev_get\0").ok_or("ggml_backend_dev_get missing")?;
    let kind: FnGgmlDevType =
        find(&libs, b"ggml_backend_dev_type\0").ok_or("ggml_backend_dev_type missing")?;
    let description: FnGgmlDevDescription = find(&libs, b"ggml_backend_dev_description\0")
        .ok_or("ggml_backend_dev_description missing")?;
    let mut devices = Vec::new();
    // SAFETY: registry calls on the loaded ggml; indices stay below count().
    unsafe {
        for i in 0..count() {
            let dev = get(i);
            if dev.is_null() {
                continue;
            }
            let ptr = description(dev);
            let text = if ptr.is_null() {
                String::new()
            } else {
                CStr::from_ptr(ptr).to_string_lossy().into_owned()
            };
            devices.push((kind(dev), text));
        }
    }
    // `libs` drop here: FreeLibrary only decrements the count whisper.dll holds.
    Ok(devices)
}

/// The calibrated DirectML adapter's name mapped onto whisper.cpp's GPU index.
fn calibrated_gpu_index(dll_dir: &Path) -> Option<c_int> {
    let adapter_index = crate::calibration::active_route().dml_device_id?;
    let adapters = crate::calibration::enumerate_dml_adapters().ok()?;
    let name = adapters
        .iter()
        .find(|adapter| adapter.index == adapter_index)
        .map(|adapter| adapter.name.clone())?;
    let devices = match ggml_devices(dll_dir) {
        Ok(devices) => devices,
        Err(error) => {
            tracing::warn!(
                error,
                "whisper calibrated GPU: ggml device list unavailable"
            );
            return None;
        }
    };
    let index = ggml_gpu_index_by_name(&devices, &name);
    tracing::info!(
        calibrated_adapter = %name,
        ggml_devices = ?devices,
        resolved_gpu_device = ?index,
        "whisper calibrated GPU resolution"
    );
    index
}

type FnInitFromFile = unsafe extern "C" fn(*const c_char) -> *mut WhisperContext;
type FnContextDefaultParams = unsafe extern "C" fn() -> WhisperContextParams;
type FnInitFromFileWithParams =
    unsafe extern "C" fn(*const c_char, WhisperContextParams) -> *mut WhisperContext;
// ggml backend device registry (ggml.dll / ggml-base.dll at the same tag,
// ggml/include/ggml-backend.h:134-145,181-184,241-242). Only resolved for
// `RIGHTKIT_WHISPER_WIN_GPU_DEVICE=calibrated`.
type FnGgmlDevCount = unsafe extern "C" fn() -> usize;
type FnGgmlDevGet = unsafe extern "C" fn(usize) -> *mut c_void;
type FnGgmlDevType = unsafe extern "C" fn(*mut c_void) -> c_int;
type FnGgmlDevDescription = unsafe extern "C" fn(*mut c_void) -> *const c_char;
const GGML_BACKEND_DEVICE_TYPE_GPU: c_int = 1;
const GGML_BACKEND_DEVICE_TYPE_IGPU: c_int = 2;
type FnFree = unsafe extern "C" fn(*mut WhisperContext);
type FnDefaultParams = unsafe extern "C" fn(WhisperSamplingStrategy) -> WhisperFullParams;
type FnFull =
    unsafe extern "C" fn(*mut WhisperContext, WhisperFullParams, *const c_float, c_int) -> c_int;
type FnNSegments = unsafe extern "C" fn(*mut WhisperContext) -> c_int;
type FnSegmentText = unsafe extern "C" fn(*mut WhisperContext, c_int) -> *const c_char;
type FnFullLangId = unsafe extern "C" fn(*mut WhisperContext) -> c_int;
type FnLangStr = unsafe extern "C" fn(c_int) -> *const c_char;
// v1.9.3 include/whisper.h:446-447: both return void and take only ctx.
// Avoid whisper_get_timings: it allocates with C++ new, has no public matching
// free API, omits mel and returns per-run averages rather than stage totals.
type FnContextTimings = unsafe extern "C" fn(*mut WhisperContext);

#[derive(Clone, Copy)]
struct NativeTimings {
    reset: FnContextTimings,
    print: FnContextTimings,
}

impl NativeTimings {
    fn pair(reset: Option<FnContextTimings>, print: Option<FnContextTimings>) -> Option<Self> {
        Some(Self {
            reset: reset?,
            print: print?,
        })
    }
}

static NEXT_TIMING_CALL: AtomicU64 = AtomicU64::new(1);

/// Cooperative abort flag threaded through `whisper_full_params::abort_callback`.
/// whisper.cpp polls this between decode steps, which is the sanctioned way
/// to bound an in-process decode — there is no way to safely kill the OS
/// thread mid-`whisper_full` call from Rust.
struct AbortFlag(AtomicBool);

extern "C" fn abort_trampoline(user_data: *mut c_void) -> bool {
    if user_data.is_null() {
        return false;
    }
    // SAFETY: `user_data` is always the `Arc<AbortFlag>` raw pointer we set
    // up in `transcribe`, kept alive for the duration of the call.
    let flag = unsafe { &*(user_data as *const AbortFlag) };
    flag.0.load(Ordering::Relaxed)
}

/// A loaded whisper.dll plus a resident model context. Lives for the process
/// lifetime once created; `WhisperWin` owns exactly one behind a `Mutex` and
/// creates it lazily on first use.
pub struct WhisperFfiEngine {
    _lib: Option<Library>,
    free: FnFree,
    default_params: FnDefaultParams,
    full: FnFull,
    n_segments: FnNSegments,
    segment_text: FnSegmentText,
    full_lang_id: FnFullLangId,
    lang_str: FnLangStr,
    timings: Option<NativeTimings>,
    ctx: *mut WhisperContext,
    threads: c_int,
    /// `Some` only under `RIGHTKIT_WHISPER_WIN_AUDIO_CTX`; `None` leaves the
    /// default params' value (0 = full context) untouched.
    audio_ctx: Option<c_int>,
}

// SAFETY: whisper.cpp's `whisper_context` is safe to use from a single
// owning thread at a time, which is exactly how `WhisperWin` uses it (calls
// serialized behind a `Mutex<WhisperFfiEngine>`). The raw fn pointers and
// `Library` handle are immutable after construction.
unsafe impl Send for WhisperFfiEngine {}

impl WhisperFfiEngine {
    /// Load `whisper.dll` next to `dll_dir` and initialize a context from
    /// `model_path`. Returns `Err` (never panics/aborts) if the DLL, any
    /// required export, or the model load fails — callers should treat this
    /// as "FFI unavailable, use the CLI fallback", not a fatal error.
    pub fn load(dll_dir: &Path, model_path: &Path) -> Result<Self, String> {
        let dll_path = dll_dir.join("whisper.dll");
        // SAFETY: load_with_flags executes DLL initialization; this is our
        // staged build output. Search flags keep sibling ggml DLL resolution
        // directory-local without changing process-global DLL search state.
        let lib = unsafe {
            Library::load_with_flags(
                &dll_path,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
            )
        }
        .map_err(|e| format!("load whisper.dll at {}: {e}", dll_path.display()))?;

        macro_rules! sym {
            ($name:literal) => {{
                let s: Symbol<_> = unsafe {
                    lib.get($name)
                        .map_err(|e| format!("resolve {}: {e}", stringify!($name)))?
                };
                // Copy out a plain fn pointer, detached from the `Symbol`
                // borrow — sound because `_lib` stays alive for the struct's
                // whole lifetime and the DLL is never unloaded early.
                //
                // This dereferences rather than `transmute_copy`ing. The
                // transmute left `Symbol`'s own type parameter unconstrained:
                // `transmute_copy::<T, U>` relates T and U not at all, so a call
                // site's annotation fixed only U and inference had nothing to
                // pin T to — E0282 on rustc 1.97, which broke the whole Windows
                // build. Dereferencing makes the symbol's type BE the annotated
                // fn type, which compiles and also replaces an unchecked
                // reinterpret with a type-checked copy.
                *s
            }};
        }

        // ABI pin: the struct layouts in this file were verified against the
        // v1.9.3 `whisper.h`. Refuse any other DLL version — a layout drift
        // corrupts memory silently instead of failing, so an exact-match
        // guard (with CLI fallback) is the only safe default.
        let version: unsafe extern "C" fn() -> *const c_char = sym!(b"whisper_version\0");
        // SAFETY: resolved export returning a static NUL-terminated string.
        let reported = unsafe { CStr::from_ptr(version()) }.to_string_lossy();
        if reported != PINNED_WHISPER_VERSION {
            return Err(format!(
                "whisper.dll version {reported} does not match pinned {PINNED_WHISPER_VERSION}; using CLI path"
            ));
        }

        let init_from_file: FnInitFromFile = sym!(b"whisper_init_from_file\0");
        // ORT-12: v1.9.3 exports both (include/whisper.h:206, :594 region);
        // optional so a DLL lacking them keeps today's legacy init.
        // SAFETY: exact DLL version checked above; signatures mirror the header.
        let context_default_params: Option<FnContextDefaultParams> = unsafe {
            lib.get::<FnContextDefaultParams>(b"whisper_context_default_params\0")
                .ok()
                .map(|s| *s)
        };
        let init_with_params: Option<FnInitFromFileWithParams> = unsafe {
            lib.get::<FnInitFromFileWithParams>(b"whisper_init_from_file_with_params\0")
                .ok()
                .map(|s| *s)
        };
        let tuning = WhisperWinTuning::from_env();
        let free: FnFree = sym!(b"whisper_free\0");
        let default_params: FnDefaultParams = sym!(b"whisper_full_default_params\0");
        let full: FnFull = sym!(b"whisper_full\0");
        let n_segments: FnNSegments = sym!(b"whisper_full_n_segments\0");
        let segment_text: FnSegmentText = sym!(b"whisper_full_get_segment_text\0");
        let full_lang_id: FnFullLangId = sym!(b"whisper_full_lang_id\0");
        let lang_str: FnLangStr = sym!(b"whisper_lang_str\0");
        // Optional as a pair: without both exports preserve decoding and report
        // unavailable timings, never cumulative values or synthetic zeroes.
        // SAFETY: exact DLL version was checked above; signatures match its
        // authoritative header, and `_lib` retains both function pointers.
        let timings = unsafe {
            NativeTimings::pair(
                lib.get::<FnContextTimings>(b"whisper_reset_timings\0")
                    .ok()
                    .map(|s| *s),
                lib.get::<FnContextTimings>(b"whisper_print_timings\0")
                    .ok()
                    .map(|s| *s),
            )
        };

        let model_c = CString::new(model_path.to_string_lossy().as_bytes())
            .map_err(|e| format!("model path has NUL byte: {e}"))?;
        let (ctx, init_call) = match (tuning.legacy_init, context_default_params, init_with_params)
        {
            (false, Some(defaults), Some(init)) => {
                // SAFETY: resolved exports; the returned struct is the DLL's own
                // defaults, overridden only by explicit test-mode switches.
                let base = unsafe { defaults() };
                let params = tuning.apply_context(base, || calibrated_gpu_index(dll_dir));
                tracing::info!(
                    use_gpu = params.use_gpu,
                    flash_attn = params.flash_attn,
                    gpu_device = params.gpu_device,
                    default_flash_attn = base.flash_attn,
                    default_gpu_device = base.gpu_device,
                    "whisper context params"
                );
                // SAFETY: `model_c` outlives the call; params passed by value.
                (
                    unsafe { init(model_c.as_ptr(), params) },
                    "whisper_init_from_file_with_params",
                )
            }
            _ => {
                if tuning.changes_context() {
                    tracing::warn!(
                        "whisper context switches ignored: legacy init or no _with_params export"
                    );
                }
                // SAFETY: `init_from_file` is the resolved `whisper_init_from_file`
                // export; `model_c` stays alive for the call.
                (
                    unsafe { init_from_file(model_c.as_ptr()) },
                    "whisper_init_from_file",
                )
            }
        };
        if ctx.is_null() {
            return Err(format!(
                "{init_call} returned null for {}",
                model_path.display()
            ));
        }

        let threads = tuning.threads.unwrap_or_else(default_threads);
        tracing::info!(
            threads,
            audio_ctx = ?tuning.audio_ctx,
            init_call,
            "whisper decode params"
        );

        Ok(Self {
            _lib: Some(lib),
            free,
            default_params,
            full,
            n_segments,
            segment_text,
            full_lang_id,
            lang_str,
            timings,
            ctx,
            threads,
            audio_ctx: tuning.audio_ctx,
        })
    }

    /// Decode `samples` (16 kHz mono f32) with a resident context. `deadline`
    /// bounds the call cooperatively via `abort_callback` — whisper.cpp
    /// checks it between decode steps and returns early rather than being
    /// killed, since there is no safe way to abandon an in-flight FFI call.
    pub fn transcribe(
        &self,
        samples: &[f32],
        lang: &str,
        deadline: std::time::Instant,
    ) -> Result<String, String> {
        self.transcribe_resolved(samples, lang, deadline)
            .map(|(text, _)| text)
    }

    /// Decode and return whisper.cpp's resolved language id. This is needed
    /// for AUTO recordings: the id is stable across Static15 windows, while
    /// passing `detect_language=true` makes whisper.cpp return before decode.
    pub fn transcribe_resolved(
        &self,
        samples: &[f32],
        lang: &str,
        deadline: std::time::Instant,
    ) -> Result<(String, c_int), String> {
        let lang_c = CString::new(lang).map_err(|e| format!("lang has NUL byte: {e}"))?;

        // SAFETY: `default_params` is the resolved `whisper_full_default_params`
        // export; the returned struct's layout is trusted to match ours (see
        // module doc comment on ABI risk).
        let mut params = unsafe { (self.default_params)(WHISPER_SAMPLING_GREEDY) };
        params.n_threads = self.threads;
        params.translate = false;
        params.no_context = true;
        params.no_timestamps = true;
        params.single_segment = false;
        params.print_special = false;
        params.print_progress = false;
        params.print_realtime = false;
        params.print_timestamps = false;
        params.language = lang_c.as_ptr();
        // whisper.cpp v1.9.3 performs AUTO detection during whisper_full when
        // language is "auto". Its detect_language flag is detect-only and
        // returns before transcription, so it must remain false here.
        params.detect_language = false;
        params.greedy_best_of = 1;
        params.beam_search_beam_size = 1;
        if let Some(audio_ctx) = self.audio_ctx {
            params.audio_ctx = audio_ctx;
        }

        let abort_flag = Arc::new(AbortFlag(AtomicBool::new(false)));
        let flag_ptr = Arc::as_ptr(&abort_flag) as *mut c_void;
        params.abort_callback = Some(abort_trampoline);
        params.abort_callback_user_data = flag_ptr;

        // Poll the deadline from a watchdog thread and flip the flag; the
        // decode call itself is synchronous and blocks this thread.
        let watchdog_flag = Arc::clone(&abort_flag);
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let watchdog = std::thread::spawn(move || loop {
            if done_rx
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_ok()
            {
                return;
            }
            if std::time::Instant::now() >= deadline {
                watchdog_flag.0.store(true, Ordering::Relaxed);
                return;
            }
        });

        let timing_call = NEXT_TIMING_CALL.fetch_add(1, Ordering::Relaxed);
        tracing::info!(
            target: "rightkit_dictation_inference::whisper_win::timing",
            call_id = timing_call,
            phase = "begin",
            samples = samples.len(),
            native_timings_available = self.timings.is_some(),
            "whisper native timing boundary"
        );
        if let Some(timings) = self.timings {
            // SAFETY: serialized access to this live context. v1.9.3 resets
            // mel/sample/encode/decode/batchd/prompt counters and total start;
            // model-load time and fallback counts remain context-lifetime data.
            unsafe { (timings.reset)(self.ctx) };
        }
        let full_started = std::time::Instant::now();
        // SAFETY: `ctx` was produced by `whisper_init_from_file` above and is
        // freed only in `Drop`; `samples` outlives the call.
        let rc = unsafe { (self.full)(self.ctx, params, samples.as_ptr(), samples.len() as c_int) };
        let full_wall_ms = full_started.elapsed().as_secs_f64() * 1000.0;
        let _ = done_tx.send(());
        let _ = watchdog.join();

        let aborted = abort_flag.0.load(Ordering::Relaxed);
        let succeeded = !aborted && rc == 0;
        if succeeded {
            if let Some(timings) = self.timings {
                // Native stderr lines are framed by matching call_id events.
                // v1.9.3 prints true stage totals, run counts and per-run means;
                // only the totals since reset belong to this transcription.
                unsafe { (timings.print)(self.ctx) };
            }
        }
        tracing::info!(
            target: "rightkit_dictation_inference::whisper_win::timing",
            call_id = timing_call,
            phase = "end",
            full_wall_ms,
            return_code = rc,
            aborted,
            native_timings_available = self.timings.is_some(),
            native_totals_printed = succeeded && self.timings.is_some(),
            native_timings_source = "whisper_print_timings_stderr",
            native_stage_scope = "since_call_reset",
            native_load_and_fallback_scope = "context_lifetime",
            "whisper native timing boundary"
        );

        if aborted {
            return Err(format!(
                "whisper_full aborted after deadline ({} samples)",
                samples.len()
            ));
        }
        if rc != 0 {
            return Err(format!("whisper_full returned {rc}"));
        }

        // SAFETY: `n_segments`/`segment_text` are the resolved exports;
        // `ctx` is valid and was just decoded into.
        let n = unsafe { (self.n_segments)(self.ctx) };
        let mut text = String::new();
        for i in 0..n {
            // SAFETY: `i` is in `0..n_segments`, so whisper.cpp guarantees a
            // valid, NUL-terminated pointer for the lifetime of this call.
            let ptr = unsafe { (self.segment_text)(self.ctx, i) };
            if ptr.is_null() {
                continue;
            }
            // SAFETY: `ptr` was just checked non-null and points at a
            // NUL-terminated C string owned by `ctx` (valid until the next
            // `whisper_full` call or `whisper_free`).
            let seg = unsafe { CStr::from_ptr(ptr) }.to_string_lossy();
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(seg.trim());
        }
        let resolved_lang = unsafe { (self.full_lang_id)(self.ctx) };
        Ok((text.trim().to_string(), resolved_lang))
    }

    pub fn language_code(&self, lang_id: c_int) -> Option<String> {
        let ptr = unsafe { (self.lang_str)(lang_id) };
        if ptr.is_null() {
            return None;
        }
        Some(
            unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned(),
        )
    }
}

impl Drop for WhisperFfiEngine {
    fn drop(&mut self) {
        if !self.ctx.is_null() {
            // SAFETY: `ctx` was produced by `init_from_file` and is not used
            // again after this point.
            unsafe { (self.free)(self.ctx) };
            self.ctx = std::ptr::null_mut();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    #[derive(Debug, Default)]
    struct MockCall {
        detect_language: bool,
        language: String,
    }

    fn calls() -> &'static Mutex<Vec<MockCall>> {
        static CALLS: OnceLock<Mutex<Vec<MockCall>>> = OnceLock::new();
        CALLS.get_or_init(|| Mutex::new(Vec::new()))
    }

    unsafe extern "C" fn mock_free(_ctx: *mut WhisperContext) {}

    unsafe extern "C" fn mock_default(_strategy: WhisperSamplingStrategy) -> WhisperFullParams {
        // This test only exercises fields configured by transcribe_resolved;
        // zeroed callback/layout fields match the C default for this mock.
        unsafe { std::mem::zeroed() }
    }

    unsafe extern "C" fn mock_full(
        _ctx: *mut WhisperContext,
        params: WhisperFullParams,
        _samples: *const c_float,
        _n_samples: c_int,
    ) -> c_int {
        let language = if params.language.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(params.language) }
                .to_string_lossy()
                .into_owned()
        };
        calls().lock().unwrap().push(MockCall {
            detect_language: params.detect_language,
            language,
        });
        0
    }

    unsafe extern "C" fn mock_n_segments(_ctx: *mut WhisperContext) -> c_int {
        1
    }

    unsafe extern "C" fn mock_segment_text(
        _ctx: *mut WhisperContext,
        _segment: c_int,
    ) -> *const c_char {
        c"mock transcript".as_ptr()
    }

    unsafe extern "C" fn mock_full_lang_id(_ctx: *mut WhisperContext) -> c_int {
        7
    }

    unsafe extern "C" fn mock_lang_str(lang_id: c_int) -> *const c_char {
        if lang_id == 7 {
            c"fr".as_ptr()
        } else {
            std::ptr::null()
        }
    }

    fn mock_engine() -> WhisperFfiEngine {
        WhisperFfiEngine {
            _lib: None,
            free: mock_free,
            default_params: mock_default,
            full: mock_full,
            n_segments: mock_n_segments,
            segment_text: mock_segment_text,
            full_lang_id: mock_full_lang_id,
            lang_str: mock_lang_str,
            timings: None,
            ctx: std::ptr::dangling_mut::<WhisperContext>(),
            threads: 1,
            audio_ctx: None,
        }
    }

    fn tuning(pairs: &[(&str, &str)]) -> WhisperWinTuning {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        WhisperWinTuning::parse(|name| {
            owned
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        })
    }

    fn v193_context_defaults() -> WhisperContextParams {
        // src/whisper.cpp:3616-3632 at v1.9.3.
        WhisperContextParams {
            use_gpu: true,
            flash_attn: true,
            gpu_device: 0,
            dtw_token_timestamps: false,
            dtw_aheads_preset: 0,
            dtw_n_top: -1,
            dtw_aheads: WhisperAheads {
                n_heads: 0,
                heads: std::ptr::null(),
            },
            dtw_mem_size: 1024 * 1024 * 128,
        }
    }

    fn context_bytes(params: &WhisperContextParams) -> Vec<u8> {
        // Compare field by field (padding is not part of the contract).
        format!(
            "{} {} {} {} {} {} {} {:?} {}",
            params.use_gpu,
            params.flash_attn,
            params.gpu_device,
            params.dtw_token_timestamps,
            params.dtw_aheads_preset,
            params.dtw_n_top,
            params.dtw_aheads.n_heads,
            params.dtw_aheads.heads,
            params.dtw_mem_size
        )
        .into_bytes()
    }

    #[test]
    fn no_switches_means_todays_context_and_decode() {
        let none = tuning(&[]);
        assert_eq!(none, WhisperWinTuning::default());
        assert!(!none.changes_context());
        let base = v193_context_defaults();
        let applied = none.apply_context(base, || panic!("calibration not consulted"));
        assert_eq!(context_bytes(&applied), context_bytes(&base));
        assert_eq!(
            none.threads.unwrap_or_else(default_threads),
            default_threads()
        );
        assert_eq!(none.audio_ctx, None);
    }

    #[test]
    fn switches_parse_and_apply() {
        let t = tuning(&[
            ("RIGHTKIT_WHISPER_WIN_GPU_DEVICE", "2"),
            ("RIGHTKIT_WHISPER_WIN_FLASH_ATTN", "0"),
            ("RIGHTKIT_WHISPER_WIN_THREADS", "6"),
            ("RIGHTKIT_WHISPER_WIN_AUDIO_CTX", "768"),
        ]);
        assert_eq!(t.gpu_device, Some(GpuDeviceChoice::Index(2)));
        assert_eq!(t.flash_attn, Some(false));
        assert_eq!(t.threads, Some(6));
        assert_eq!(t.audio_ctx, Some(768));
        let applied = t.apply_context(v193_context_defaults(), || None);
        assert_eq!(applied.gpu_device, 2);
        assert!(!applied.flash_attn);
        assert!(applied.use_gpu);

        let calibrated = tuning(&[("RIGHTKIT_WHISPER_WIN_GPU_DEVICE", "calibrated")]);
        assert_eq!(calibrated.gpu_device, Some(GpuDeviceChoice::Calibrated));
        assert_eq!(
            calibrated
                .apply_context(v193_context_defaults(), || Some(1))
                .gpu_device,
            1
        );
        // No unique match: keep the default device.
        assert_eq!(
            calibrated
                .apply_context(v193_context_defaults(), || None)
                .gpu_device,
            0
        );

        // Malformed values never change behaviour.
        let bad = tuning(&[
            ("RIGHTKIT_WHISPER_WIN_GPU_DEVICE", "-1"),
            ("RIGHTKIT_WHISPER_WIN_FLASH_ATTN", "yes"),
            ("RIGHTKIT_WHISPER_WIN_THREADS", "0"),
            ("RIGHTKIT_WHISPER_WIN_AUDIO_CTX", "abc"),
        ]);
        assert_eq!(bad, WhisperWinTuning::default());
        assert!(tuning(&[("RIGHTKIT_WHISPER_WIN_LEGACY_INIT", "1")]).legacy_init);
    }

    #[test]
    fn calibrated_name_maps_to_whisper_gpu_ordinal() {
        let devices = vec![
            (0, "CPU".to_string()),
            (2, "Intel(R) Iris(R) Xe Graphics".to_string()),
            (3, "BLAS".to_string()),
            (1, "NVIDIA GeForce RTX 4060 Laptop GPU".to_string()),
        ];
        assert_eq!(
            ggml_gpu_index_by_name(&devices, "NVIDIA GeForce RTX 4060 Laptop GPU"),
            Some(1)
        );
        assert_eq!(
            ggml_gpu_index_by_name(&devices, "Intel(R) Iris(R) Xe Graphics"),
            Some(0)
        );
        assert_eq!(ggml_gpu_index_by_name(&devices, "CPU"), None);
        assert_eq!(ggml_gpu_index_by_name(&devices, "Missing"), None);
        let twins = vec![(1, "GPU".to_string()), (1, "GPU".to_string())];
        assert_eq!(ggml_gpu_index_by_name(&twins, "GPU"), None);
    }

    #[test]
    fn audio_ctx_reaches_whisper_full_only_when_switched() {
        static SEEN: Mutex<Vec<c_int>> = Mutex::new(Vec::new());
        unsafe extern "C" fn record_full(
            _ctx: *mut WhisperContext,
            params: WhisperFullParams,
            _samples: *const c_float,
            _n_samples: c_int,
        ) -> c_int {
            SEEN.lock().unwrap().push(params.audio_ctx);
            0
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let mut engine = mock_engine();
        engine.full = record_full;
        engine.transcribe(&[0.0; 160], "en", deadline).unwrap();
        engine.audio_ctx = Some(512);
        engine.transcribe(&[0.0; 160], "en", deadline).unwrap();
        assert_eq!(*SEEN.lock().unwrap(), [0, 512]);
    }

    struct TimingTestState {
        events: Mutex<Vec<&'static str>>,
        result: std::sync::atomic::AtomicI32,
    }

    unsafe fn timing_test_state<'a>(ctx: *mut WhisperContext) -> &'a TimingTestState {
        // Test engines borrow a live stack-owned state through their context.
        unsafe { &*ctx.cast::<TimingTestState>() }
    }

    unsafe extern "C" fn mock_reset_timings(ctx: *mut WhisperContext) {
        unsafe { timing_test_state(ctx) }
            .events
            .lock()
            .unwrap()
            .push("reset");
    }

    unsafe extern "C" fn mock_print_timings(ctx: *mut WhisperContext) {
        unsafe { timing_test_state(ctx) }
            .events
            .lock()
            .unwrap()
            .push("print");
    }

    unsafe extern "C" fn mock_timed_full(
        ctx: *mut WhisperContext,
        _params: WhisperFullParams,
        _samples: *const c_float,
        _n_samples: c_int,
    ) -> c_int {
        let state = unsafe { timing_test_state(ctx) };
        state.events.lock().unwrap().push("full");
        state.result.load(Ordering::Relaxed)
    }

    fn timing_test_engine(
        state: &TimingTestState,
        timings: Option<NativeTimings>,
    ) -> WhisperFfiEngine {
        let mut engine = mock_engine();
        engine.ctx = std::ptr::from_ref(state).cast_mut().cast();
        engine.full = mock_timed_full;
        engine.timings = timings;
        engine
    }

    #[test]
    fn timing_exports_must_exist_as_a_pair_or_decode_is_unchanged() {
        for (reset, print) in [(false, false), (true, false), (false, true)] {
            let timings = NativeTimings::pair(
                reset.then_some(mock_reset_timings as FnContextTimings),
                print.then_some(mock_print_timings as FnContextTimings),
            );
            assert!(timings.is_none());
            let state = TimingTestState {
                events: Mutex::new(Vec::new()),
                result: std::sync::atomic::AtomicI32::new(0),
            };
            let engine = timing_test_engine(&state, timings);
            let text = engine
                .transcribe(
                    &[0.0; 160],
                    "en",
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                )
                .unwrap();
            assert_eq!(text, "mock transcript");
            assert_eq!(*state.events.lock().unwrap(), ["full"]);
        }
    }

    #[test]
    fn production_timing_order_resets_each_call_and_skips_print_on_failure() {
        let state = TimingTestState {
            events: Mutex::new(Vec::new()),
            result: std::sync::atomic::AtomicI32::new(7),
        };
        let engine = timing_test_engine(
            &state,
            NativeTimings::pair(Some(mock_reset_timings), Some(mock_print_timings)),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        assert!(engine.transcribe(&[0.0; 160], "en", deadline).is_err());
        assert_eq!(*state.events.lock().unwrap(), ["reset", "full"]);
        state.result.store(0, Ordering::Relaxed);
        for _ in 0..2 {
            assert_eq!(
                engine.transcribe(&[0.0; 160], "en", deadline).unwrap(),
                "mock transcript"
            );
        }
        assert_eq!(
            *state.events.lock().unwrap(),
            ["reset", "full", "reset", "full", "print", "reset", "full", "print"]
        );
    }

    #[test]
    fn production_auto_entry_decodes_and_returns_resolved_language() {
        calls().lock().unwrap().clear();
        let engine = mock_engine();
        let (text, lang_id) = engine
            .transcribe_resolved(
                &[0.0; 160],
                "auto",
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            )
            .expect("mock production FFI decode");
        assert_eq!(text, "mock transcript");
        assert_eq!(lang_id, 7);
        let recorded = calls().lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].language, "auto");
        assert!(!recorded[0].detect_language);
        let locked = engine.language_code(lang_id).expect("resolved language");
        assert_eq!(locked, "fr");
        drop(recorded);
        engine
            .transcribe_resolved(
                &[0.0; 160],
                &locked,
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            )
            .expect("mock locked production FFI decode");
        let recorded = calls().lock().unwrap();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[1].language, "fr");
        assert!(!recorded[1].detect_language);
    }
}
