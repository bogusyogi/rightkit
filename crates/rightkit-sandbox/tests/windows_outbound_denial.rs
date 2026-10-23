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

/// Bound for a confined PowerShell probe to start, run its 5 s bounded connect and exit. Measured on
/// the windows-2025 runner (hosted 39885718722, `confined_powershell_starts_and_prints_within_the_wide_bound`):
/// a script that only prints and exits took 26 056 ms inside a network-denied AppContainer and
/// exited 0, so a 20 s bound never reached the connect phase there (39872794369, 39881928960,
/// 39884672645 timed out with no output; 39878045340 started faster and passed). The denial proof
/// is the exit code (17 or 18), not the time; the bound only has to outlast a cold start.
const CHILD_DEADLINE: Duration = Duration::from_secs(90);
/// Wide bound for the startup diagnostic only; its elapsed time is the evidence, not the bound.
const STARTUP_PROBE_DEADLINE: Duration = Duration::from_secs(120);
const LOCAL_BLACKHOLE_DEADLINE: Duration = Duration::from_secs(2);
const LOCAL_BLACKHOLE_EXIT: i32 = 18;
const ACCEPT_SETTLE: Duration = Duration::from_millis(300);
const DENIED_EXIT: i32 = 17;
const REMOTE_CONTROL_DEADLINE: Duration = Duration::from_secs(10);
/// Connect bound of both remote probes. The unsandboxed control must connect within it and the
/// confined probe must not: a connect with no bound waits out the OS SYN retry (about 21 s on
/// Windows), longer than `CHILD_DEADLINE`, so a silently dropped connect looked like a hang.
const REMOTE_CONNECT_BOUND: Duration = Duration::from_secs(5);
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
Write-Output ("probe start " + [DateTime]::UtcNow.ToString("o"))
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
Write-Output ("probe connect-phase done status=" + $status + " " + [DateTime]::UtcNow.ToString("o"))
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

/// Diagnostic for the two bounded-deadline misses above (hosted 39872794369, 39881928960,
/// 39884672645: `elapsed_ms=20018 timed_out=true exit=Some(1) stdout="" stderr=""`): the confined
/// PowerShell wrote nothing at all within `CHILD_DEADLINE`, so the first question is whether
/// PowerShell starts inside a network-denied lowbox on this image at all, and how long that takes.
/// No TCP: the script only prints and exits. The bound is deliberately wide; the printed elapsed
/// time is the evidence.
#[test]
fn confined_powershell_starts_and_prints_within_the_wide_bound() {
    if !common::real_user_stores_allowed(
        "confined_powershell_starts_and_prints_within_the_wide_bound",
    ) {
        return;
    }
    let scratch = ScratchDir::new("windows-confined-powershell-startup");
    let script = scratch.path().join("startup-probe.ps1");
    fs::write(
        &script,
        "Write-Output (\"probe start \" + [DateTime]::UtcNow.ToString(\"o\"))\nexit 0\n",
    )
    .expect("write startup probe script");
    let sandbox = Sandbox::new(ResourceAuthority::new(ResourceProfile::Expanded));
    let policy = SandboxPolicy::new()
        .allow_read(scratch.path())
        .network(NetworkPolicy::Deny);
    let mut profile = TestProfileCleanup::for_policy(&policy);
    let command = SandboxCommand::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-File"])
        .arg(script.as_os_str());
    let started = Instant::now();
    let pipe_spawn = sandbox
        .spawn_with_pipe_pair(&policy, command, "--remote-debugging-io-pipes")
        .expect("spawn network-denied AppContainer PowerShell through pipe-pair path");
    let rightkit_sandbox::WindowsPipeSpawn { child, io } = pipe_spawn;
    let output = child
        .wait_with_output(STARTUP_PROBE_DEADLINE)
        .expect("wait for the confined PowerShell startup probe");
    drop(io);
    println!(
        "windows confined powershell startup elapsed_ms={} timed_out={} exit={:?} stdout={:?} stderr={:?}",
        started.elapsed().as_millis(),
        output.timed_out,
        output.status.and_then(|status| status.code()),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    profile.delete_after_child_exit();
    assert!(
        !output.timed_out,
        "confined PowerShell did not start and exit within {STARTUP_PROBE_DEADLINE:?}"
    );
    // The pipe-pair spawn carries the two auxiliary pipes, not the child's stdout: hosted
    // 39885718722 printed `exit=Some(0) stdout=""` for this script, so the exit code, not the
    // text, is the proof that PowerShell ran.
    assert_eq!(
        output.status.and_then(|status| status.code()),
        Some(0),
        "confined PowerShell must run the script to its `exit 0`"
    );
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
                    tcp_probe_command(&script, host, port, false, Some(REMOTE_CONNECT_BOUND)),
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
            tcp_probe_command(&script, host, port, true, Some(REMOTE_CONNECT_BOUND)),
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
    let started = Instant::now();
    let output = child
        .wait_with_output(CHILD_DEADLINE)
        .expect("wait for network-denied AppContainer remote control");
    drop(io);
    // Evidence first, verdict second: a deadline miss must say whether PowerShell started inside
    // the lowbox at all and whether the bounded connect expired (runs 39872794369 and 39881928960
    // timed out with no output recorded; 39878045340 passed).
    println!(
        "windows outbound remote confined elapsed_ms={} timed_out={} exit={:?} stdout={:?} stderr={:?}",
        started.elapsed().as_millis(),
        output.timed_out,
        output.status.and_then(|status| status.code()),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output_completed(&output, "network-denied AppContainer remote control");
    // The unsandboxed helper connected to this endpoint inside `REMOTE_CONNECT_BOUND` (exit 0), so
    // only the missing `internetClient` capability explains the confined helper not connecting:
    // either Winsock refuses at once (WSAEACCES, exit 17) or the AppContainer drops the SYN and
    // the bounded connect expires (exit 18). Hosted runners show the second mode, exactly as the
    // narrow local control does and as `report_denial` documents. Exit 0 (it connected) or
    // anything else (the helper failed some other way) still fails.
    let remote_exit = output.status.and_then(|status| status.code());
    assert!(
        matches!(remote_exit, Some(DENIED_EXIT | LOCAL_BLACKHOLE_EXIT)),
        "same helper reached remote TCP control despite NetworkPolicy::Deny, or failed for \
         another reason: {remote_exit:?}"
    );
    let remote_behavior = if remote_exit == Some(DENIED_EXIT) {
        "access_denied"
    } else {
        "bounded_blackhole"
    };
    println!(
        "windows outbound remote denial endpoint={host}:{port} exit={} behavior={remote_behavior}",
        remote_exit.unwrap_or_default()
    );

    profile.delete_after_child_exit();
}
