// rightkit-text-polish consumed by the exact registry pin in Cargo.toml, outside the workspace.
// Public API only; in-memory values only.
use rightkit_text_polish::text_pipeline::{
    deterministic_polish, deterministic_polish_with_brand, BrandHomophones, HomophoneGuard,
};

const BRAND: BrandHomophones<'static> = BrandHomophones {
    brand: "NoteWell",
    phrases: &[(
        "note well",
        HomophoneGuard::LiteralAfter(&["i", "you", "we"]),
    )],
    context_terms: &["app"],
};

fn check(ok: bool, what: &str, got: &str) -> bool {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}: got {got:?}");
    }
    ok
}

fn main() {
    let mut all = true;
    let out = deterministic_polish("we need twenty five copies");
    all &= check(
        out.starts_with('W') && out.contains("25"),
        "polish capitalizes and writes the number as digits",
        &out,
    );
    let out = deterministic_polish_with_brand("open the note well app", Some(&BRAND));
    all &= check(out.contains("NoteWell"), "brand phrase repaired", &out);
    let out = deterministic_polish_with_brand("you note well the risk", Some(&BRAND));
    all &= check(!out.contains("NoteWell"), "literal reading spared", &out);
    let out = deterministic_polish("open the note well app");
    all &= check(
        !out.contains("NoteWell"),
        "no brand repair without rules",
        &out,
    );
    if !all {
        std::process::exit(1);
    }
}
