use rightkit_launcher::calc::{convert, convert_with, evaluate};

#[test]
fn evaluates_arithmetic_but_not_a_bare_number() {
    assert_eq!(evaluate("2+3*4").as_deref(), Some("14"));
    assert_eq!(evaluate("42"), None);
    assert_eq!(evaluate("hello"), None);
}

#[test]
fn converts_units_and_lets_the_host_word_the_detail() {
    let c = convert("1 km to m").expect("conversion");
    assert!(c.copy.starts_with("1000"));
    let d = convert_with("1 km to m", |a, f, t| format!("{t}<-{f}:{a}")).expect("conversion");
    assert!(d.detail.starts_with("m<-km:"));
    assert!(convert("1 km to banana").is_none());
}
