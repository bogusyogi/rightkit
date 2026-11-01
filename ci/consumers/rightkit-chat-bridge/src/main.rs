//! Registry consumer for rightkit-chat-bridge 0.1.0: builds against the published API only.
use rightkit_chat_bridge::chat::brand::{brand, branded};
use rightkit_chat_bridge::chat::envelope::plain_label;
use rightkit_chat_bridge::chat::links::Link;

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        brand().product == "RightKit" && brand().cli == "rightkit",
        "the default display names are product-neutral",
    );
    check(
        brand().envelope_domain == "pulse-chat/envelope/v1",
        "the default wire identifier is the one existing installs use",
    );
    check(
        branded("`@cli@ chat peers`") == "`rightkit chat peers`",
        "branded text fills the command name",
    );
    let link: Link = serde_json::from_value(serde_json::json!({"device": "d", "ssh": "h"}))
        .expect("a link with defaults");
    check(
        link.cli == "rightkit" && link.shell == "posix",
        "a link defaults to the brand's command on PATH",
    );
    check(
        plain_label("a \"b\" <c>", 20) == "a b c",
        "labels carry no quotes or markup",
    );
    println!("rightkit-chat-bridge consumer: all checks passed");
}
