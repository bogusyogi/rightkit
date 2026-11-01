//! Registry consumer for rightkit-clipboard-history 0.1.0: published API only, no OS access.
use rightkit_clipboard_history::detect::{is_secret, link};
use rightkit_clipboard_history::{detect, Detected};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        matches!(detect("#ff8800"), Detected::Colour { .. }),
        "a hex colour is detected",
    );
    check(
        matches!(detect("https://example.com/a"), Detected::Link { .. }),
        "a web address is detected as a link",
    );
    check(
        link("plain words").is_none() && detect("plain words") == Detected::Text,
        "plain words stay text",
    );
    check(!is_secret("   "), "blank text is not a secret");
}
