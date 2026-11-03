use rightkit_launcher::host::tf;
use rightkit_launcher::json::{Value, parse};

#[test]
fn json_keeps_member_order_and_rejects_garbage() {
    let value = parse(br#"{"b": 1, "a": [true, "x"]}"#, 1024).expect("parses");
    let Value::Object(members) = &value else {
        panic!("not an object");
    };
    assert_eq!(members[0].0, "b");
    assert_eq!(members[1].0, "a");
    assert!(parse(b"{not json", 1024).is_none());
}

#[test]
fn template_without_host_fills_in_order() {
    assert_eq!(tf("{} of {}", &["1", "2"]), "1 of 2");
    assert_eq!(tf("{} and {}", &["1"]), "1 and {}");
}
