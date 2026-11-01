use super::*;

/// Thin wrapper so the tests read like fixtures of search_recipients' flatten
/// step, but the actual logic is the shared `collect_recipients` helper in
/// `recipients.rs`. One source of truth, exercised by both the impl path and
/// the test fixtures.
fn flatten_for_test(sources: &[&[SearchResult]], limit: usize) -> Vec<Recipient> {
    collect_recipients(sources, limit)
}

fn parse_resp(json: &str) -> SearchResp {
    serde_json::from_str(json).expect("parse failed")
}

#[test]
fn dedup_by_lowercased_email_contacts_wins_over_directory() {
    // contacts has Alice with canonical casing; directory has same address
    // with different case + different name — contacts entry should win.
    let contacts_json = r#"{
            "results": [{
                "person": {
                    "names": [{ "displayName": "Alice (Contacts)" }],
                    "emailAddresses": [{ "value": "Alice@example.com" }],
                    "photos": [{ "url": "https://lh3/contacts" }]
                }
            }]
        }"#;
    let directory_json = r#"{
            "results": [{
                "person": {
                    "names": [{ "displayName": "Alice (Directory)" }],
                    "emailAddresses": [{ "value": "alice@example.com" }],
                    "photos": [{ "url": "https://lh3/directory" }]
                }
            }]
        }"#;

    let cr = parse_resp(contacts_json);
    let dr = parse_resp(directory_json);

    let out = flatten_for_test(&[&cr.results, &dr.results, &[]], 12);

    assert_eq!(out.len(), 1, "duplicate email should collapse to one");
    assert_eq!(
        out[0].name, "Alice (Contacts)",
        "contacts source should win"
    );
    assert_eq!(out[0].photo_url, "https://lh3/contacts");
}

#[test]
fn one_person_with_two_addresses_emits_two_recipients() {
    let json = r#"{
            "results": [{
                "person": {
                    "names": [{ "displayName": "Bob Builder" }],
                    "emailAddresses": [
                        { "value": "bob@work.com" },
                        { "value": "bob@personal.com" }
                    ],
                    "photos": []
                }
            }]
        }"#;
    let resp = parse_resp(json);
    let out = flatten_for_test(&[&resp.results, &[], &[]], 12);

    assert_eq!(out.len(), 2);
    assert_eq!(out[0].email, "bob@work.com");
    assert_eq!(out[1].email, "bob@personal.com");
    // Both share the same name/photo from the parent Person.
    assert_eq!(out[0].name, "Bob Builder");
    assert_eq!(out[1].name, "Bob Builder");
}

#[test]
fn limit_caps_at_specified_count() {
    // 10 persons × 2 addresses each = 20 candidates; limit = 12.
    let entries: String = (0..10)
        .map(|i| {
            format!(
                r#"{{"person":{{"names":[{{"displayName":"Person {i}"}}],
                         "emailAddresses":[{{"value":"p{i}a@x.com"}},{{"value":"p{i}b@x.com"}}],
                         "photos":[]}}}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let json = format!(r#"{{"results":[{entries}]}}"#);
    let resp = parse_resp(&json);
    let out = flatten_for_test(&[&resp.results, &[], &[]], 12);

    assert_eq!(out.len(), 12, "should be capped at 12");
}

#[test]
fn empty_email_value_skipped() {
    let json = r#"{
            "results": [{
                "person": {
                    "names": [{ "displayName": "Ghost" }],
                    "emailAddresses": [{ "value": "" }, { "value": "ghost@x.com" }],
                    "photos": []
                }
            }]
        }"#;
    let resp = parse_resp(json);
    let out = flatten_for_test(&[&resp.results, &[], &[]], 12);

    assert_eq!(out.len(), 1);
    assert_eq!(out[0].email, "ghost@x.com");
}

#[test]
fn all_sources_empty_returns_empty_vec() {
    let out = flatten_for_test(&[&[], &[], &[]], 12);
    assert!(out.is_empty());
}
