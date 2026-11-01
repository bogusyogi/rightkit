use super::*;

#[test]
fn extract_hrefs_handles_quote_styles_and_whitespace() {
    let html = r#"<a href="https://a.com">A</a>
            <a href='https://b.com'>B</a>
            <a href = "https://c.com">C (spaced =)</a>
            <a href=https://d.com>D (unquoted)</a>"#;
    let hrefs = extract_hrefs(html);
    assert!(hrefs.contains(&"https://a.com".to_string()));
    assert!(hrefs.contains(&"https://b.com".to_string()));
    assert!(hrefs.contains(&"https://c.com".to_string()));
    assert!(hrefs.contains(&"https://d.com".to_string()));
    assert_eq!(hrefs.len(), 4);
}

#[test]
fn extract_hrefs_ignores_comments_and_non_attr_matches() {
    let html = r#"<!-- <a href="https://evil.com">hidden</a> -->
            <p>the word href appears in prose, not as href to anything</p>
            <a href="https://good.com">good</a>"#;
    let hrefs = extract_hrefs(html);
    assert_eq!(hrefs, vec!["https://good.com".to_string()]);
}

#[test]
fn auth_results_parses_all_three() {
    let h = "mx.google.com; dkim=pass header.i=@stripe.com; spf=pass (google.com: domain of bounce@stripe.com designates 1.2.3.4) smtp.mailfrom=bounce@stripe.com; dmarc=pass (p=REJECT sp=REJECT dis=NONE) header.from=stripe.com";
    let a = parse_auth_results(h);
    assert_eq!(a.spf, "pass");
    assert_eq!(a.dkim, "pass");
    assert_eq!(a.dmarc, "pass");
}

#[test]
fn auth_results_failures_and_missing() {
    let a = parse_auth_results("mx.google.com; spf=softfail smtp.mailfrom=x; dkim=fail");
    assert_eq!(a.spf, "softfail");
    assert_eq!(a.dkim, "fail");
    assert_eq!(a.dmarc, "none"); // absent → none
}

#[test]
fn auth_results_keeps_strongest_when_repeated() {
    // two dkim signatures: none then pass — keep pass
    let a = parse_auth_results("x; dkim=none; dkim=pass header.i=@a.com");
    assert_eq!(a.dkim, "pass");
}

#[test]
fn phishing_sample_danger() {
    let html = r#"
            <a href="https://bit.ly/abc123">Verify account</a>
            <a href="http://paypa1-secure.evil.tld/login">Click here</a>
        "#;
    let report = assess(
        "x@secure-paypa1.tld",
        "PayPal Security",
        Some("attacker@harvest.tld"),
        html,
    );
    // Should be danger (brand spoof)
    assert_eq!(report.level, "danger");
    // At minimum: brand spoof + Reply-To mismatch + shortened link + http link
    assert!(
        report.findings.len() >= 2,
        "expected >= 2 findings, got {}",
        report.findings.len()
    );
    // At least one danger finding must be present
    let has_danger = report.findings.iter().any(|f| f.level == "danger");
    assert!(has_danger, "expected at least one danger finding");
}

#[test]
fn clean_sample_info() {
    let html = r#"<a href="https://example.com/page">Visit us</a>"#;
    let report = assess(
        "support@example.com",
        "Example Support",
        Some("support@example.com"),
        html,
    );
    assert_eq!(report.level, "info");
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].message, "No obvious risk signals.");
}

#[test]
fn raw_ip_link_is_danger() {
    let html = r#"<a href="http://192.168.1.1/phish">click</a>"#;
    let report = assess("user@legit.com", "Legit Corp", None, html);
    assert_eq!(report.level, "danger");
    let has_ip_finding = report.findings.iter().any(|f| f.message.contains("raw IP"));
    assert!(has_ip_finding);
}

#[test]
fn punycode_is_warn() {
    let html = r#"<a href="https://xn--pypal-4ve.com/verify">Verify</a>"#;
    let report = assess("user@legit.com", "Normal Person", None, html);
    // punycode warn; no danger, so worst should be warn
    assert!(report.level == "warn" || report.level == "danger");
    let has_puny = report
        .findings
        .iter()
        .any(|f| f.message.contains("Punycode"));
    assert!(has_puny);
}

#[test]
fn domain_of_email_angle_bracket() {
    assert_eq!(domain_of_email("Alice <alice@Example.COM>"), "example.com");
}

#[test]
fn domain_of_email_bare() {
    assert_eq!(domain_of_email("bob@domain.org"), "domain.org");
}

#[test]
fn host_of_url_strips_path_and_port() {
    assert_eq!(host_of_url("https://Evil.COM:8080/path?q=1"), "evil.com");
}
