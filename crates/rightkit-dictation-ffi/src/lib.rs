//! Stable C surface for native Apple shells. It keeps native shells on the exact same
//! implementation desktop hosts call instead of maintaining regex copies in Swift.

#[cfg(feature = "kws")]
pub mod kws;

use std::ffi::{c_char, CStr, CString};
use std::sync::OnceLock;

use rightkit_text_polish::text_pipeline::{BrandHomophones, HomophoneGuard};

/// The process-wide brand homophone table for [`rightkit_dictation_polish`], installed once by
/// [`rightkit_dictation_configure_brand_homophones_json`]. Without it polish applies no brand
/// repair, exactly as in 0.2.0.
static BRAND_HOMOPHONES: OnceLock<BrandHomophones<'static>> = OnceLock::new();

/// Owned UTF-8 bytes returned by a shared-core FFI function. The buffer is
/// released with `rightkit_dictation_owned_bytes_free`; callers must not use a
/// Rust allocator from Swift or Objective-C.
#[repr(C)]
pub struct RightKitOwnedBytes {
    pub ptr: *mut u8,
    pub len: usize,
}

fn empty_owned_bytes() -> RightKitOwnedBytes {
    RightKitOwnedBytes {
        ptr: std::ptr::null_mut(),
        len: 0,
    }
}

fn owned_utf8(value: String) -> RightKitOwnedBytes {
    if value.is_empty() {
        return empty_owned_bytes();
    }
    let mut bytes = value.into_bytes().into_boxed_slice();
    let result = RightKitOwnedBytes {
        ptr: bytes.as_mut_ptr(),
        len: bytes.len(),
    };
    std::mem::forget(bytes);
    result
}

fn capitalize_first_alpha(input: &str) -> String {
    let Some((index, first)) = input
        .char_indices()
        .find(|(_, value)| value.is_alphabetic())
    else {
        return input.to_string();
    };
    let upper = first.to_uppercase().to_string();
    if upper == first.to_string() {
        return input.to_string();
    }
    let mut output = input.to_string();
    output.replace_range(index..index + first.len_utf8(), &upper);
    output
}

/// Returns newly allocated UTF-8. Caller releases it with
/// `rightkit_dictation_string_free`. Invalid UTF-8/null input returns null.
///
/// # Safety
///
/// `input` must be null or point to a valid NUL-terminated C string that stays
/// valid for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn rightkit_dictation_polish(
    input: *const c_char,
    aggressive: bool,
) -> *mut c_char {
    if input.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(input) = unsafe { CStr::from_ptr(input) }.to_str() else {
        return std::ptr::null_mut();
    };
    let deterministic = rightkit_text_polish::text_pipeline::deterministic_polish_with_brand(
        input,
        BRAND_HOMOPHONES.get(),
    );
    let local = if aggressive {
        capitalize_first_alpha(
            &rightkit_text_polish::text_pipeline::aggressive_speech_cleanup(&deterministic),
        )
    } else {
        deterministic
    };
    let tightened = rightkit_text_polish::text_pipeline::tighten_domain_spacing(&local);
    let output = rightkit_text_polish::text_pipeline::visual_chunk_transcript(&tightened);
    CString::new(output).map_or(std::ptr::null_mut(), CString::into_raw)
}

/// Releases a string returned by this library's `*mut c_char` functions.
///
/// # Safety
///
/// `value` must be null or a pointer previously returned by one of this
/// library's string-returning functions, and must not be freed twice.
#[no_mangle]
pub unsafe extern "C" fn rightkit_dictation_string_free(value: *mut c_char) {
    if !value.is_null() {
        drop(unsafe { CString::from_raw(value) });
    }
}

/// Conditions mono f32 samples in place through the desktop pipeline's canonical ASR policy.
/// Returns false only when a non-empty buffer has no storage.
///
/// # Safety
///
/// `samples` must be null or point to `len` initialized, writable `f32` values
/// that nothing else accesses during the call.
#[no_mangle]
pub unsafe extern "C" fn rightkit_dictation_condition_for_asr(
    samples: *mut f32,
    len: usize,
    sample_rate: u32,
) -> bool {
    if len == 0 {
        return true;
    }
    if samples.is_null() {
        return false;
    }
    let input = unsafe { std::slice::from_raw_parts(samples, len) };
    let output = rightkit_dictation_core::audio_conditioning::condition_for_asr(
        input,
        sample_rate,
        "asr_simple_gain_hpf",
    );
    unsafe { std::ptr::copy_nonoverlapping(output.as_ptr(), samples, len) };
    true
}

/// Routes the desktop pipeline's final-ASR transform suffix grammar. `mode` receives 1 for
/// normal polish, 2 for `prompt`, or 3 for `summarize`.
///
/// # Safety
///
/// `input` must be null or point to a valid NUL-terminated C string, and `mode`
/// must be null or point to a writable `u8`, both valid for the call.
#[no_mangle]
pub unsafe extern "C" fn rightkit_dictation_route_ai_transform(
    input: *const c_char,
    mode: *mut u8,
) -> *mut c_char {
    if input.is_null() || mode.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(input) = unsafe { CStr::from_ptr(input) }.to_str() else {
        return std::ptr::null_mut();
    };
    let (text, value) = match rightkit_text_polish::text_pipeline::parse_ai_transform_command(input)
    {
        Some(command) => {
            let value = match command.intent {
                rightkit_text_polish::text_pipeline::AiTransformIntent::Prompt => 2,
                rightkit_text_polish::text_pipeline::AiTransformIntent::Summarize => 3,
            };
            (command.clean_text, value)
        }
        None => (input.to_string(), 1),
    };
    unsafe { *mode = value };
    CString::new(text).map_or(std::ptr::null_mut(), CString::into_raw)
}

/// Detokenize a UTF-8 JSON array of raw SentencePiece vocabulary pieces with
/// the one canonical Rust policy used by desktop decoders. JSON keeps this
/// boundary low-frequency: it is called once after a completed decode, never
/// inside recurrent beam search.
///
/// Returns zero on success. Return codes: 1 invalid pointers, 2 malformed
/// UTF-8/JSON, 3 output pointer was null. The output is released with
/// `rightkit_dictation_owned_bytes_free`.
///
/// # Safety
///
/// `json_ptr` must be null (only with `json_len == 0`) or point to `json_len`
/// readable bytes, and `out_text` must be null or point to a writable
/// `RightKitOwnedBytes`.
#[no_mangle]
pub unsafe extern "C" fn rightkit_dictation_detokenize_json(
    json_ptr: *const u8,
    json_len: usize,
    out_text: *mut RightKitOwnedBytes,
) -> i32 {
    if out_text.is_null() {
        return 3;
    }
    unsafe { *out_text = empty_owned_bytes() };
    if json_ptr.is_null() && json_len != 0 {
        return 1;
    }
    let bytes = if json_len == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(json_ptr, json_len) }
    };
    let pieces: Vec<String> = match serde_json::from_slice(bytes) {
        Ok(pieces) => pieces,
        Err(_) => return 2,
    };
    let decoded: Vec<rightkit_asr::parakeet_core::DecodedPiece> = pieces
        .into_iter()
        .enumerate()
        .map(
            |(token_id, piece)| rightkit_asr::parakeet_core::DecodedPiece {
                token_id,
                piece,
                start: 0.0,
                end: 0.0,
            },
        )
        .collect();
    let stripped = rightkit_asr::parakeet_core::strip_mandatory_fillers(&decoded);
    let text = rightkit_asr::parakeet_core::assemble_pieces(
        stripped.iter().map(|piece| piece.piece.as_str()),
        rightkit_asr::parakeet_core::DetokPolicy::canonical_rust(),
    )
    .text;
    unsafe { *out_text = owned_utf8(text) };
    0
}

/// Releases bytes returned by `rightkit_dictation_detokenize_json`.
///
/// # Safety
///
/// `bytes` must be a value produced by `rightkit_dictation_detokenize_json` (or an
/// empty/null-pointer value) and must not be released twice.
#[no_mangle]
pub unsafe extern "C" fn rightkit_dictation_owned_bytes_free(bytes: RightKitOwnedBytes) {
    if bytes.ptr.is_null() {
        return;
    }
    let raw = std::ptr::slice_from_raw_parts_mut(bytes.ptr, bytes.len);
    unsafe { drop(Box::from_raw(raw)) };
}

/// Removes only measured residue left when confirmed KWS cuts exactly at its
/// onset. This must be called only for a forced KWS onset cut; ordinary text
/// stays byte-for-byte intact. Returns newly allocated UTF-8 released with
/// `rightkit_dictation_string_free`.
///
/// # Safety
///
/// `input` must be null or point to a valid NUL-terminated C string that stays
/// valid for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn rightkit_dictation_strip_kws_onset_cut_residue(
    input: *const c_char,
) -> *mut c_char {
    if input.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(input) = unsafe { CStr::from_ptr(input) }.to_str() else {
        return std::ptr::null_mut();
    };
    let output = rightkit_text_polish::text_pipeline::strip_kws_onset_cut_residue(input);
    CString::new(output).map_or(std::ptr::null_mut(), CString::into_raw)
}

// Parsing owns all allocations until the whole document has passed validation.
// Only OnceLock's winning initializer converts that validated table to static storage.
struct OwnedBrand {
    brand: String,
    context_terms: Vec<String>,
    phrases: Vec<(String, OwnedGuard)>,
}

enum OwnedGuard {
    Always,
    NeedsBrandContext,
    LiteralBefore(Vec<String>),
    LiteralAfter(Vec<String>),
}

fn string_list(value: &serde_json::Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|word| Some(word.as_str()?.to_owned()))
        .collect()
}

fn parse_guard(value: &serde_json::Value) -> Option<OwnedGuard> {
    if let Some(kind) = value.as_str() {
        return match kind {
            "always" => Some(OwnedGuard::Always),
            "needs_brand_context" => Some(OwnedGuard::NeedsBrandContext),
            _ => None,
        };
    }
    let object = value.as_object()?;
    if object.len() != 1 {
        return None;
    }
    if let Some(words) = object.get("literal_before") {
        return Some(OwnedGuard::LiteralBefore(string_list(words)?));
    }
    if let Some(words) = object.get("literal_after") {
        return Some(OwnedGuard::LiteralAfter(string_list(words)?));
    }
    None
}

fn parse_brand(json: &[u8]) -> Option<OwnedBrand> {
    let root: serde_json::Value = serde_json::from_slice(json).ok()?;
    let object = root.as_object()?;
    let brand = object.get("brand")?.as_str()?.to_owned();
    let context_terms = string_list(object.get("context_terms")?)?;
    let phrases = object
        .get("phrases")?
        .as_array()?
        .iter()
        .map(|entry| {
            Some((
                entry.get("phrase")?.as_str()?.to_owned(),
                parse_guard(entry.get("guard")?)?,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(OwnedBrand {
        brand,
        phrases,
        context_terms,
    })
}

fn static_string(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}

fn static_strings(values: Vec<String>) -> &'static [&'static str] {
    values
        .into_iter()
        .map(static_string)
        .collect::<Vec<_>>()
        .leak()
}

impl OwnedBrand {
    fn into_static(self) -> BrandHomophones<'static> {
        let phrases = self
            .phrases
            .into_iter()
            .map(|(phrase, guard)| {
                let guard = match guard {
                    OwnedGuard::Always => HomophoneGuard::Always,
                    OwnedGuard::NeedsBrandContext => HomophoneGuard::NeedsBrandContext,
                    OwnedGuard::LiteralBefore(words) => {
                        HomophoneGuard::LiteralBefore(static_strings(words))
                    }
                    OwnedGuard::LiteralAfter(words) => {
                        HomophoneGuard::LiteralAfter(static_strings(words))
                    }
                };
                (static_string(phrase), guard)
            })
            .collect::<Vec<_>>()
            .leak();
        BrandHomophones {
            brand: static_string(self.brand),
            phrases,
            context_terms: static_strings(self.context_terms),
        }
    }
}

/// Installs the brand homophone table `rightkit_dictation_polish` applies after the lexicon, so a
/// product name that ASR splits into ordinary words ("example name") is repaired to its spelling.
/// Call once at startup, before the first `rightkit_dictation_polish`; the table is process-wide.
///
/// `json` is a NUL-terminated UTF-8 object:
/// `{"brand": "Name", "context_terms": ["app", ...], "phrases": [{"phrase": "two words",
/// "guard": G}, ...]}` where `G` is `"always"`, `"needs_brand_context"`,
/// `{"literal_before": ["word", ...]}` or `{"literal_after": ["word", ...]}`. Phrases and
/// context terms are lowercase.
///
/// Returns 0 when installed, 1 when `json` is null, 2 when it is not valid UTF-8 or does not
/// have the shape above (nothing is installed), 3 when a table is already installed (the first
/// one stays). NULL returns 1 even after installation. Malformed input & losing concurrent
/// attempts release their temporary allocations. Only one fully validated table retains
/// process-lifetime storage; callers keep ownership of their input buffer.
///
/// # Safety
///
/// `json` must be null or point to a valid NUL-terminated C string that stays valid for the
/// duration of the call.
#[no_mangle]
pub unsafe extern "C" fn rightkit_dictation_configure_brand_homophones_json(
    json: *const c_char,
) -> i32 {
    if json.is_null() {
        return 1;
    }
    if BRAND_HOMOPHONES.get().is_some() {
        return 3;
    }
    let bytes = unsafe { CStr::from_ptr(json) }.to_bytes();
    if std::str::from_utf8(bytes).is_err() {
        return 2;
    }
    let Some(table) = parse_brand(bytes) else {
        return 2;
    };
    let mut installed = false;
    BRAND_HOMOPHONES.get_or_init(|| {
        installed = true;
        table.into_static()
    });
    if installed {
        0
    } else {
        3
    }
}
