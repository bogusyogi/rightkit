// rightkit-github 0.1.0 consumed by exact registry pin: webhook replay guard, verifier secret rules,
// forge parsing and the unconfigured sign-in refusal. No network and no keychain access.
use rightkit_github::cli::parse_forge;
use rightkit_github::signin::{begin_device_sign_in, configure, SignInConfig};
use rightkit_github::webhook::{ReplayGuard, WebhookVerifier};
use rightkit_github::Forge;

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let mut guard = ReplayGuard::new(4, 300).expect("guard");
    check(guard.admit("d1", 100).is_ok(), "first delivery admitted");
    check(guard.admit("d1", 101).is_err(), "replayed delivery refused");
    check(
        WebhookVerifier::new(7, b"short".to_vec()).is_err(),
        "short webhook secret refused",
    );
    check(
        matches!(parse_forge(None), Ok(Forge::GitHub)),
        "default forge is GitHub",
    );
    // Keychain off and no client id: sign-in must refuse without touching the network or keychain.
    configure(SignInConfig {
        keychain_disable_env: "RK_GITHUB_CONSUMER_NO_KEYCHAIN".into(),
        ..SignInConfig::default()
    });
    std::env::set_var("RK_GITHUB_CONSUMER_NO_KEYCHAIN", "1");
    check(
        begin_device_sign_in().is_err(),
        "sign-in refuses when the keychain is disabled",
    );
}
