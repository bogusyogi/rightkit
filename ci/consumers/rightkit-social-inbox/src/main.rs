//! Registry consumer for rightkit-social-inbox 0.1.0: builds against the published API only.
use rightkit_social_inbox::keywords::keyword_matches;
use rightkit_social_inbox::webhook::{
    parse_delivery, signature_header_for, verify_signature, Parsed,
};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    let body = br#"{"object":"instagram","entry":[{"id":"a","changes":[{"field":"comments","value":{"id":"c","text":"price?"}}]}]}"#;
    let header = signature_header_for(b"secret", body);
    check(
        verify_signature(b"secret", body, Some(&header)),
        "a genuine signature verifies",
    );
    check(
        !verify_signature(b"other", body, Some(&header)),
        "a wrong secret is refused",
    );
    let events = match parse_delivery(body) {
        Parsed::Events { events, .. } => events,
        Parsed::Unparseable(_) => Vec::new(),
    };
    check(
        events.len() == 1 && events[0].kind == "comment",
        "the delivery parses into one comment",
    );
    check(
        keyword_matches("price", "price?"),
        "the keyword matches as a whole word",
    );
    println!("rightkit-social-inbox consumer: all checks passed");
}
