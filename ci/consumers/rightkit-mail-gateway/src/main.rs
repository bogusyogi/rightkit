//! Registry consumer for rightkit-mail-gateway 0.1.0: builds against the published API only.
use rightkit_mail_gateway::content::untrusted;
use rightkit_mail_gateway::outcome::{to_json, AgentError, Profile};
use rightkit_mail_gateway::redact::find_secret;
use serde_json::json;

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        Profile::parse("send_with_approval") == Some(Profile::SendWithApproval)
            && Profile::ReadOnly < Profile::Draft,
        "profiles parse and order by privilege",
    );
    check(
        untrusted("hello")["untrusted"] == json!(true),
        "sender text is wrapped as untrusted",
    );
    check(
        to_json(&Err(AgentError::Unauthorised))["error"]["code"] == json!("unauthorised"),
        "an error serialises with a stable code",
    );
    let planted = format!("{}{}", "Bear", "er abc");
    check(
        find_secret(&json!({ "a": planted }), &[]).is_some(),
        "the redaction check finds a planted token",
    );
}
