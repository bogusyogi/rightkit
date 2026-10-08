use rightkit_process::{
    OwnedChild, OwnedCommand, ReapOutcome, TerminationOptions, UnixContainment,
};
#[cfg(unix)]
use std::{io::Read, sync::mpsc};
use std::{
    io::{BufRead, BufReader},
    process::{ChildStdout, Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime},
};

fn helper(unix_script: &str, windows_script: &str) -> OwnedCommand {
    #[cfg(unix)]
    let mut command = {
        let _ = windows_script;
        let mut command = Command::new("/bin/sh");
        command.args(["-c", unix_script]);
        command
    };
    #[cfg(windows)]
    let mut command = {
        let _ = unix_script;
        let mut command = Command::new("powershell.exe");
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            windows_script,
        ]);
        command
    };
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    OwnedCommand::from_command(command)
}

fn sleeper() -> OwnedCommand {
    helper(
        "trap '' TERM; printf 'READY\\n'; exec sleep 2",
        "Write-Output 'READY'; Start-Sleep -Seconds 2",
    )
}

fn read_ready(child: &mut OwnedChild) -> BufReader<ChildStdout> {
    let mut stdout = BufReader::new(child.take_stdout().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "READY");
    stdout
}

#[test]
fn std_child_borrow_and_independent_exit_reader_share_status() {
    let mut child = sleeper().spawn().unwrap();
    {
        let mut native = child.child();
        assert!(native.stdin.as_mut().is_some());
        assert!(native.stdout.as_ref().is_some());
        assert!(native.stderr.as_ref().is_some());
        assert!(native.try_wait().unwrap().is_none());
    }
    let _stdout = read_ready(&mut child);
    assert!(child.take_stdin().is_some());
    assert!(child.take_stderr().is_some());
    let reader = child.process_handle().unwrap();
    let copy = reader.try_clone().unwrap();
    assert_eq!(copy.creation_time(), reader.creation_time());
    let waiter = thread::spawn(move || copy.wait().unwrap());
    let status = child.terminate_tree().unwrap();
    assert_eq!(waiter.join().unwrap(), status);
    assert_eq!(reader.try_wait().unwrap(), Some(status));
    assert_eq!(child.child().wait().unwrap(), status);
}

#[test]
fn creation_timestamp_is_present_on_native_hosts() {
    let mut child = sleeper().spawn().unwrap();
    let _stdout = read_ready(&mut child);
    let created = child.creation_time().expect("native creation timestamp");
    assert!(created <= SystemTime::now());
    assert!(created > SystemTime::UNIX_EPOCH);
    let reader = child.process_handle().unwrap();
    assert_eq!(reader.creation_time(), Some(created));
    #[cfg(windows)]
    {
        use std::os::windows::io::{AsHandle, AsRawHandle};
        assert!(child.creation_time_ticks().unwrap() > 0);
        let duplicate = reader.try_clone().unwrap();
        assert_ne!(
            reader.as_handle().as_raw_handle(),
            duplicate.as_handle().as_raw_handle()
        );
        assert_eq!(reader.creation_time_ticks(), child.creation_time_ticks());
    }
}

#[test]
fn term_then_kill_escalates_at_configured_grace() {
    let mut command = sleeper();
    command.unix_containment(UnixContainment::Session);
    let mut child = command.spawn().unwrap();
    let _stdout = read_ready(&mut child);
    let grace = Duration::from_millis(100);
    let started = Instant::now();
    let status = child
        .terminate_tree_with_options(TerminationOptions::default().with_grace(grace))
        .unwrap();
    assert!(started.elapsed() < grace + Duration::from_secs(1));
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert!(started.elapsed() >= grace);
        assert_eq!(status.signal(), Some(nix::libc::SIGKILL));
    }
    #[cfg(windows)]
    assert_eq!(status.code(), Some(1));
}

#[test]
fn zero_grace_supports_immediate_escalation_and_bounded_reap() {
    let mut child = sleeper().spawn().unwrap();
    let _stdout = read_ready(&mut child);
    let started = Instant::now();
    assert!(matches!(
        child
            .terminate_tree_bounded_with_options(
                Duration::from_secs(1),
                TerminationOptions::default(),
            )
            .unwrap(),
        ReapOutcome::Exited(_)
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn detached_child_survives_owner_drop_and_exit_reader_remains_usable() {
    let result = sleeper().spawn_uncontained_detached();
    #[cfg(windows)]
    if let Err(error) = &result {
        if error.raw_os_error() == Some(5) {
            // A surrounding job may prohibit breakaway. Escape must fail
            // without silently substituting a contained process.
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            eprintln!("detached survival requires a host job that permits breakaway");
            return;
        }
    }
    let mut child = result.unwrap();
    let stdout = read_ready(&mut child);
    let reader = child.process_handle().unwrap();
    drop(child);
    thread::sleep(Duration::from_millis(50));
    assert!(reader.try_wait().unwrap().is_none());
    assert!(reader
        .wait_timeout(Duration::from_secs(5))
        .unwrap()
        .is_some());
    drop(stdout);
}

#[cfg(unix)]
#[test]
fn session_mode_creates_session_and_group_led_by_child() {
    let mut command = sleeper();
    command.unix_containment(UnixContainment::Session);
    let mut child = command.spawn().unwrap();
    let _stdout = read_ready(&mut child);
    assert_eq!(
        unsafe { nix::libc::getsid(child.id() as i32) },
        child.id() as i32
    );
    assert_eq!(
        unsafe { nix::libc::getpgid(child.id() as i32) },
        child.id() as i32
    );
}

#[cfg(unix)]
#[test]
fn tree_termination_reaches_descendant_after_parent_was_reaped() {
    let mut child = helper("sleep 2 & printf 'READY\\n'; exit 0", "unused")
        .spawn()
        .unwrap();
    let mut stdout = read_ready(&mut child);
    assert!(child
        .wait_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap()
        .success());
    // Descendant inherited stdout; EOF proves its retained group was killed.
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.read_to_end(&mut bytes);
        let _ = sender.send(result);
    });
    assert!(receiver.recv_timeout(Duration::from_millis(20)).is_err());
    child
        .terminate_tree_with_options(TerminationOptions::default())
        .unwrap();
    receiver
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap();
    reader.join().unwrap();
}

#[cfg(windows)]
fn process_handle_that_is_not_a_job() -> std::os::windows::io::OwnedHandle {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    let handle =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, std::process::id()) }
            .unwrap();
    unsafe { OwnedHandle::from_raw_handle(handle.0 as _) }
}

#[cfg(windows)]
#[test]
fn best_effort_reports_real_assignment_failure_without_failing_spawn() {
    use rightkit_process::JobAssignmentMode;
    use std::os::windows::io::AsHandle;
    // A process handle is valid and duplicable, but cannot be used as a job.
    // This deterministically fails assignment without requiring special host jobs.
    for allowlisted in [false, true] {
        let not_a_job = process_handle_that_is_not_a_job();
        let mut command = sleeper();
        if allowlisted {
            use rightkit_process::{WindowsSpawnConfig, WindowsStdio};
            command
                .inherit_handles_only([])
                .windows_spawn_config(WindowsSpawnConfig {
                    stdin: WindowsStdio::Piped,
                    stdout: WindowsStdio::Piped,
                    stderr: WindowsStdio::Piped,
                    ..Default::default()
                });
        }
        command.windows_job(not_a_job.as_handle()).unwrap();
        command.job_assignment_mode(JobAssignmentMode::BestEffort);
        let mut child = command.spawn().unwrap();
        let _stdout = read_ready(&mut child);
        assert!(!child.job_assignment_failures().is_empty());
        assert_eq!(
            child.job_assignment_failures()[0].stage,
            "caller job assignment"
        );
        assert_eq!(child.job_assignment_failures()[0].raw_os_error(), Some(6));
        assert_eq!(
            child.job_assignment_failures()[0].error.raw_os_error(),
            Some(6)
        );
        assert!(child.try_wait().unwrap().is_none());
    }
}

#[cfg(windows)]
#[test]
fn strict_assignment_failure_is_still_fatal() {
    use std::os::windows::io::AsHandle;
    for allowlisted in [false, true] {
        let not_a_job = process_handle_that_is_not_a_job();
        let mut command = sleeper();
        if allowlisted {
            use rightkit_process::{WindowsSpawnConfig, WindowsStdio};
            command
                .inherit_handles_only([])
                .windows_spawn_config(WindowsSpawnConfig {
                    stdin: WindowsStdio::Piped,
                    stdout: WindowsStdio::Piped,
                    stderr: WindowsStdio::Piped,
                    ..Default::default()
                });
        }
        command.windows_job(not_a_job.as_handle()).unwrap();
        command.partial_spawn_cleanup_timeout(Duration::from_secs(5));
        assert_eq!(command.spawn().unwrap_err().raw_os_error(), Some(6));
    }
}

#[cfg(windows)]
#[test]
fn custom_windows_exit_code_is_visible_to_independent_waiter() {
    let mut child = sleeper().spawn().unwrap();
    let _stdout = read_ready(&mut child);
    let reader = child.process_handle().unwrap();
    let options = TerminationOptions::default().with_windows_exit_code(1067);
    assert_eq!(
        child.terminate_tree_with_options(options).unwrap().code(),
        Some(1067)
    );
    assert_eq!(reader.wait().unwrap().code(), Some(1067));
}

#[cfg(windows)]
#[test]
fn retained_job_terminates_descendant_after_parent_was_reaped() {
    use rightkit_process::AdoptedProcess;
    use std::num::NonZeroU32;
    let mut child = helper("unused",
        "$p = Start-Process powershell.exe -WindowStyle Hidden -ArgumentList '-NoLogo','-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 5' -PassThru; Write-Output $p.Id; exit 0",
    ).spawn().unwrap();
    let mut stdout = BufReader::new(child.take_stdout().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let descendant = AdoptedProcess::new(NonZeroU32::new(line.trim().parse().unwrap()).unwrap());
    assert!(child
        .wait_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap()
        .success());
    assert!(descendant.is_running().unwrap());
    child
        .terminate_tree_with_options(TerminationOptions::default().with_windows_exit_code(1067))
        .unwrap();
    let started = Instant::now();
    while descendant.is_running().unwrap() {
        assert!(started.elapsed() < Duration::from_secs(1));
        thread::sleep(Duration::from_millis(5));
    }
}
