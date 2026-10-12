//! Native AppContainer egress proof for FixtureSession's pipe-pair launch path.
//!
//! AppContainers have a separate loopback exemption mechanism, so same-host non-loopback is only
//! a narrow listener/no-hit control. A separate TCP-only remote control proves outbound egress
//! denial after that endpoint accepts the same helper unsandboxed.
#![cfg(windows)]

#[path = "common/mod.rs"]
mod common;

use rightkit_sandbox::{
    EnvPolicy, NetworkPolicy, ResourceAuthority, ResourceProfile, Sandbox, SandboxCommand,
    SandboxPolicy,
};
use std::collections::BTreeSet;
use std::fs;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CHILD_DEADLINE: Duration = Duration::from_secs(20);
const LOCAL_BLACKHOLE_DEADLINE: Duration = Duration::from_secs(2);
const LOCAL_BLACKHOLE_EXIT: i32 = 18;
const ACCEPT_SETTLE: Duration = Duration::from_millis(300);
const DENIED_EXIT: i32 = 17;
const REMOTE_CONTROL_DEADLINE: Duration = Duration::from_secs(10);
// Cloudflare advertises these IPv4 resolver endpoints on TCP 443:
// https://blog.cloudflare.com/announcing-ddr-support/
const REMOTE_CONTROLS: &[(Ipv4Addr, u16)] = &[
    (Ipv4Addr::new(1, 1, 1, 1), 443),
    (Ipv4Addr::new(1, 0, 0, 1), 443),
];

struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(name: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("rk-sandbox-{name}-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path).expect("create scratch directory");
        let canonical = fs::canonicalize(&path).expect("canonicalize scratch directory");
        let rendered = canonical.to_string_lossy();
        let plain = rendered
            .strip_prefix(r"\\?\")
            .unwrap_or(&rendered)
            .to_string();
        Self(PathBuf::from(plain))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct TestProfileCleanup {
    name: String,
    deleted: bool,
}

impl TestProfileCleanup {
    fn for_policy(policy: &SandboxPolicy) -> Self {
        let name = rightkit_sandbox::appcontainer_profile_name_for_test(policy)
            .expect("derive test AppContainer profile name");
        common::assert_test_name(&name, "rightkit.sandbox.");
        Self {
            name,
            deleted: false,
        }
    }

    fn delete_after_child_exit(&mut self) {
        let deleted = rightkit_sandbox::delete_appcontainer_profile_for_test(&self.name)
            .expect("delete test AppContainer profile");
        assert!(
            deleted,
            "test AppContainer profile remained registered: {}",
            self.name
        );
        println!("windows outbound proof profile_deleted=true");
        self.deleted = true;
    }
}

impl Drop for TestProfileCleanup {
    fn drop(&mut self) {
        if !self.deleted {
            let _ = rightkit_sandbox::delete_appcontainer_profile_for_test(&self.name);
        }
    }
}

struct ListenerProbe {
    port: u16,
    accepted: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ListenerProbe {
    fn start() -> Self {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0))
            .expect("bind host listener on every IPv4 interface");
        listener
            .set_nonblocking(true)
            .expect("make host listener nonblocking");
        let port = listener
            .local_addr()
            .expect("read host listener address")
            .port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_accepted = accepted.clone();
        let worker_stop = stop.clone();
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((_stream, _peer)) => {
                        worker_accepted.fetch_add(1, Ordering::AcqRel);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            port,
            accepted,
            stop,
            worker: Some(worker),
        }
    }

    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::Acquire)
    }

    fn wait_for_next_accept(&self, previous: usize) -> bool {
        let deadline = Instant::now() + ACCEPT_SETTLE;
        while Instant::now() < deadline {
            if self.accepted() > previous {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        self.accepted() > previous
    }

    fn finish(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("listener worker must join");
        }
    }
}

impl Drop for ListenerProbe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn non_loopback_ipv4_addresses() -> Vec<Ipv4Addr> {
    let output = ProcessCommand::new("ipconfig.exe")
        .output()
        .expect("run ipconfig to enumerate host IPv4 interfaces");
    assert!(
        output.status.success(),
        "ipconfig failed: {:?}",
        output.status
    );

    let mut addresses = BTreeSet::new();
    for token in String::from_utf8_lossy(&output.stdout).split_whitespace() {
        let token = token.trim_matches(|byte: char| !byte.is_ascii_digit() && byte != '.');
        let Ok(address) = token.parse::<Ipv4Addr>() else {
            continue;
        };
        // `ipconfig` also prints masks and gateways. A port-zero bind succeeds only for an
        // address assigned to this host; the temporary listener drops immediately.
        let local_interface = TcpListener::bind((address, 0)).is_ok();
        if local_interface
            && !address.is_loopback()
            && !address.is_unspecified()
            && !address.is_multicast()
        {
            addresses.insert(address);
        }
    }
    assert!(
        !addresses.is_empty(),
        "Windows host exposed no non-loopback IPv4 address; narrow local listener control cannot use loopback"
    );
    addresses.into_iter().collect()
}

fn tcp_probe_script(root: &Path) -> PathBuf {
    let script = root.join("tcp-probe.ps1");
    fs::write(
        &script,
        r#"$status = 19
$client = [System.Net.Sockets.TcpClient]::new()
try {
    $timeoutMs = [int]$env:RIGHTKIT_SANDBOX_OUTBOUND_TIMEOUT_MS
    if ($timeoutMs -gt 0) {
        $connect = $client.ConnectAsync($env:RIGHTKIT_SANDBOX_OUTBOUND_HOST, [int]$env:RIGHTKIT_SANDBOX_OUTBOUND_PORT)
        if ($connect.Wait($timeoutMs)) {
            $connect.GetAwaiter().GetResult()
            $status = 0
        } else {
            $status = 18
        }
    } else {
        $client.Connect($env:RIGHTKIT_SANDBOX_OUTBOUND_HOST, [int]$env:RIGHTKIT_SANDBOX_OUTBOUND_PORT)
        $status = 0
    }
} catch {
    $exception = $_.Exception
    for ($depth = 0; $depth -lt 8 -and $null -ne $exception; $depth++) {
        if ($exception -is [System.Net.Sockets.SocketException] -and $exception.NativeErrorCode -eq 10013) {
            $status = 17
            break
        }
        $exception = $exception.InnerException
    }
} finally {
    $client.Dispose()
}
if ($env:RIGHTKIT_SANDBOX_OUTBOUND_HOLD -eq "1") {
    Start-Sleep -Seconds 2
}
exit $status
"#,
    )
    .expect("write TCP probe script");
    script
}

fn tcp_probe_command(
    script: &Path,
    host: Ipv4Addr,
    port: u16,
    hold: bool,
    connect_timeout: Option<Duration>,
) -> SandboxCommand {
    SandboxCommand::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-File"])
        .arg(script.as_os_str())
        .env("RIGHTKIT_SANDBOX_OUTBOUND_HOST", host.to_string())
        .env("RIGHTKIT_SANDBOX_OUTBOUND_PORT", port.to_string())
        .env(
            "RIGHTKIT_SANDBOX_OUTBOUND_HOLD",
            if hold { "1" } else { "0" },
        )
        .env(
            "RIGHTKIT_SANDBOX_OUTBOUND_TIMEOUT_MS",
            connect_timeout
                .map(|timeout| timeout.as_millis().to_string())
                .unwrap_or_default(),
        )
}

fn output_completed(output: &rightkit_sandbox::SandboxOutput, label: &str) {
    assert!(!output.timed_out, "{label} exceeded bounded deadline");
    assert!(!output.truncated, "{label} output was truncated");
}

fn supervised_probe_env() -> EnvPolicy {
    EnvPolicy {
        inherit: true,
        ..EnvPolicy::default()
    }
}

/// Returns whether `pid` has an AppContainer token & its capability SIDs.
fn token_capabilities(pid: u32) -> (bool, Vec<String>) {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
    }
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn ConvertSidToStringSidW(sid: *mut c_void, text: *mut *mut u16) -> i32;
        fn GetTokenInformation(
            token: *mut c_void,
            class: u32,
            info: *mut c_void,
            len: u32,
            returned: *mut u32,
        ) -> i32;
        fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
    }

    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const TOKEN_CAPABILITIES: u32 = 30;
    const TOKEN_IS_APP_CONTAINER: u32 = 29;
    const TOKEN_QUERY: u32 = 0x8;

    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        assert!(!process.is_null(), "open confined child process");
        let mut token = std::ptr::null_mut();
        assert_ne!(OpenProcessToken(process, TOKEN_QUERY, &mut token), 0);

        let mut is_container = 0u32;
        let mut length = 0u32;
        assert_ne!(
            GetTokenInformation(
                token,
                TOKEN_IS_APP_CONTAINER,
                (&mut is_container as *mut u32).cast(),
                4,
                &mut length,
            ),
            0
        );
        GetTokenInformation(
            token,
            TOKEN_CAPABILITIES,
            std::ptr::null_mut(),
            0,
            &mut length,
        );
        assert!(length > 0, "query AppContainer token capabilities size");
        // u64 storage preserves the pointer alignment of TOKEN_GROUPS' SID array.
        let mut buffer = vec![0u64; (length as usize).div_ceil(std::mem::size_of::<u64>())];
        assert_ne!(
            GetTokenInformation(
                token,
                TOKEN_CAPABILITIES,
                buffer.as_mut_ptr().cast(),
                length,
                &mut length,
            ),
            0
        );
        let count = *(buffer.as_ptr() as *const u32) as usize;
        let groups =
            (buffer.as_ptr() as *const u8).add(std::mem::size_of::<usize>()) as *const [usize; 2];
        let mut capabilities = Vec::with_capacity(count);
        for index in 0..count {
            let sid = (*groups.add(index))[0] as *mut c_void;
            let mut text = std::ptr::null_mut();
            assert_ne!(ConvertSidToStringSidW(sid, &mut text), 0);
            let units = (0..).take_while(|&at| *text.add(at) != 0).count();
            capabilities.push(String::from_utf16_lossy(std::slice::from_raw_parts(
                text, units,
            )));
            LocalFree(text.cast());
        }
        CloseHandle(token);
        CloseHandle(process);
        (is_container != 0, capabilities)
    }
}

#[test]
fn pipe_pair_appcontainer_without_internet_client_cannot_reach_host_non_loopback_listener() {
    if !common::real_user_stores_allowed(
        "pipe_pair_appcontainer_without_internet_client_cannot_reach_host_non_loopback_listener",
    ) {
        return;
    }

    let scratch = ScratchDir::new("windows-outbound-denial");
    let script = tcp_probe_script(scratch.path());
    let listener = ListenerProbe::start();
    let sandbox = Sandbox::new(ResourceAuthority::new(ResourceProfile::Expanded));

    let (host, positive_before) = non_loopback_ipv4_addresses()
        .into_iter()
        .find_map(|host| {
            let before = listener.accepted();
            let output = sandbox
                .spawn_supervised(
                    tcp_probe_command(&script, host, listener.port, false, None),
                    &supervised_probe_env(),
                )
                .expect("spawn unsandboxed TCP positive control")
                .wait_with_output(CHILD_DEADLINE)
                .expect("wait for unsandboxed TCP positive control");
            output_completed(&output, "unsandboxed TCP positive control");
            if output.status.is_some_and(|status| status.success())
                && listener.wait_for_next_accept(before)
            {
                println!(
                    "windows outbound local control endpoint={host}:{} unsandboxed_exit=0 listener_hit=true",
                    listener.port
                );
                Some((host, before))
            } else {
                None
            }
        })
        .expect("no host non-loopback interface reached its own listener unsandboxed");
    assert!(
        listener.accepted() > positive_before,
        "unsandboxed control did not reach selected host listener"
    );

    let policy = SandboxPolicy::new()
        .allow_read(scratch.path())
        .network(NetworkPolicy::Deny);
    let mut profile = TestProfileCleanup::for_policy(&policy);
    let denied_before = listener.accepted();
    let pipe_spawn = sandbox
        .spawn_with_pipe_pair(
            &policy,
            tcp_probe_command(
                &script,
                host,
                listener.port,
                false,
                Some(LOCAL_BLACKHOLE_DEADLINE),
            ),
            "--remote-debugging-io-pipes",
        )
        .expect("spawn network-denied AppContainer through pipe-pair path");
    let output = {
        let rightkit_sandbox::WindowsPipeSpawn { child, io } = pipe_spawn;
        let output = child
            .wait_with_output(LOCAL_BLACKHOLE_DEADLINE + Duration::from_secs(3))
            .expect("wait for network-denied AppContainer");
        drop(io);
        output
    };
    output_completed(&output, "network-denied AppContainer");
    let local_exit = output.status.and_then(|status| status.code());
    assert!(
        matches!(local_exit, Some(DENIED_EXIT | LOCAL_BLACKHOLE_EXIT)),
        "host-non-loopback control must return access-denied or explicit bounded-blackhole exit: {local_exit:?}"
    );
    assert!(
        !listener.wait_for_next_accept(denied_before),
        "network-denied AppContainer reached host listener"
    );
    let local_behavior = if local_exit == Some(DENIED_EXIT) {
        "access_denied_no_hit"
    } else {
        "bounded_blackhole_no_hit"
    };
    println!(
        "windows outbound narrow local control endpoint={host}:{} exit={} behavior={local_behavior}",
        listener.port,
        local_exit.unwrap_or_default()
    );

    // `wait_with_output` consumes and drops the confined child, which closes its job before
    // releasing its profile lease. Successful deletion proves no live job/profile holder remains.
    profile.delete_after_child_exit();
    listener.finish();
}

#[test]
fn pipe_pair_appcontainer_without_internet_client_cannot_reach_remote_tcp_control() {
    if !common::real_user_stores_allowed(
        "pipe_pair_appcontainer_without_internet_client_cannot_reach_remote_tcp_control",
    ) {
        return;
    }

    let scratch = ScratchDir::new("windows-remote-outbound-denial");
    let script = tcp_probe_script(scratch.path());
    let sandbox = Sandbox::new(ResourceAuthority::new(ResourceProfile::Expanded));
    let (host, port) = REMOTE_CONTROLS
        .iter()
        .copied()
        .find(|&(host, port)| {
            let output = sandbox
                .spawn_supervised(
                    tcp_probe_command(&script, host, port, false, None),
                    &supervised_probe_env(),
                )
                .expect("spawn unsandboxed remote TCP positive control")
                .wait_with_output(REMOTE_CONTROL_DEADLINE)
                .expect("wait for unsandboxed remote TCP positive control");
            let accepted = !output.timed_out
                && !output.truncated
                && output.status.is_some_and(|status| status.success());
            if accepted {
                println!(
                    "windows outbound remote control endpoint={host}:{port} unsandboxed_exit=0"
                );
            }
            accepted
        })
        .expect("no remote TCP control accepted an unsandboxed connection");

    let policy = SandboxPolicy::new()
        .allow_read(scratch.path())
        .network(NetworkPolicy::Deny);
    let mut profile = TestProfileCleanup::for_policy(&policy);
    let pipe_spawn = sandbox
        .spawn_with_pipe_pair(
            &policy,
            tcp_probe_command(&script, host, port, true, None),
            "--remote-debugging-io-pipes",
        )
        .expect("spawn network-denied AppContainer through pipe-pair path");
    let rightkit_sandbox::WindowsPipeSpawn { child, io } = pipe_spawn;
    let (is_container, capabilities) = token_capabilities(child.id());
    assert!(
        is_container,
        "pipe-pair child must have an AppContainer token"
    );
    assert!(
        !capabilities.iter().any(|sid| sid == "S-1-15-3-1"),
        "NetworkPolicy::Deny pipe-pair child unexpectedly has internetClient: {capabilities:?}"
    );
    println!(
        "windows outbound remote token appcontainer=true internet_client=false endpoint={host}:{port}"
    );
    let output = child
        .wait_with_output(CHILD_DEADLINE)
        .expect("wait for network-denied AppContainer remote control");
    drop(io);
    output_completed(&output, "network-denied AppContainer remote control");
    assert_eq!(
        output.status.and_then(|status| status.code()),
        Some(DENIED_EXIT),
        "same helper reached remote TCP control despite NetworkPolicy::Deny"
    );
    println!("windows outbound remote denial endpoint={host}:{port} exit={DENIED_EXIT}");

    profile.delete_after_child_exit();
}
