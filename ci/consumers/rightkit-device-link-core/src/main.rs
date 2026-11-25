//! Registry consumer for rightkit-device-link-core 0.1.0: builds against the published API only.
use rightkit_device_link_core::identity::device_id_for_key;
use rightkit_device_link_core::join_token::{JoinToken, JOIN_PREFIX};
use rightkit_device_link_core::{format_code, PAIRING_CODE_DIGITS};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        PAIRING_CODE_DIGITS == 8,
        "the pairing code length is the published one",
    );
    check(
        format_code("12345678") == "1234 5678",
        "a pairing code is shown in two groups",
    );
    check(
        format_code("12") == "12",
        "a code of the wrong length is shown as is",
    );
    check(
        device_id_for_key(&[7u8; 32]).starts_with("dev_")
            && device_id_for_key(&[7u8; 32]) == device_id_for_key(&[7u8; 32]),
        "a device id is derived from its key",
    );
    check(
        JOIN_PREFIX.starts_with("pulse-join:"),
        "the join token prefix is stable",
    );
    check(
        JoinToken::parse("not a token").is_err(),
        "a malformed join token is refused",
    );
    println!("rightkit-device-link-core consumer: all checks passed");
}
