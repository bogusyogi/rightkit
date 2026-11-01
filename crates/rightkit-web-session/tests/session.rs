use rightkit_web_session::{IDLE_MS, MAX_SITES, REQUEST_TTL_MS, host_allowed, supported};

#[test]
fn approved_sites_include_their_subdomains_only() {
    let sites = vec!["example.com".to_string()];
    assert!(host_allowed("example.com", &sites));
    assert!(host_allowed("login.Example.com.", &sites));
    assert!(!host_allowed("notexample.com", &sites));
    assert!(!host_allowed("example.com.evil.test", &sites));
    assert!(!host_allowed("", &sites));
}

#[test]
fn the_default_build_has_no_browser_and_the_limits_are_fixed() {
    assert!(!supported(), "the browser feature is off in this build");
    assert_eq!(REQUEST_TTL_MS, 10 * 60 * 1000);
    assert_eq!(IDLE_MS, 15 * 60 * 1000);
    assert_eq!(MAX_SITES, 8);
}
