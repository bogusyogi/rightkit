//! Registry consumer for rightkit-web-session 0.1.0: builds against the published API only.
use rightkit_web_session::{host_allowed, supported, MAX_SITES};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    let sites = vec!["example.com".to_string()];
    check(
        host_allowed("login.example.com", &sites) && !host_allowed("notexample.com", &sites),
        "an approved site covers its subdomains and nothing that merely ends alike",
    );
    check(!supported(), "the default build has no browser");
    check(MAX_SITES == 8, "one request names at most eight sites");
    println!("rightkit-web-session consumer: all checks passed");
}
