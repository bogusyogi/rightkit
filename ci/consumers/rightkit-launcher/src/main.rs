// rightkit-launcher 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
//
// Proves, through the public API only, the cross-platform calculator and response-text helpers:
// - calc::evaluate computes a sum that has an operator and gives "4" for 2+2;
// - calc::evaluate refuses a bare number with no operator;
// - calc::format writes a whole number without a fraction;
// - response_layout::excerpt keeps a short text and returns non-empty output for it.
// The Windows panel, chat, store and spot-takeover modules are not compiled on this host and are
// not used. No file, rate refresh, network or process spawn. Exits 1 on the first failed check.
use rightkit_launcher::calc::{evaluate, format};
use rightkit_launcher::response_layout::excerpt;

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
    check(
        !excerpt("short text", 40).is_empty(),
        "a short text yields a non-empty excerpt",
    );
    println!("OK rightkit-launcher 0.1.0 (registry consumer)");
}
