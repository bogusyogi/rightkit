//! Registry consumer for rightkit-content-proof 0.1.0: builds against the published API only.
use rightkit_content_proof::numbers::{canonical, tokens};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        canonical("1,240") == canonical("1240.00"),
        "1,240 equals 1240.00 exactly",
    );
    check(
        canonical("1.24k").as_deref() == Some("1240"),
        "1.24k reads as 1240",
    );
    let found: Vec<String> = tokens("Up 12% to $3m in nine days")
        .into_iter()
        .map(|t| t.text)
        .collect();
    check(
        found == ["12%", "$3m", "nine"],
        "every number token is found",
    );
    println!("rightkit-content-proof consumer: all checks passed");
}
