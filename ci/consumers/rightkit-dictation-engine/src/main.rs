// rightkit-dictation-engine consumed by the exact registry pin in Cargo.toml, outside the
// workspace. Public API only; no engine process, no model.
use rightkit_dictation_engine::file_transcribe::{permission_error, E_PERMISSION};
use rightkit_dictation_engine::vocabulary::{
    built_in_terms, configure_product_terms, ProductTerms,
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
    all &= check(
        permission_error(std::path::Path::new("a.wav")).starts_with(E_PERMISSION),
        "facade re-exports file transcription errors",
    );
    configure_product_terms(ProductTerms {
        built_in: &["NoteWell"],
        core: &["NoteWell"],
    });
    all &= check(
        built_in_terms() == ["NoteWell"],
        "facade re-exports the product vocabulary",
    );
    if !all {
        std::process::exit(1);
    }
}
