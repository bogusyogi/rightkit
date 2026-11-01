//! Registry consumer for rightkit-repo-graph-grammars 0.1.0: builds against the published API only.
use rightkit_repo_graph_grammars::{ql, systemrdl, vue};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    let langs = [ql::LANGUAGE, systemrdl::LANGUAGE, vue::LANGUAGE];
    check(
        langs.len() == 3,
        "ql, systemrdl and vue language functions link",
    );
    println!("rightkit-repo-graph-grammars consumer: all checks passed");
}
