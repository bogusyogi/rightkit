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
//!
//! Symbols added in 0.2.0 each carry the exact `whisper.h` declaration (v1.9.3,
//! fetched from the `v1.9.3` tag) in a comment on their type alias. Scalar
//! accessors are used for token data; `whisper_full_get_token_data` (returns a
//! 56-byte struct by value) is deliberately not bound.
use super::{LanguageDetection, Segment, WindowDecoder, WindowOutput, WindowRequest, WordSpan};
use crate::error::{Error, Result};
use std::ffi::{c_char, c_float, c_int, c_void, CStr, CString};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const PINNED_WHISPER_VERSION: &str = "1.9.3";

/// whisper.cpp's input rate. Centisecond timestamps (10 ms) are exact multiples
/// of 160 samples, so token and segment sample positions need no float math.
const SAMPLES_PER_CS: u64 = 160;
/// Samples of the first whisper window (30 s at 16 kHz).
const DETECT_SAMPLES: usize = 480_000;

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

// ---- Symbols added in 0.2.0 (declarations from whisper.h v1.9.3) ----------

/// `WHISPER_API int whisper_pcm_to_mel(struct whisper_context * ctx, const float * samples, int n_samples, int n_threads);` (whisper.h:276-280)
type FnPcmToMel = unsafe extern "C" fn(*mut WhisperContext, *const c_float, c_int, c_int) -> c_int;
/// `WHISPER_API int whisper_lang_auto_detect(struct whisper_context * ctx, int offset_ms, int n_threads, float * lang_probs);` (whisper.h:378-382)
type FnLangAutoDetect = unsafe extern "C" fn(*mut WhisperContext, c_int, c_int, *mut c_float) -> c_int;
/// `WHISPER_API whisper_token whisper_token_eot (struct whisper_context * ctx);` (whisper.h:424); `whisper_token` is `int32_t` (whisper.h:85).
type FnTokenEot = unsafe extern "C" fn(*mut WhisperContext) -> i32;
/// `WHISPER_API int whisper_full_n_tokens (struct whisper_context * ctx, int i_segment);` (whisper.h:655)
type FnSegNTokens = unsafe extern "C" fn(*mut WhisperContext, c_int) -> c_int;
/// `WHISPER_API const char * whisper_full_get_token_text (struct whisper_context * ctx, int i_segment, int i_token);` (whisper.h:659)
type FnTokText = unsafe extern "C" fn(*mut WhisperContext, c_int, c_int) -> *const c_char;
/// `WHISPER_API whisper_token whisper_full_get_token_id (struct whisper_context * ctx, int i_segment, int i_token);` (whisper.h:662)
type FnTokId = unsafe extern "C" fn(*mut WhisperContext, c_int, c_int) -> i32;
/// `WHISPER_API int64_t whisper_full_get_token_t0 (struct whisper_context * ctx, int i_segment, int i_token);` (whisper.h:675), centiseconds.
/// `WHISPER_API int64_t whisper_full_get_token_t1 (struct whisper_context * ctx, int i_segment, int i_token);` (whisper.h:677), centiseconds.
type FnTokTime = unsafe extern "C" fn(*mut WhisperContext, c_int, c_int) -> i64;
/// `WHISPER_API float whisper_full_get_token_p (struct whisper_context * ctx, int i_segment, int i_token);` (whisper.h:681)
type FnTokP = unsafe extern "C" fn(*mut WhisperContext, c_int, c_int) -> c_float;

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
    pcm_to_mel: FnPcmToMel,
    lang_auto_detect: FnLangAutoDetect,
    token_eot: FnTokenEot,
    seg_n_tokens: FnSegNTokens,
    tok_text: FnTokText,
    tok_id: FnTokId,
    tok_t0: FnTokTime,
    tok_t1: FnTokTime,
    tok_p: FnTokP,
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
        let pcm_to_mel: FnPcmToMel = sym!(b"whisper_pcm_to_mel\0", FnPcmToMel);
        let lang_auto_detect: FnLangAutoDetect = sym!(b"whisper_lang_auto_detect\0", FnLangAutoDetect);
        let token_eot: FnTokenEot = sym!(b"whisper_token_eot\0", FnTokenEot);
        let seg_n_tokens: FnSegNTokens = sym!(b"whisper_full_n_tokens\0", FnSegNTokens);
        let tok_text: FnTokText = sym!(b"whisper_full_get_token_text\0", FnTokText);
        let tok_id: FnTokId = sym!(b"whisper_full_get_token_id\0", FnTokId);
        let tok_t0: FnTokTime = sym!(b"whisper_full_get_token_t0\0", FnTokTime);
        let tok_t1: FnTokTime = sym!(b"whisper_full_get_token_t1\0", FnTokTime);
        let tok_p: FnTokP = sym!(b"whisper_full_get_token_p\0", FnTokP);

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
            pcm_to_mel,
            lang_auto_detect,
            token_eot,
            seg_n_tokens,
            tok_text,
            tok_id,
            tok_t0,
            tok_t1,
            tok_p,
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

    fn lang_name(&self, id: c_int) -> Option<String> {
        // SAFETY: resolved export; returns a static string or null.
        let p = unsafe { (self.lang_str)(id) };
        (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    }

    /// Language identification on the first 30 s of `samples`: `whisper_pcm_to_mel`
    /// then `whisper_lang_auto_detect` (whisper.h:276-280, :378-382). No text
    /// decode runs. Takes `&self`: the context is reached only through its raw
    /// pointer and `Whisper` is not `Sync`, so no other thread can touch it.
    /// The deadline set at load time is not applied to this call.
    pub(crate) fn detect_pcm(&self, samples: &[f32]) -> Result<LanguageDetection> {
        if samples.is_empty() {
            return Err(Error::Audio("no samples to detect the language from".into()));
        }
        let n = samples.len().min(DETECT_SAMPLES);
        // SAFETY: ctx valid until Drop; `samples[..n]` outlives the call.
        let rc = unsafe { (self.pcm_to_mel)(self.ctx, samples.as_ptr(), n as c_int, self.threads) };
        if rc != 0 {
            return Err(Error::Model(format!("whisper_pcm_to_mel returned {rc}")));
        }
        // SAFETY: resolved export, no arguments.
        let max_id = unsafe { (self.lang_max_id)() };
        if max_id < 0 {
            return Err(Error::Model("whisper_lang_max_id returned a negative id".into()));
        }
        let count = max_id as usize + 1;
        let mut probs = vec![0f32; count];
        // SAFETY: `probs` holds whisper_lang_max_id()+1 floats, as the header requires.
        let top = unsafe { (self.lang_auto_detect)(self.ctx, 0, self.threads, probs.as_mut_ptr()) };
        if top < 0 || top as usize >= count {
            return Err(Error::Model(format!(
                "whisper_lang_auto_detect returned {top} (the model may not be multilingual)"
            )));
        }
        let mut candidates: Vec<(String, f32)> = (0..count)
            .map(|i| (self.lang_name(i as c_int).unwrap_or_default(), probs[i]))
            .collect();
        candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        Ok(LanguageDetection {
            lang: self.lang_name(top).unwrap_or_default(),
            probability: probs[top as usize],
            candidates,
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

/// Accumulates one word's token bytes. Bytes, not `String`: a multi-byte
/// character can span two tokens, so decoding happens once per word.
struct WordAcc {
    bytes: Vec<u8>,
    start_cs: i64,
    end_cs: i64,
    p_sum: f32,
    n: u32,
}

impl WindowDecoder for WhisperCpp {
    fn decode_window(&mut self, samples: &[f32], req: &WindowRequest<'_>) -> Result<WindowOutput> {
        if let Some(code) = req.lang {
            if !self.lang_known(code) {
                return Err(Error::Config(format!(
                    "unknown Whisper language code `{code}`"
                )));
            }
        }
        let lang_c =
            CString::new(req.lang.unwrap_or("auto")).map_err(|e| Error::Config(e.to_string()))?;
        // SAFETY: resolved export; struct layout pinned (module docs).
        let mut p = unsafe { (self.defaults)(GREEDY) };
        p.n_threads = self.threads;
        p.translate = false;
        // Text-only and timed modes differ only here. `no_context` is always set:
        // windows are disjoint and must not condition on each other.
        p.no_context = true;
        p.no_timestamps = !req.timed;
        p.token_timestamps = req.timed && req.token_timestamps;
        p.max_len = req.max_len as c_int;
        p.split_on_word = req.split_on_word;
        p.single_segment = false;
        p.print_special = false;
        p.print_progress = false;
        p.print_realtime = false;
        p.print_timestamps = false;
        p.language = lang_c.as_ptr();
        // Stays false on purpose. whisper.cpp returns from `whisper_full` before
        // decoding when this is set (existing 0.1.0 finding, re-confirmed in 0.2.0
        // design). Language identification is done by `detect_pcm` instead.
        p.detect_language = false;
        p.greedy_best_of = 1;
        p.beam_search_beam_size = 1;
        // Copy before `p` moves into `whisper_full` below.
        let token_timestamps = p.token_timestamps;

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
        let max_sample = samples.len() as u64;
        // SAFETY: exports resolved; ctx just decoded into; strings are owned by
        // ctx until the next decode.
        let n = unsafe { (self.n_seg)(self.ctx) };
        let eot = unsafe { (self.token_eot)(self.ctx) };
        let mut segs = Vec::new();
        let mut words = Vec::new();
        for i in 0..n {
            let ptr = unsafe { (self.seg_text)(self.ctx, i) };
            if ptr.is_null() {
                continue;
            }
            let text = unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .trim()
                .to_string();
            let (t0, t1) = unsafe { ((self.seg_t0)(self.ctx, i), (self.seg_t1)(self.ctx, i)) };
            if text.is_empty() {
                continue;
            }
            let start_sample = cs_to_sample(t0, max_sample);
            let end_sample = cs_to_sample(t1, max_sample).max(start_sample);
            segs.push(Segment {
                text,
                start: t0 as f32 / 100.0,
                end: t1 as f32 / 100.0,
                start_sample,
                end_sample,
            });
            if token_timestamps {
                self.collect_words(i, eot, max_sample, &mut words);
            }
        }
        let id = unsafe { (self.lang_id)(self.ctx) };
        let used = match req.lang {
            Some(l) => l.to_string(),
            None => self.lang_name(id).unwrap_or_else(|| "en".into()),
        };
        Ok(WindowOutput { segments: segs, words, language: used })
    }

    fn detect_window(&mut self, samples: &[f32]) -> Result<LanguageDetection> {
        self.detect_pcm(samples)
    }
}

impl WhisperCpp {
    /// Assemble words from one segment's tokens. Special tokens (id >= EOT:
    /// SOT, language, task, timestamp tokens) are skipped. A token whose text
    /// begins with a space starts a new word (Whisper's BPE convention); other
    /// tokens extend the current word. Word time = first token start to last
    /// token end; probability = mean token probability.
    fn collect_words(&self, seg: c_int, eot: i32, max_sample: u64, out: &mut Vec<WordSpan>) {
        let mut cur: Option<WordAcc> = None;
        // SAFETY: called right after whisper_full on the same context.
        let ntok = unsafe { (self.seg_n_tokens)(self.ctx, seg) };
        let flush = |acc: WordAcc, out: &mut Vec<WordSpan>| {
            let text = String::from_utf8_lossy(&acc.bytes).trim().to_string();
            if text.is_empty() {
                return;
            }
            let start_sample = cs_to_sample(acc.start_cs, max_sample);
            out.push(WordSpan {
                text,
                start_sample,
                end_sample: cs_to_sample(acc.end_cs, max_sample).max(start_sample),
                probability: acc.p_sum / acc.n.max(1) as f32,
            });
        };
        for j in 0..ntok {
            // SAFETY: indices are within 0..n_tokens for this segment.
            let id = unsafe { (self.tok_id)(self.ctx, seg, j) };
            if id >= eot {
                continue;
            }
            let ptr = unsafe { (self.tok_text)(self.ctx, seg, j) };
            if ptr.is_null() {
                continue;
            }
            let bytes = unsafe { CStr::from_ptr(ptr) }.to_bytes();
            if bytes.is_empty() {
                continue;
            }
            let (t0, t1, p) = unsafe {
                (
                    (self.tok_t0)(self.ctx, seg, j),
                    (self.tok_t1)(self.ctx, seg, j),
                    (self.tok_p)(self.ctx, seg, j),
                )
            };
            if bytes[0] == b' ' || cur.is_none() {
                if let Some(acc) = cur.take() {
                    flush(acc, out);
                }
                cur = Some(WordAcc { bytes: Vec::new(), start_cs: t0, end_cs: t1, p_sum: 0.0, n: 0 });
            }
            if let Some(acc) = cur.as_mut() {
                acc.bytes.extend_from_slice(bytes);
                acc.end_cs = t1;
                acc.p_sum += p;
                acc.n += 1;
            }
        }
        if let Some(acc) = cur.take() {
            flush(acc, out);
        }
    }
}

/// Centiseconds (window-relative) to a window-relative sample index, clamped
/// to the window. Exact: 1 cs = 160 samples at 16 kHz.
fn cs_to_sample(cs: i64, max_sample: u64) -> u64 {
    (cs.max(0) as u64).saturating_mul(SAMPLES_PER_CS).min(max_sample)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Resolves every 0.2.0 symbol in the pinned library and checks the
    /// language table length against it. Skips (printing SKIP) when
    /// `RIGHTKIT_WHISPER_LIB` is unset; the e2e suite is the hard gate.
    #[test]
    fn new_symbols_resolve_in_pinned_library() {
        let Some(lib_path) = std::env::var_os("RIGHTKIT_WHISPER_LIB") else {
            eprintln!("SKIP: RIGHTKIT_WHISPER_LIB not set");
            return;
        };
        let lib = open_library(Path::new(&lib_path)).expect("open whisper library");
        for name in [
            &b"whisper_pcm_to_mel\0"[..],
            b"whisper_lang_auto_detect\0",
            b"whisper_token_eot\0",
            b"whisper_full_n_tokens\0",
            b"whisper_full_get_token_text\0",
            b"whisper_full_get_token_id\0",
            b"whisper_full_get_token_t0\0",
            b"whisper_full_get_token_t1\0",
            b"whisper_full_get_token_p\0",
        ] {
            let found = unsafe { lib.get::<*const c_void>(name) };
            assert!(found.is_ok(), "missing export {}", String::from_utf8_lossy(name));
        }
        let max_id = unsafe { lib.get::<FnLangMaxId>(b"whisper_lang_max_id\0") }
            .map(|f| unsafe { f() })
            .expect("whisper_lang_max_id");
        assert_eq!(max_id as usize + 1, crate::whisper::languages::WHISPER_LANGUAGES.len());
    }

    #[test]
    fn centiseconds_map_to_exact_samples_and_clamp() {
        assert_eq!(cs_to_sample(0, 1_000_000), 0);
        assert_eq!(cs_to_sample(1, 1_000_000), 160);
        assert_eq!(cs_to_sample(57, 1_000_000), 9_120);
        assert_eq!(cs_to_sample(-5, 1_000_000), 0);
        assert_eq!(cs_to_sample(100_000, 480_000), 480_000);
    }
}
