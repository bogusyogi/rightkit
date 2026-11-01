//! Registry consumer for rightkit-bench 0.1.0: builds against the published API only.
use rightkit_bench::article::{readable_text, score_article, summarize};
use rightkit_bench::{measure_many, percentile_ms};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

const PAGE: &str =
    "<html><body><nav>Menu Links</nav><article><p>The article body has plenty of words. \
It keeps going for a while so it counts as readable text. Done!</p></article></body></html>";

fn main() {
    check(
        percentile_ms(&mut [3, 1, 2], 0.5) == 2,
        "the median of 1,2,3 is 2",
    );
    let m = measure_many("noop", 2, || Ok(1));
    check(
        m.map(|m| m.n == 2).unwrap_or(false),
        "measure_many times n runs",
    );
    let text = readable_text(PAGE).unwrap_or_default();
    check(
        text.contains("article body") && !text.contains("Menu"),
        "the reference extractor drops navigation",
    );
    let score = score_article("page", PAGE, &text);
    check(
        summarize(&[score]).map(|s| s.cases == 1).unwrap_or(false),
        "a scored page summarizes",
    );
    println!("rightkit-bench consumer: all checks passed");
}
