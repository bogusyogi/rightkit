//! Desktop and CLI Github sign-in is `rightkit-github`'s (`signin`); this host installs its
//! client id and credential namespace before the first call.

use std::sync::Once;

pub(crate) use rightkit_github::signin::DeviceWait;

/// Public Github App client id shared with the iOS configuration.
const CLIENT_ID: &str = "Iv23liUIkiMqvrf6e3hi";

fn configured() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        rightkit_github::signin::configure(rightkit_github::signin::SignInConfig {
            client_id: CLIENT_ID.to_string(),
            credential_namespace: "coderight".to_string(),
            keychain_disable_env: "RIGHTKIT_DISABLE_OS_KEYCHAIN".to_string(),
        });
    });
}

pub(crate) fn resolve_access_token() -> Result<Option<String>, String> {
    configured();
    rightkit_github::signin::resolve_access_token()
}

pub(crate) fn sign_out() -> Result<(), String> {
    configured();
    rightkit_github::signin::sign_out()
}

pub(crate) fn device_sign_in_with_terminal() -> Result<(), String> {
    configured();
    rightkit_github::signin::device_sign_in_with_terminal()
}

pub(crate) fn begin_device_sign_in() -> Result<rightkit_github::signin::DeviceSignIn, String> {
    configured();
    rightkit_github::signin::begin_device_sign_in()
}

pub(crate) fn open_verification_url(url: &str) -> Result<(), String> {
    rightkit_github::signin::open_verification_url(url)
}
