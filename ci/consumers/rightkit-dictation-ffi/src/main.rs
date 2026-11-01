// rightkit-dictation-ffi consumed by the exact registry pin in Cargo.toml, outside the workspace.
use std::ffi::{CStr, CString};

use rightkit_dictation_ffi::{
    rightkit_dictation_condition_for_asr, rightkit_dictation_polish, rightkit_dictation_string_free,
};

fn check(ok: bool, what: &str) -> bool {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
    }
    ok
}

fn main() {
    let mut all = true;
    let input = CString::new("email me at someone at example dot com").unwrap();
    let out = unsafe { rightkit_dictation_polish(input.as_ptr(), true) };
    all &= check(!out.is_null(), "polish returns text");
    if !out.is_null() {
        let text = unsafe { CStr::from_ptr(out) }
            .to_string_lossy()
            .into_owned();
        unsafe { rightkit_dictation_string_free(out) };
        all &= check(
            text.contains("someone@example.com"),
            "polish joins the address",
        );
    }
    let mut samples = vec![0.003f32; 1600];
    let ok = unsafe {
        rightkit_dictation_condition_for_asr(samples.as_mut_ptr(), samples.len(), 16_000)
    };
    all &= check(ok, "conditioning accepts a buffer");
    if !all {
        std::process::exit(1);
    }
}
