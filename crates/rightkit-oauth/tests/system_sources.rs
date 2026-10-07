//! System sources must never prompt: an unknown host yields nothing, promptly, with every
//! interactive path (terminal, askpass, Git Credential Manager UI) disabled.

use std::time::{Duration, Instant};

use rightkit_oauth::system::SystemSource;

#[test]
fn unknown_hosts_yield_nothing_without_prompting() {
    for source in [
        SystemSource::GitCredential {
            host: "rightkit-signin-probe.invalid".into(),
        },
        SystemSource::GitHubCli {
            host: "rightkit-signin-probe.invalid".into(),
        },
    ] {
        let started = Instant::now();
        assert!(
            source.lookup(Duration::from_secs(10)).is_none(),
            "{source:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(12),
            "{source:?} must stay bounded"
        );
    }
}
