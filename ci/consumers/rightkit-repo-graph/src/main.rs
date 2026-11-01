// rightkit-repo-graph 0.1.0 consumed by exact registry pin: skeletonization passthrough and expansion. No I/O.
use rightkit_repo_graph::{expand_region, skeletonize};
use std::path::Path;

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let small = "fn main() {}\n";
    let r = skeletonize(Path::new("a.rs"), small, "");
    check(
        !r.was_skeletonized && r.output == small,
        "small files pass through unchanged",
    );
    check(
        expand_region(small, &r.regions, "missing").is_none(),
        "unknown region does not expand",
    );
}
