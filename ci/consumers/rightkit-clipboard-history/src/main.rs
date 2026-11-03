// rightkit-clipboard-history 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
//
// Proves, through the public API only, the platform-neutral pure helpers that compile on every host:
// - model::Kind round-trips its wire names through as_str and parse, and refuses an unknown name;
// - detect::detect classifies a plain sentence as Text, a web address as Link (host without www.,
//   lower-case scheme) and a #RRGGBB value as Colour;
// - detect::colour accepts a hex colour and refuses a non-hex one;
// - json::parse reads a small object within its byte cap and refuses input over the cap.
// The Store, the Windows capture and paste paths, and any persisted history are not touched.
// No I/O, no network, no clipboard access. Exits 1 on the first failed check.
use rightkit_clipboard_history::detect::{colour, detect, Detected};
use rightkit_clipboard_history::json;
use rightkit_clipboard_history::model::Kind;

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    check(Kind::parse("link") == Some(Kind::Link), "the link kind parses from its name");
    check(Kind::Image.as_str() == "image", "the image kind serialises as image");
    check(Kind::parse("video").is_none(), "an unknown kind name is refused");

    check(
        matches!(detect("just some words"), Detected::Text),
        "a plain sentence is Text",
    );
    check(
        matches!(
            detect("https://www.Example.com/path"),
            Detected::Link { ref host, ref scheme } if host == "example.com" && scheme == "https"
        ),
        "a web address is a Link with a bare lower-case host and its scheme",
    );
    check(
        matches!(detect("#40C8E0"), Detected::Colour { ref hex } if hex.starts_with('#')),
        "a hex colour is a Colour",
    );
    check(colour("#40C8E0").is_some(), "colour accepts a six-digit hex value");
    check(colour("#40C8EZ").is_none(), "colour refuses a non-hex digit");

    let doc = json::parse(br#"{"n": 2, "ok": true}"#, 1024);
    check(
        doc.as_ref().and_then(|v| v.path(&["n"])).and_then(|v| v.as_f64()) == Some(2.0),
        "json::parse reads a number member",
    );
    check(
        json::parse(br#"{"n": 2}"#, 3).is_none(),
        "json::parse refuses input over its byte cap",
    );
    println!("OK rightkit-clipboard-history 0.1.0 (registry consumer)");
}
