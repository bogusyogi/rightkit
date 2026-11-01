//! Registry consumer for rightkit-google-apis 0.1.0: builds against the published API only.
//! It never contacts Google.
use rightkit_google_apis::gmail::{decode_b64url, encode_raw};
use rightkit_google_apis::oauth::SCOPE;

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        SCOPE.split(' ').any(|s| s.ends_with("/gmail.modify")),
        "the scope set includes gmail.modify",
    );
    check(
        SCOPE.split(' ').any(|s| s == "openid"),
        "the scope set includes openid",
    );
    let mime = "Subject: hi\r\n\r\nbody?>";
    check(
        decode_b64url(&encode_raw(mime)) == mime.as_bytes(),
        "a raw message round-trips through base64url",
    );
    println!("rightkit-google-apis consumer: all checks passed");
}
