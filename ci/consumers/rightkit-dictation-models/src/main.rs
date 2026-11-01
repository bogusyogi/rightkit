// rightkit-dictation-models consumed by the exact registry pin in Cargo.toml, outside the
// workspace. Public API only; no network, no files.
use rightkit_dictation_models::{ane_cache_prune, model_paths, shared};

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
    all &= check(!shared::APP_ID.is_empty(), "store owner id is set");
    all &= check(
        model_paths::WINDOWS_TDT_CURRENT_SUBDIR != model_paths::WINDOWS_TDT_PREVIOUS_SUBDIR,
        "current and previous Windows TDT directories differ",
    );
    all &= check(
        ane_cache_prune::configure_bundle_identifier("com.example.consumer")
            && !ane_cache_prune::configure_bundle_identifier("com.example.other"),
        "bundle identifier is configured exactly once",
    );
    if !all {
        std::process::exit(1);
    }
}
