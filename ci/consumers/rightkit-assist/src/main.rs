//! Registry consumer for rightkit-assist 0.1.1: builds against the published API only.
use rightkit_assist::{digest, text, NAME};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(NAME == "rightkit-assist", "the package name reads back");
    check(
        text::clean("a \n\t b", 10) == "a b",
        "outside text collapses to one clean line",
    );
    check(
        text::escape_angle("<&>") == "&lt;&amp;&gt;",
        "angle brackets cannot close a wrapper tag",
    );
    check(
        text::truncate_bytes("abcdef", 3) == "abc",
        "truncation keeps a prefix",
    );
    check(
        digest::digest(&["a", "bc"]) != digest::digest(&["ab", "c"])
            && digest::short(&["a"]).len() == 16,
        "digests length-prefix their parts",
    );
    println!("rightkit-assist consumer: all checks passed");
}
