use super::*;

#[test]
fn parses_search_contacts_response() {
    let json = r#"{
            "results": [
                {
                    "person": {
                        "names": [{ "displayName": "Alice Example" }],
                        "emailAddresses": [
                            { "value": "other@example.com" },
                            { "value": "alice@example.com" }
                        ],
                        "photos": [{ "url": "https://lh3.googleusercontent.com/abc123" }],
                        "organizations": [{ "title": "Engineering Lead", "name": "Acme Corp" }]
                    }
                }
            ]
        }"#;

    let resp: SearchResp = serde_json::from_str(json).expect("deserialize failed");
    let card = first_match(&resp.results, "alice@example.com").expect("expected a ContactCard");

    assert_eq!(card.display_name, "Alice Example");
    assert_eq!(card.email, "alice@example.com"); // matched queried address
    assert_eq!(card.photo_url, "https://lh3.googleusercontent.com/abc123");
    assert_eq!(card.org_title, "Engineering Lead");
}

#[test]
fn falls_back_to_org_name_when_title_empty() {
    let json = r#"{
            "results": [
                {
                    "person": {
                        "names": [{ "displayName": "Bob Builder" }],
                        "emailAddresses": [{ "value": "bob@acme.com" }],
                        "photos": [],
                        "organizations": [{ "title": "", "name": "Acme Corp" }]
                    }
                }
            ]
        }"#;

    let resp: SearchResp = serde_json::from_str(json).expect("deserialize failed");
    let card = first_match(&resp.results, "bob@acme.com").expect("expected a ContactCard");

    assert_eq!(card.org_title, "Acme Corp");
    assert_eq!(card.photo_url, ""); // photos empty → empty string
}

#[test]
fn empty_results_returns_none() {
    let json = r#"{ "results": [] }"#;
    let resp: SearchResp = serde_json::from_str(json).expect("deserialize failed");
    assert!(first_match(&resp.results, "nobody@example.com").is_none());
}

#[test]
fn missing_optional_fields_default_to_empty() {
    // API omits empty fields entirely — serde(default) must handle this.
    let json = r#"{
            "results": [
                {
                    "person": {
                        "names": [{ "displayName": "Sparse User" }],
                        "emailAddresses": [{ "value": "sparse@example.com" }]
                    }
                }
            ]
        }"#;

    let resp: SearchResp = serde_json::from_str(json).expect("deserialize failed");
    let card = first_match(&resp.results, "sparse@example.com").expect("expected a ContactCard");

    assert_eq!(card.display_name, "Sparse User");
    assert_eq!(card.photo_url, "");
    assert_eq!(card.org_title, "");
}
