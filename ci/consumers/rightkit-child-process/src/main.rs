// rightkit-child-process 0.5.5 registry consumer, outside the workspace.
//
// The child is this same executable, started through std::env::current_exe with a `--child <mode>`
// argument, so it exists on every runner without a shell, a network fetch or a second binary.
// Exercises run (exit status, bounded stdout and stderr), run_with_stdin (stdin delivered to the
// child), run_command (configured OwnedCommand with stdin), run_command_capture (first-N capture with
// truncation counts) and an execution deadline that terminates the child (timed_out).
// Also checks the 0.5.5 descriptor policy surface: the inherit_all_descriptors opt-out still runs a child,
// and (Unix) naming a descriptor with inherit_fd while opting out is refused as InvalidInput.
// Not covered: the strict incomplete-capture Err path; it is not deterministic on a trivial child.
// State: no files, no network, no real user data.
use rightkit_child_process::{
    run, run_command, run_command_capture, run_with_stdin, CaptureOptions, OwnedCommand, RunOptions,
};
use std::{
    io::{Read, Write},
    path::Path,
    time::Duration,
};

type Failure = Box<dyn std::error::Error>;

fn child(mode: &str) -> std::io::Result<()> {
    match mode {
        "ok" => {
            println!("child-ok");
            Ok(())
        }
        "fail" => {
            eprint!("child-fail");
            std::process::exit(3)
        }
        "echo" => {
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input)?;
            std::io::stdout().write_all(&input)?;
            std::io::stdout().flush()
        }
        "sleep" => {
            std::thread::sleep(Duration::from_secs(60));
            Ok(())
        }
        _ => std::process::exit(2),
    }
}

fn command(exe: &Path, mode: &str) -> OwnedCommand {
    let mut command = OwnedCommand::new(exe);
    command.command_mut().arg("--child").arg(mode);
    command
}

fn main() -> Result<(), Failure> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--child") {
        child(args.get(2).map(String::as_str).unwrap_or(""))?;
        return Ok(());
    }

    let exe = std::env::current_exe()?;
    let deadline = Duration::from_secs(30);

    // run: a zero exit with bounded stdout.
    let ok = run(&exe, ["--child", "ok"], &RunOptions::new(deadline))?;
    assert!(
        ok.status.success() && !ok.timed_out,
        "run must report a clean exit"
    );
    assert_eq!(ok.stdout.bytes, b"child-ok\n".to_vec());
    assert!(!ok.stdout.truncated);

    // run: a non-zero exit is Ok with the status set, and stderr is captured.
    let failed = run(&exe, ["--child", "fail"], &RunOptions::new(deadline))?;
    assert_eq!(
        failed.status.code(),
        Some(3),
        "non-zero exit is an Ok status, not an error"
    );
    assert_eq!(failed.stderr.bytes, b"child-fail".to_vec());

    // run_with_stdin: input reaches the child and comes back on stdout.
    let echoed = run_with_stdin(
        &exe,
        ["--child", "echo"],
        &b"payload"[..],
        &RunOptions::new(deadline),
    )?;
    assert_eq!(echoed.stdout.bytes, b"payload".to_vec());

    // run_command: the same exchange through a configured OwnedCommand.
    let via_command = run_command(
        command(&exe, "echo"),
        Some(&b"payload"[..]),
        &RunOptions::new(deadline),
    )?;
    assert_eq!(via_command.stdout.bytes, b"payload".to_vec());

    // run_command_capture: first four bytes kept, the rest drained and counted.
    let capped = run_command_capture(
        command(&exe, "ok"),
        None,
        &CaptureOptions::new().stdout_cap(4),
    )?;
    assert_eq!(capped.stdout.bytes, b"chil".to_vec());
    assert!(
        capped.stdout.truncated,
        "stdout longer than the cap must report truncation"
    );
    assert_eq!(
        capped.stdout.total_bytes, 9,
        "every produced byte is counted"
    );

    // Execution deadline: a sleeping child is terminated and reported as timed out.
    let timed = run(
        &exe,
        ["--child", "sleep"],
        &RunOptions::new(Duration::from_secs(1)),
    )?;
    assert!(
        timed.timed_out,
        "a child past its deadline must be reported as timed out"
    );

    // Descriptor policy (0.5.5): the opt-out is accepted and the child still runs.
    let mut opted_out = command(&exe, "ok");
    opted_out.inherit_all_descriptors(true);
    let opted = run_command(opted_out, None, &RunOptions::new(deadline))?;
    assert_eq!(opted.stdout.bytes, b"child-ok\n".to_vec());

    // Unix: naming a descriptor while opting out of sanitation conflicts and is refused before spawn.
    #[cfg(unix)]
    {
        let mut conflicting = command(&exe, "ok");
        conflicting.inherit_all_descriptors(true).inherit_fd(2);
        let refused = run_command(conflicting, None, &RunOptions::new(deadline));
        assert!(
            refused.is_err(),
            "inherit_fd with the opt-out must be refused"
        );
    }

    println!("rightkit-child-process 0.5.5 registry consumer passed");
    Ok(())
}
