// rightkit-dictation-text-pipeline consumed by the exact registry pin in Cargo.toml, outside the
// workspace. Public API only; in-memory values only.
use rightkit_dictation_text_pipeline::vocabulary::{
    built_in_terms, configure_product_terms, core_product_terms, ProductTerms,
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
        built_in_terms().is_empty(),
        "no product terms until configured",
    );
    configure_product_terms(ProductTerms {
        built_in: &["NoteWell", "NoteWell stop"],
        core: &["NoteWell"],
    });
    all &= check(
        built_in_terms().len() == 2 && core_product_terms() == ["NoteWell"],
        "configured product terms read back",
    );
    if !all {
        std::process::exit(1);
    }
}
