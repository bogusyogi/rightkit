use rightkit_http::{Client, ClientConfig, ProxyMode, ProxyModeUsed};
use std::time::Duration;

#[test]
fn single_deadline_clears_phase_limits_and_sets_one_deadline() {
    let t = Duration::from_millis(1234);
    let config = ClientConfig::legacy_ureq_parity("t/1").single_deadline(t);
    assert_eq!(config.connect_timeout, None);
    assert_eq!(config.response_timeout, None);
    assert_eq!(config.recv_body_timeout, None);
    assert_eq!(config.request_timeout, t);
    assert_eq!(config.stream_timeout, Some(t));
    // Everything else of the parity config is kept.
    assert_eq!(config.max_redirects, 10);
    assert_eq!(config.user_agent, "t/1");
}

#[test]
fn try_new_or_direct_falls_back_when_the_proxy_is_unusable() {
    let config = ClientConfig::default().proxy(ProxyMode::Explicit("socks5://127.0.0.1:1".into()));
    assert!(Client::try_new(config.clone()).is_err());
    let client = Client::try_new_or_direct(config).expect("direct fallback");
    assert_eq!(client.proxy_mode_used(), &ProxyModeUsed::Direct);
}

#[test]
fn try_new_or_direct_keeps_a_usable_proxy() {
    let config = ClientConfig::default().proxy(ProxyMode::Explicit("http://127.0.0.1:3128".into()));
    let client = Client::try_new_or_direct(config).expect("client");
    assert_eq!(client.proxy_mode_used(), &ProxyModeUsed::Explicit);
}
