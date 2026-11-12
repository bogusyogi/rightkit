//! Desktop and CLI Github sign-in is `rightkit-github`'s (`signin`); this module installs the
//! host app's client id and credential namespace ([`crate::HostIdentity`]) before the first call.
//! Without a client id the crate reports that sign-in is not configured.

use std::sync::Once;

pub(crate) use rightkit_github::signin::DeviceWait;

fn configured() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let host = crate::host_identity::host();
        rightkit_github::signin::configure(rightkit_github::signin::SignInConfig {
            client_id: host.github_client_id().unwrap_or_default().to_string(),
            credential_namespace: host.credential_namespace().to_string(),
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
