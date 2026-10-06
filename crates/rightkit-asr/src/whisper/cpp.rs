//! whisper.cpp through dynamic FFI (`libloading`), model-resident.
//!
//! Donor: HeardRight `whisper_win_ffi.rs` (Windows, shipping), generalised to
//! macOS (Metal build of `libwhisper.dylib`) and Linux.
//!
//! ABI CONTRACT: `whisper_full_params` is a large non-opaque struct passed and
//! returned by value. The layouts below were verified field by field against
//! the v1.9.3 `include/whisper.h`; a mismatch corrupts fields silently, so
//! `load` refuses any library whose `whisper_version()` is not
//! [`PINNED_WHISPER_VERSION`]. To upgrade: rebuild from the new tag, re-diff
//! this file against that tag's `whisper.h`, then bump the pin.
use super::{Segment, WindowDecoder};
use crate::error::{Error, Result};
use std::ffi::{c_char, c_float, c_int, c_void, CStr, CString};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const PINNED_WHISPER_VERSION: &str = "1.9.3";

#[repr(C)]
struct WhisperContext {
    _private: [u8; 0],
}

type Strategy = c_int;
const GREEDY: Strategy = 0;

#[repr(C)]
struct FullParams {
    strategy: Strategy,
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
    vad_params: VadParams,
}

#[repr(C)]
struct VadParams {
    threshold: c_float,
    min_speech_duration_ms: c_int,
    min_silence_duration_ms: c_int,
    max_speech_duration_s: c_float,
    speech_pad_ms: c_int,
    samples_overlap: c_float,
}

type FnLoadModel = unsafe extern "C" fn(*const c_char) -> *mut WhisperContext;
type FnFree = unsafe extern "C" fn(*mut WhisperContext);
type FnDefaults = unsafe extern "C" fn(Strategy) -> FullParams;
type FnFull = unsafe extern "C" fn(*mut WhisperContext, FullParams, *const c_float, c_int) -> c_int;
type FnNSeg = unsafe extern "C" fn(*mut WhisperContext) -> c_int;
type FnSegText = unsafe extern "C" fn(*mut WhisperContext, c_int) -> *const c_char;
type FnSegTime = unsafe extern "C" fn(*mut WhisperContext, c_int) -> i64;
type FnLangId = unsafe extern "C" fn(*mut WhisperContext) -> c_int;
type FnLangStr = unsafe extern "C" fn(c_int) -> *const c_char;
type FnLangMaxId = unsafe extern "C" fn() -> c_int;
type FnVersion = unsafe extern "C" fn() -> *const c_char;

struct AbortFlag(AtomicBool);

extern "C" fn abort_trampoline(user_data: *mut c_void) -> bool {
    if user_data.is_null() {
        return false;
    }
    // SAFETY: always the `Arc<AbortFlag>` raw pointer set in `decode_window`,
    // kept alive for the whole `whisper_full` call.
    unsafe { &*(user_data as *const AbortFlag) }
        .0
        .load(Ordering::Relaxed)
}

pub struct WhisperCpp {
    _lib: libloading::Library,
    free: FnFree,
    defaults: FnDefaults,
    full: FnFull,
    n_seg: FnNSeg,
    seg_text: FnSegText,
    seg_t0: FnSegTime,
    seg_t1: FnSegTime,
    lang_id: FnLangId,
    lang_str: FnLangStr,
    lang_max_id: FnLangMaxId,
    ctx: *mut WhisperContext,
    threads: c_int,
    deadline: Option<Duration>,
}

// SAFETY: a whisper_context is used by one thread at a time (`&mut self`);
// fn pointers and the library handle are immutable after construction.
unsafe impl Send for WhisperCpp {}

impl WhisperCpp {
    /// Load the shared library and create a resident context from `model`.
    /// Never panics; any missing file/export/version mismatch is an `Err`.
    pub fn load(
        library: &Path,
        model: &Path,
        threads: usize,
        deadline: Option<Duration>,
    ) -> Result<Self> {
        if !model.is_file() {
            return Err(Error::Model(format!(
                "whisper ggml model not found: {}",
                model.display()
            )));
        }
        let lib = open_library(library)?;
        macro_rules! sym {
            ($name:literal, $ty:ty) => {{
                // SAFETY: symbol type is the one verified against whisper.h v1.9.3;
                // the library outlives the copied fn pointer (stored in Self).
                let s: libloading::Symbol<$ty> = unsafe { lib.get($name) }.map_err(|e| {
                    Error::Model(format!(
                        "resolve {}: {e}",
                        String::from_utf8_lossy(&$name[..$name.len() - 1])
                    ))
                })?;
                *s
            }};
        }
        let version: FnVersion = sym!(b"whisper_version\0", FnVersion);
        // SAFETY: static NUL-terminated string.
        let reported = unsafe { CStr::from_ptr(version()) }
            .to_string_lossy()
            .into_owned();
        // A build from the exact v1.9.3 tag may report `1.9.3-dev`; accept that
        // suffix of the pinned version, nothing else.
        let pinned_ok = reported == PINNED_WHISPER_VERSION
            || reported.strip_prefix(PINNED_WHISPER_VERSION).is_some_and(|r| r.starts_with('-'));
        if !pinned_ok {
            return Err(Error::Model(format!(
                "whisper library version {reported} does not match pinned {PINNED_WHISPER_VERSION} (ABI-checked)"
            )));
        }
        let open_model: FnLoadModel = sym!(b"whisper_init_from_file\0", FnLoadModel);
        let free: FnFree = sym!(b"whisper_free\0", FnFree);
        let defaults: FnDefaults = sym!(b"whisper_full_default_params\0", FnDefaults);
        let full: FnFull = sym!(b"whisper_full\0", FnFull);
        let n_seg: FnNSeg = sym!(b"whisper_full_n_segments\0", FnNSeg);
        let seg_text: FnSegText = sym!(b"whisper_full_get_segment_text\0", FnSegText);
        let seg_t0: FnSegTime = sym!(b"whisper_full_get_segment_t0\0", FnSegTime);
        let seg_t1: FnSegTime = sym!(b"whisper_full_get_segment_t1\0", FnSegTime);
        let lang_id: FnLangId = sym!(b"whisper_full_lang_id\0", FnLangId);
        let lang_str: FnLangStr = sym!(b"whisper_lang_str\0", FnLangStr);
        let lang_max_id: FnLangMaxId = sym!(b"whisper_lang_max_id\0", FnLangMaxId);

        let model_c = CString::new(model.to_string_lossy().as_bytes())
            .map_err(|e| Error::Config(format!("model path has NUL byte: {e}")))?;
        // SAFETY: resolved export; `model_c` outlives the call.
        let ctx = unsafe { open_model(model_c.as_ptr()) };
        if ctx.is_null() {
            return Err(Error::Model(format!(
                "whisper_init_from_file failed for {}",
                model.display()
            )));
        }
        let threads = if threads == 0 {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .clamp(1, 8)
        } else {
            threads
        } as c_int;
        Ok(Self {
            _lib: lib,
            free,
            defaults,
            full,
            n_seg,
            seg_text,
            seg_t0,
            seg_t1,
            lang_id,
            lang_str,
            lang_max_id,
            ctx,
            threads,
            deadline,
        })
    }

    fn lang_known(&self, code: &str) -> bool {
        // SAFETY: resolved exports; ids are within 0..=max_id.
        (0..=unsafe { (self.lang_max_id)() }).any(|i| {
            let p = unsafe { (self.lang_str)(i) };
            !p.is_null()
                && unsafe { CStr::from_ptr(p) }
                    .to_str()
                    .map(|s| s == code)
                    .unwrap_or(false)
        })
    }
}

#[cfg(windows)]
fn open_library(path: &Path) -> Result<libloading::Library> {
    use libloading::os::windows::{
        Library, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
    };
    // SAFETY: runs the library's initialisers; the caller supplies the staged
    // build output. Search flags keep sibling ggml DLL resolution
    // directory-local.
    unsafe {
        Library::load_with_flags(
            path,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        )
    }
    .map(Into::into)
    .map_err(|e| Error::Model(format!("load {}: {e}", path.display())))
}

#[cfg(not(windows))]
fn open_library(path: &Path) -> Result<libloading::Library> {
    // SAFETY: runs the library's initialisers; the caller supplies the staged
    // build output.
    unsafe { libloading::Library::new(path) }
        .map_err(|e| Error::Model(format!("load {}: {e}", path.display())))
}

impl WindowDecoder for WhisperCpp {
    fn decode_window(
        &mut self,
        samples: &[f32],
        lang: Option<&str>,
    ) -> Result<(Vec<Segment>, String)> {
        if let Some(code) = lang {
            if !self.lang_known(code) {
                return Err(Error::Config(format!(
                    "unknown Whisper language code `{code}`"
                )));
            }
        }
        let lang_c =
            CString::new(lang.unwrap_or("auto")).map_err(|e| Error::Config(e.to_string()))?;
        // SAFETY: resolved export; struct layout pinned (module docs).
        let mut p = unsafe { (self.defaults)(GREEDY) };
        p.n_threads = self.threads;
        p.translate = false;
        p.no_context = true;
        p.no_timestamps = false;
        p.single_segment = false;
        p.print_special = false;
        p.print_progress = false;
        p.print_realtime = false;
        p.print_timestamps = false;
        p.language = lang_c.as_ptr();
        // whisper_full performs AUTO detection itself when language is "auto";
        // `detect_language=true` is detect-only and returns before decoding.
        p.detect_language = false;
        p.greedy_best_of = 1;
        p.beam_search_beam_size = 1;

        let flag = Arc::new(AbortFlag(AtomicBool::new(false)));
        p.abort_callback = Some(abort_trampoline);
        p.abort_callback_user_data = Arc::as_ptr(&flag) as *mut c_void;
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let watchdog = self.deadline.map(|d| {
            let flag = Arc::clone(&flag);
            let limit = Instant::now() + d;
            std::thread::spawn(move || loop {
                if done_rx.recv_timeout(Duration::from_millis(100)).is_ok() {
                    return;
                }
                if Instant::now() >= limit {
                    flag.0.store(true, Ordering::Relaxed);
                    return;
                }
            })
        });
        // SAFETY: ctx valid until Drop; `samples` outlives the call.
        let rc = unsafe { (self.full)(self.ctx, p, samples.as_ptr(), samples.len() as c_int) };
        let _ = done_tx.send(());
        if let Some(w) = watchdog {
            let _ = w.join();
        }
        if flag.0.load(Ordering::Relaxed) {
            return Err(Error::Model(format!(
                "whisper_full aborted after deadline ({} samples)",
                samples.len()
            )));
        }
        if rc != 0 {
            return Err(Error::Model(format!("whisper_full returned {rc}")));
        }
        // SAFETY: exports resolved; ctx just decoded into; strings are owned by
        // ctx until the next decode.
        let n = unsafe { (self.n_seg)(self.ctx) };
        let mut segs = Vec::new();
        for i in 0..n {
            let ptr = unsafe { (self.seg_text)(self.ctx, i) };
            if ptr.is_null() {
                continue;
            }
            let text = unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .trim()
                .to_string();
            if text.is_empty() {
                continue;
            }
            let (t0, t1) = unsafe { ((self.seg_t0)(self.ctx, i), (self.seg_t1)(self.ctx, i)) };
            segs.push(Segment {
                text,
                start: t0 as f32 / 100.0,
                end: t1 as f32 / 100.0,
            });
        }
        let id = unsafe { (self.lang_id)(self.ctx) };
        let sp = unsafe { (self.lang_str)(id) };
        let used = match (lang, sp.is_null()) {
            (Some(l), _) => l.to_string(),
            (None, false) => unsafe { CStr::from_ptr(sp) }.to_string_lossy().into_owned(),
            (None, true) => "en".into(),
        };
        Ok((segs, used))
    }
}

impl Drop for WhisperCpp {
    fn drop(&mut self) {
        if !self.ctx.is_null() {
            // SAFETY: created by whisper_init_from_file; not used afterwards.
            unsafe { (self.free)(self.ctx) };
            self.ctx = std::ptr::null_mut();
        }
    }
}
