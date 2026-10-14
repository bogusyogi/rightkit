//! Native Seatbelt proof: disposable `.app` helper runs exact browser-fixture profile; parent
//! controls prove listeners before it runs, while accepted connections are independent evidence.

#![cfg(target_os = "macos")]

use rightkit_child_process::OwnedCommand;
use rightkit_sandbox::seatbelt::{BrowserFixturePolicy, Prepared};
use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const HELPER_ROLE: &str = "RIGHTKIT_MACOS_FIXTURE_NETWORK_ROLE";
const HELPER_PORT: &str = "RIGHTKIT_MACOS_FIXTURE_NETWORK_PORT";
const HELPER_TIMEOUT: Duration = Duration::from_secs(10);
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(2);
const NO_HIT_SETTLE: Duration = Duration::from_millis(300);
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "rightkit-macos-fixture-network-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create fixture-network scratch directory");
        Self(fs::canonicalize(path).expect("canonicalize fixture-network scratch directory"))
    }

    fn helper(&self) -> PathBuf {
        let source = std::env::current_exe().expect("current integration-test executable");
        let helper = self
            .0
            .join("Synthetic.app/Contents/MacOS/fixture-network-helper");
        fs::create_dir_all(helper.parent().expect("helper parent"))
            .expect("create synthetic app bundle");
        fs::copy(&source, &helper).expect("copy integration-test helper");
        let mode = fs::metadata(&source)
            .expect("read source helper permissions")
            .permissions()
            .mode();
        fs::set_permissions(&helper, fs::Permissions::from_mode(mode))
            .expect("preserve helper execute permission");
        fs::canonicalize(helper).expect("canonicalize synthetic app helper")
    }

    fn profile(&self) -> PathBuf {
        let profile = self.0.join("profile");
        fs::create_dir(&profile).expect("create disposable fixture profile");
        fs::canonicalize(profile).expect("canonicalize disposable fixture profile")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn ci_required() -> bool {
    std::env::var("GITHUB_ACTIONS").is_ok_and(|value| value == "true")
}

fn wait_for_accept(listener: &TcpListener, label: &str) {
    let deadline = Instant::now() + ACCEPT_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((_stream, _peer)) => return,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("{label} listener accept failed: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "{label} listener received no connection"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn assert_no_accept(listener: &TcpListener, label: &str) {
    let deadline = Instant::now() + NO_HIT_SETTLE;
    loop {
        match listener.accept() {
            Ok((_stream, peer)) => panic!("{label} listener was reached by {peer}"),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("{label} listener accept failed: {error}"),
        }
        if Instant::now() >= deadline {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn parent_positive_control(listener: &TcpListener, address: SocketAddr, label: &str) {
    TcpStream::connect_timeout(&address, ACCEPT_TIMEOUT)
        .unwrap_or_else(|error| panic!("parent {label} control connect failed: {error}"));
    wait_for_accept(listener, label);
}

fn run_helper(policy: BrowserFixturePolicy, profile: &Path, role: &str, port: u16) -> ExitStatus {
    let prepared = Prepared::for_browser_fixture(&policy).expect("prepare exact fixture profile");
    let mut command = OwnedCommand::new(&policy.chrome_executable);
    command.env_allowlist(std::iter::empty::<&str>());
    command
        .command_mut()
        .args([
            "--exact",
            "fixture_network_helper",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HELPER_ROLE, role)
        .env(HELPER_PORT, port.to_string())
        .env("HOME", profile)
        .env("TMPDIR", profile)
        .env("TMP", profile)
        .env("TEMP", profile)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.inherit_handles_only(std::iter::empty());
    // SAFETY: parent prepares profile/CI descriptor; child work is bounded native code.
    unsafe {
        command.command_mut().pre_exec(move || prepared.apply());
    }
    let mut child = command.spawn().expect("spawn confined fixture helper");
    match child
        .wait_timeout(HELPER_TIMEOUT)
        .expect("wait for fixture helper")
    {
        Some(status) => status,
        None => {
            child
                .terminate_tree()
                .expect("terminate timed-out fixture helper tree");
            panic!("fixture helper exceeded {HELPER_TIMEOUT:?}");
        }
    }
}

fn helper_port() -> u16 {
    std::env::var(HELPER_PORT)
        .expect("helper port")
        .parse()
        .expect("numeric helper port")
}

#[test]
fn fixture_network_helper() {
    let Ok(role) = std::env::var(HELPER_ROLE) else {
        return;
    };
    let port = helper_port();
    let address = match role.as_str() {
        "ipv4-allowed" => SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        "ipv6-denied" => SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port),
        "other-port-denied" => SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        other => panic!("unknown fixture-network helper role: {other}"),
    };
    let result = TcpStream::connect_timeout(&address, ACCEPT_TIMEOUT);
    if role == "ipv4-allowed" {
        result.unwrap_or_else(|error| panic!("allowed IPv4 proxy connect failed: {error}"));
    } else {
        let error = result.expect_err("fixture policy allowed a forbidden TCP connection");
        assert_eq!(
            error.kind(),
            io::ErrorKind::PermissionDenied,
            "forbidden {role} connect did not receive Seatbelt permission denial: {error}"
        );
    }
}

#[test]
fn fixture_profile_allows_only_ipv4_fixed_proxy_port() {
    let scratch = Scratch::new();
    let helper = scratch.helper();
    let profile = scratch.profile();
    let ipv4 = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind IPv4 proxy control");
    ipv4.set_nonblocking(true)
        .expect("make IPv4 control nonblocking");
    let proxy_port = ipv4.local_addr().expect("read IPv4 proxy port").port();
    let ipv6 = match TcpListener::bind((Ipv6Addr::LOCALHOST, proxy_port)) {
        Ok(listener) => listener,
        Err(error) if !ci_required() => {
            println!(
                "macOS fixture network probe skipped: cannot bind [::1]:{proxy_port}: {error}"
            );
            return;
        }
        Err(error) => panic!("CI requires [::1]:{proxy_port} control listener: {error}"),
    };
    ipv6.set_nonblocking(true)
        .expect("make IPv6 control nonblocking");
    let other_ipv4 = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind other-port control");
    other_ipv4
        .set_nonblocking(true)
        .expect("make other-port control nonblocking");
    let other_port = other_ipv4
        .local_addr()
        .expect("read other-port control")
        .port();

    parent_positive_control(
        &ipv4,
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), proxy_port),
        "IPv4",
    );
    parent_positive_control(
        &ipv6,
        SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), proxy_port),
        "IPv6",
    );
    parent_positive_control(
        &other_ipv4,
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), other_port),
        "other IPv4",
    );
    let policy = BrowserFixturePolicy {
        chrome_executable: helper,
        profile_dir: profile.clone(),
        proxy_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), proxy_port),
    };
    assert!(
        run_helper(policy.clone(), &profile, "ipv4-allowed", proxy_port).success(),
        "fixture profile rejected its IPv4 fixed proxy port"
    );
    wait_for_accept(&ipv4, "fixture IPv4 proxy");

    assert!(
        run_helper(policy.clone(), &profile, "ipv6-denied", proxy_port).success(),
        "fixture profile did not report IPv6 same-port denial"
    );
    assert_no_accept(&ipv6, "fixture IPv6 same-port");

    assert!(
        run_helper(policy, &profile, "other-port-denied", other_port).success(),
        "fixture profile did not report unrelated IPv4 port denial"
    );
    assert_no_accept(&other_ipv4, "fixture unrelated IPv4 port");
}
