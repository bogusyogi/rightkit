// rightkit-evidence 0.1.0 consumed by exact registry pin: spec store and placeholder detection. No I/O.
use rightkit_evidence::{is_placeholder_text, SpecStore};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let store = SpecStore::new();
    check(
        store.list_specs(None).is_empty(),
        "a new SpecStore is empty",
    );
    check(
        store.spec("missing").is_none(),
        "unknown spec id returns None",
    );
    check(
        is_placeholder_text("TODO") || is_placeholder_text("[NEEDS CLARIFICATION]"),
        "placeholder text detected",
    );
    let json = serde_json::to_string(&store).expect("serialize store");
    let back: SpecStore = serde_json::from_str(&json).expect("deserialize store");
    check(
        back.list_specs(None).is_empty(),
        "SpecStore serde round trip",
    );
}
