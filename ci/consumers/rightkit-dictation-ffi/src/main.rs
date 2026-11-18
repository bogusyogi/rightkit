// rightkit-dictation-ffi 0.2.1 consumed by exact registry pin (`=0.2.1`), outside the workspace.
//
// Proves, through the public API only, the FFI ownership contract, keyword-spotting file names,
// and brand-table configuration result codes:
// - RightKitOwnedBytes is the repr(C) pair the free function takes, and an empty value (null, 0)
//   is accepted by rightkit_dictation_owned_bytes_free, which is a no-op for null;
// - rightkit_dictation_string_free accepts null, as its contract allows;
// - kws::TOKENS_FILE names the tokens file the spotter reads;
// - the new brand table function rejects null/malformed input, accepts one valid table, and refuses replacement.
// No model is loaded, no keyword spotter or stream is created, no audio is read, no file is opened.
// No network, no process spawn. Exits 1 on the first failed check.
use std::ffi::CString;
use std::ptr;

use rightkit_dictation_ffi::kws::{ENCODER_FILE, TOKENS_FILE};
use rightkit_dictation_ffi::{
    rightkit_dictation_configure_brand_homophones_json, rightkit_dictation_owned_bytes_free,
    rightkit_dictation_string_free, RightKitOwnedBytes,
};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let empty = RightKitOwnedBytes {
        ptr: ptr::null_mut(),
        len: 0,
    };
    check(
        empty.ptr.is_null() && empty.len == 0,
        "the empty owned-bytes value is null with length 0",
    );
    // SAFETY: the value was not returned by the library and is null, which the free accepts as a no-op.
    unsafe { rightkit_dictation_owned_bytes_free(empty) };
    check(true, "freeing the null owned-bytes value is a no-op");

    // SAFETY: null is documented as accepted by the string free.
    unsafe { rightkit_dictation_string_free(ptr::null_mut()) };
    check(true, "freeing a null string is a no-op");

    check(TOKENS_FILE == "tokens.txt", "the spotter reads tokens.txt");
    check(
        ENCODER_FILE.ends_with(".onnx"),
        "the encoder is an ONNX model file",
    );

    // Configure once at end: the API installs a process-wide table and never replaces it.
    // SAFETY: null is an explicitly supported input.
    check(
        unsafe { rightkit_dictation_configure_brand_homophones_json(ptr::null()) } == 1,
        "null table is refused",
    );
    let malformed = CString::new("{").unwrap();
    // SAFETY: CString provides a valid NUL-terminated buffer for the call.
    check(
        unsafe { rightkit_dictation_configure_brand_homophones_json(malformed.as_ptr()) } == 2,
        "malformed table is refused",
    );
    let valid = CString::new(r#"{"brand":"Example","context_terms":[],"phrases":[{"phrase":"example phrase","guard":"always"}]}"#).unwrap();
    // SAFETY: CString provides a valid NUL-terminated UTF-8 buffer for the call.
    check(
        unsafe { rightkit_dictation_configure_brand_homophones_json(valid.as_ptr()) } == 0,
        "valid table is installed",
    );
    // SAFETY: the same valid buffer remains alive; the already-installed table is retained.
    check(
        unsafe { rightkit_dictation_configure_brand_homophones_json(valid.as_ptr()) } == 3,
        "second table is refused",
    );
    println!("OK rightkit-dictation-ffi 0.2.1 (registry consumer)");
}
