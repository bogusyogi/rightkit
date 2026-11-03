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

/// Today's decode thread count: logical CPUs clamped to 1..=8.
fn default_threads() -> c_int {
    std::thread::available_parallelism()
        .map(|n| n.get() as c_int)
        .unwrap_or(4)
        .clamp(1, 8)
}

type FnInitFromFile = unsafe extern "C" fn(*const c_char) -> *mut WhisperContext;
type FnContextDefaultParams = unsafe extern "C" fn() -> WhisperContextParams;
type FnInitFromFileWithParams =
    unsafe extern "C" fn(*const c_char, WhisperContextParams) -> *mut WhisperContext;
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
        let (ctx, init_call) = match (context_default_params, init_with_params) {
            (Some(defaults), Some(init)) => {
                // SAFETY: resolved exports; the returned struct is the DLL's own defaults.
                let params = unsafe { defaults() };
                tracing::info!(
                    use_gpu = params.use_gpu,
                    flash_attn = params.flash_attn,
                    gpu_device = params.gpu_device,
                    "whisper context params"
                );
                // SAFETY: `model_c` outlives the call; params passed by value.
                (
                    unsafe { init(model_c.as_ptr(), params) },
                    "whisper_init_from_file_with_params",
                )
            }
            _ => {
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

        let threads = default_threads();
        tracing::info!(threads, init_call, "whisper decode params");

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
