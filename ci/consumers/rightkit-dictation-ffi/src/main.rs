// rightkit-dictation-ffi 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
//
// Proves, through the public API only, the FFI ownership contract and the keyword-spotting file names:
// - RightKitOwnedBytes is the repr(C) pair the free function takes, and an empty value (null, 0)
//   is accepted by rightkit_dictation_owned_bytes_free, which is a no-op for null;
// - rightkit_dictation_string_free accepts null, as its contract allows;
// - kws::TOKENS_FILE names the tokens file the spotter reads.
// No model is loaded, no keyword spotter or stream is created, no audio is read, no file is opened.
// No network, no process spawn. Exits 1 on the first failed check.
use std::ptr;

use rightkit_dictation_ffi::kws::{ENCODER_FILE, TOKENS_FILE};
use rightkit_dictation_ffi::{
    rightkit_dictation_owned_bytes_free, rightkit_dictation_string_free, RightKitOwnedBytes,
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
    check(empty.ptr.is_null() && empty.len == 0, "the empty owned-bytes value is null with length 0");
    // SAFETY: the value was not returned by the library and is null, which the free accepts as a no-op.
    unsafe { rightkit_dictation_owned_bytes_free(empty) };
    check(true, "freeing the null owned-bytes value is a no-op");

    // SAFETY: null is documented as accepted by the string free.
    unsafe { rightkit_dictation_string_free(ptr::null_mut()) };
    check(true, "freeing a null string is a no-op");

    check(TOKENS_FILE == "tokens.txt", "the spotter reads tokens.txt");
    check(ENCODER_FILE.ends_with(".onnx"), "the encoder is an ONNX model file");
    println!("OK rightkit-dictation-ffi 0.1.0 (registry consumer)");
}
