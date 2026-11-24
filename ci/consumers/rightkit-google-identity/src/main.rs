//! Registry consumer for rightkit-google-identity 0.3.0: builds against the published API only.
use rightkit_google_identity::{Failure, DRIVE_APPDATA_SCOPE, GOOGLE_ISSUERS};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        DRIVE_APPDATA_SCOPE.ends_with("/auth/drive.appdata"),
        "the Drive appdata scope is the published one",
    );
    check(
        GOOGLE_ISSUERS.contains(&"https://accounts.google.com"),
        "Google's issuer is accepted",
    );
    let failure = Failure::new("sign_in_again", "sign in again");
    check(
        failure.code == "sign_in_again" && failure.message == "sign in again",
        "a failure carries a fixed code and text",
    );
    println!("rightkit-google-identity consumer: all checks passed");
}
