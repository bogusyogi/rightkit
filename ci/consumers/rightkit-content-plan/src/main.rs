//! Registry consumer for rightkit-content-plan 0.1.3: builds against the published API only.
use rightkit_content_plan::drafts::{approval_gate, disclosed, host_allowed};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    let phrases = vec!["#ad".to_string()];
    check(
        disclosed("#ad Great kit. Link below", &phrases),
        "a disclosure in the first sentence counts",
    );
    check(
        approval_gate("d", "post", "Great kit. #ad", None, true, &phrases, None).is_err(),
        "a late disclosure blocks approval",
    );
    check(
        host_allowed("x", "x.com").is_ok(),
        "x.com is an allowed X host",
    );
    println!("rightkit-content-plan consumer: all checks passed");
}
