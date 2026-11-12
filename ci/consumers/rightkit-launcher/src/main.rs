// rightkit-launcher 0.2.0 consumed by exact registry pin (`=0.2.0`), outside the workspace.
//
// Proves, through the public API only, the portable launcher core:
// - calc::evaluate computes a sum that has an operator and gives "4" for 2+2;
// - calc::evaluate refuses a bare number with no operator;
// - calc::format writes a whole number without a fraction;
// - json::parse reads a small object within its byte cap and refuses input over the cap (the one
//   bounded JSON reader, which rightkit-clipboard-history re-exports);
// - host::tf fills a template's placeholders in order with no host installed.
// The Windows panel lives in rightkit-launcher-win since 0.2.0; the answer layout in
// rightkit-overlay-win. No file, rate refresh, network or process spawn. Exits 1 on the first
// failed check.
use rightkit_launcher::calc::{evaluate, format};
use rightkit_launcher::host::tf;
use rightkit_launcher::json;

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    check(
        evaluate("2+2").as_deref() == Some("4"),
        "2+2 evaluates to 4",
    );
    check(
        evaluate("7").is_none(),
        "a bare number with no operator is not a calculation",
    );
    check(
        format(4.0) == "4",
        "a whole number is written without a fraction",
    );
    let doc = json::parse(br#"{"n": 2, "ok": true}"#, 1024);
    check(
        doc.as_ref()
            .and_then(|d| d.get("n"))
            .and_then(json::Value::as_f64)
            == Some(2.0),
        "json::parse reads a number member",
    );
    check(
        json::parse(br#"{"n": 2}"#, 3).is_none(),
        "json::parse refuses input over its byte cap",
    );
    check(
        tf(
            "{} opens first; {} no longer gets the key",
            &["App", "Other"],
        ) == "App opens first; Other no longer gets the key",
        "tf fills placeholders in order without a host",
    );
    println!("OK rightkit-launcher 0.2.0 (registry consumer)");
}
