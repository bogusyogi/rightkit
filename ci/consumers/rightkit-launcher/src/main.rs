//! Registry consumer for rightkit-launcher 0.1.0: published API only, no OS access.
use rightkit_launcher::calc::{convert, evaluate};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        evaluate("2+3*4").as_deref() == Some("14"),
        "arithmetic is evaluated",
    );
    check(evaluate("42").is_none(), "a bare number is not a sum");
    check(
        convert("1 km to m").is_some() && convert("1 km to banana").is_none(),
        "known units convert and unknown ones are refused",
    );
}
