//! Stable C surface for native Apple shells. It keeps native shells on the exact same
//! implementation desktop hosts call instead of maintaining regex copies in Swift.

pub mod kws;

use std::ffi::{c_char, CStr, CString};

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
    let deterministic = rightkit_text_polish::text_pipeline::deterministic_polish(input);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffi_matches_shared_pipeline() {
        // No leading "um" — see the N0 filler policy in `rightkit_asr::parakeet_core::detok`. This FFI
        // is a text-in surface, so anything reaching it has already passed N0.
        let input = CString::new("email me at adrian at example dot com").unwrap();
        let output = unsafe { rightkit_dictation_polish(input.as_ptr(), true) };
        assert!(!output.is_null());
        let text = unsafe { CStr::from_ptr(output) }
            .to_string_lossy()
            .into_owned();
        unsafe { rightkit_dictation_string_free(output) };
        assert_eq!(text, "Email me at adrian@example.com");
    }

    #[test]
    fn ffi_conditions_audio_through_shared_policy() {
        let mut input: Vec<f32> = (0..1600)
            .map(|index| if index % 2 == 0 { 0.003 } else { -0.003 })
            .collect();
        let before = rightkit_dictation_core::audio_conditioning::rms(&input);
        let ok = unsafe {
            rightkit_dictation_condition_for_asr(input.as_mut_ptr(), input.len(), 16_000)
        };
        assert!(ok);
        assert!(rightkit_dictation_core::audio_conditioning::rms(&input) > before * 2.0);
    }

    #[test]
    fn ffi_routes_transform_tail_through_shared_parser() {
        let input = CString::new("Turn this into a useful request prompt").unwrap();
        let mut mode = 0;
        let output = unsafe { rightkit_dictation_route_ai_transform(input.as_ptr(), &mut mode) };
        assert_eq!(mode, 2);
        let text = unsafe { CStr::from_ptr(output) }
            .to_string_lossy()
            .into_owned();
        unsafe { rightkit_dictation_string_free(output) };
        assert_eq!(text, "Turn this into a useful request");
    }

    #[test]
    fn ffi_detokenizes_with_canonical_rust_policy() {
        let json = r#"["▁at","60","▁hello","▁uh",",","▁world","<pad>"]"#.as_bytes();
        let mut output = RightKitOwnedBytes {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        let status =
            unsafe { rightkit_dictation_detokenize_json(json.as_ptr(), json.len(), &mut output) };
        assert_eq!(status, 0);
        let text = unsafe {
            std::str::from_utf8(std::slice::from_raw_parts(output.ptr, output.len)).unwrap()
        };
        assert_eq!(text, "at 60 hello world");
        unsafe { rightkit_dictation_owned_bytes_free(output) };
    }

    #[test]
    fn ffi_strip_onset_residue_preserves_ordinary_words() {
        let input = CString::new("keep this sentence zeph s").unwrap();
        let output = unsafe { rightkit_dictation_strip_kws_onset_cut_residue(input.as_ptr()) };
        let text = unsafe { CStr::from_ptr(output) }.to_str().unwrap();
        assert_eq!(text, "keep this sentence");
        unsafe { rightkit_dictation_string_free(output) };

        let ordinary = CString::new("keep this sentence is ordinary").unwrap();
        let unchanged =
            unsafe { rightkit_dictation_strip_kws_onset_cut_residue(ordinary.as_ptr()) };
        let unchanged_text = unsafe { CStr::from_ptr(unchanged) }.to_str().unwrap();
        assert_eq!(unchanged_text, "keep this sentence is ordinary");
        unsafe { rightkit_dictation_string_free(unchanged) };
    }
}
