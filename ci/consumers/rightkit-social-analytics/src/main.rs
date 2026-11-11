//! Registry consumer for rightkit-social-analytics 0.2.0: builds against the published API only.
use rightkit_social_analytics::links::{final_url, is_link_id, link_id_of, referrer_host, Utm};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    let id = link_id_of("v", "c", "a", "camp", "var", "x", "https://example.com/");
    check(is_link_id(&id), "a derived link id has the lnk_ shape");
    let utm = Utm::resolve(None, "x", "spring", "v1").expect("defaults resolve");
    let url = final_url("https://example.com/p", &utm.params()).unwrap_or_default();
    check(
        url.contains("utm_medium=social"),
        "the default medium is appended",
    );
    check(
        referrer_host("https://Example.com:443/path?q").as_deref() == Some("example.com"),
        "a referrer keeps only its host",
    );
    println!("rightkit-social-analytics consumer: all checks passed");
}
