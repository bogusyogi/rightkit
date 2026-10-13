#![cfg(target_os = "macos")]

use rightkit_sandbox::seatbelt::{fixture_profile_for, BrowserFixturePolicy};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

struct FixtureTree(PathBuf);

impl FixtureTree {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "rightkit-browser-fixture-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create fixture test tree");
        Self(root)
    }

    fn chrome(&self) -> PathBuf {
        let executable = self
            .0
            .join("Google Chrome.app/Contents/MacOS/Google Chrome");
        std::fs::create_dir_all(executable.parent().expect("executable parent"))
            .expect("create app bundle");
        std::fs::write(&executable, b"test executable").expect("create executable fixture");
        executable
    }

    fn profile(&self) -> PathBuf {
        let profile = self.0.join("disposable-profile");
        std::fs::create_dir(&profile).expect("create profile fixture");
        profile
    }

    fn policy(&self, chrome_executable: PathBuf, profile_dir: PathBuf) -> BrowserFixturePolicy {
        BrowserFixturePolicy {
            chrome_executable,
            profile_dir,
            proxy_addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 38081)),
        }
    }
}

impl Drop for FixtureTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn fixture_profile_confines_paths_network_and_writes() {
    let tree = FixtureTree::new();
    let chrome = tree.chrome();
    let profile = tree.profile();
    let sbpl = fixture_profile_for(&tree.policy(chrome, profile.clone())).expect("profile");

    let canonical_profile = std::fs::canonicalize(profile).expect("canonical profile");
    assert!(sbpl.contains("(deny default)"));
    assert!(!sbpl.contains("(import"));
    assert!(sbpl.contains("(allow process-exec* (subpath "));
    assert!(sbpl.contains("Google Chrome.app"));
    assert!(sbpl.contains(&format!(
        "(allow file-write* (subpath \"{}\"))",
        canonical_profile.display()
    )));
    assert!(sbpl.contains("(deny network-outbound)"));
    assert!(sbpl.contains("localhost:38081"));
    assert!(
        sbpl.contains("(require-all (remote ip \"localhost:38081\") (socket-type SOCK_STREAM))")
    );
    assert!(sbpl.contains("(deny network-outbound (socket-domain AF_INET6))"));
    assert!(sbpl.contains("(deny network-inbound)"));
    assert!(sbpl.contains(
        "(allow network-bind (require-all (local ip \"localhost:*\") (socket-type SOCK_STREAM)))"
    ));
    assert!(sbpl.contains("(allow network-inbound (local ip \"localhost:*\"))"));
    assert!(!sbpl.contains("(allow network*)"));
    assert!(!sbpl.contains("remote unix-socket"));
    assert!(!sbpl.contains("SOCK_DGRAM"));
    assert!(!sbpl.contains("(allow process-exec* (subpath \"/usr\"))"));
    // Writes: the profile directory, plus (when the per-user temp dir resolves) Chrome's own
    // ProcessSingleton directory under it.
    assert!(sbpl.matches("file-write*").count() <= 2);
    // The singleton grant is the mkdtemp shape plus SingletonSocket, not the whole Chrome tree.
    if sbpl.contains("SingletonSocket") {
        assert!(!sbpl.contains("[^/]*"), "{sbpl}");
        assert!(sbpl.contains("(/SingletonSocket)?$"), "{sbpl}");
    }
    assert!(!sbpl.contains("(allow mach-lookup)"));
    assert!(!sbpl.contains("(allow iokit*)"));
    assert!(!sbpl.contains("(allow network-bind)"));
    assert!(!sbpl.contains("ipc-posix"));
}

#[test]
fn fixture_profile_rejects_invalid_paths_and_proxy_addresses() {
    let tree = FixtureTree::new();
    let chrome = tree.chrome();
    let profile = tree.profile();

    let mut policy = tree.policy(chrome.clone(), profile.clone());
    policy.chrome_executable = Path::new("relative/chrome").to_path_buf();
    assert!(fixture_profile_for(&policy).is_err());

    let mut policy = tree.policy(chrome.clone(), profile.clone());
    policy.profile_dir = tree.0.clone();
    assert!(fixture_profile_for(&policy).is_err());

    let mut policy = tree.policy(chrome.clone(), profile.clone());
    policy.proxy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0));
    assert!(fixture_profile_for(&policy).is_err());

    let mut policy = tree.policy(chrome, profile);
    policy.proxy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 2), 38081));
    assert!(fixture_profile_for(&policy).is_err());
}

#[test]
fn fixture_profile_escapes_path_delimiters() {
    let tree = FixtureTree::new();
    let chrome = tree.chrome();
    let profile = tree.0.join("profile-\\-\"quoted\"");
    std::fs::create_dir(&profile).expect("create quoted profile path");
    let sbpl = fixture_profile_for(&tree.policy(chrome, profile)).expect("profile");

    assert!(sbpl.contains("profile-\\\\-\\\"quoted\\\""));
}
