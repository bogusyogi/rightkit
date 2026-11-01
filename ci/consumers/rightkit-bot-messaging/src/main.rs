// rightkit-bot-messaging 0.1.0 consumed by exact registry pin: user allow-list and web token auth. No network.
use rightkit_bot_messaging::{AuthGate, WebAuth};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let gate = AuthGate::new(vec![42]);
    check(gate.check(42).is_allowed(), "allow-listed user accepted");
    check(!gate.check(7).is_allowed(), "unknown user refused");
    let web = WebAuth::new("secret-token");
    check(
        web.check(None, Some("secret-token")).is_authorized(),
        "matching query token authorized",
    );
    check(
        !web.check(None, Some("wrong")).is_authorized(),
        "wrong token refused",
    );
}
