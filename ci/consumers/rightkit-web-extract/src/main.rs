//! Registry consumer for rightkit-web-extract 0.1.1: builds against the published API only.
use rightkit_web_extract::filters::apply_article_filters;
use rightkit_web_extract::{is_hard_article_error, readable_text, static_text_needs_browser};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    let body = "A complete sentence about the subject of this article. ".repeat(8);
    let html = format!("<html><body><nav>Menu</nav><main><p>{body}</p></main></body></html>");
    let text = readable_text(&html).unwrap_or_default();
    check(
        text.contains("complete sentence") && !text.contains("Menu"),
        "readable text drops navigation",
    );
    let (pruned, _fit) = apply_article_filters(&text, false);
    check(pruned == text, "disabled filters return the text unchanged");
    check(
        is_hard_article_error("http article fetch returned status 404"),
        "a 404 is final",
    );
    check(
        static_text_needs_browser("Loading"),
        "a JS shell escalates to the browser",
    );
    check(
        rightkit_web_extract::fetch::rendered_text_ready("\"4096\""),
        "a rendered text length over the minimum is ready",
    );
    let profile = rightkit_web_extract::fetch::platform_profile_dir(
        "threads",
        None,
        None,
        &std::env::temp_dir(),
    );
    check(
        profile.ends_with("threads"),
        "the default profile directory is per platform",
    );
    println!("rightkit-web-extract consumer: all checks passed");
}
