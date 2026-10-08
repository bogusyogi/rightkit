//! `rightkit-sandbox-exec`: run one command inside a Windows AppContainer.
//!
//! ```text
//! rightkit-sandbox-exec [--read-only <dir>]... [--writable <dir>]... [--cwd <dir>]
//!                       [--env KEY=VALUE]... -- <command> [args...]
//! ```
//!
//! * `--read-only` directories can be read by the child but not written.
//! * `--writable` directories can be read and written.
//! * `--env` variables are the only caller-supplied environment the child receives
//!   (plus the platform defaults the crate always forwards).
//! * The child's stdout/stderr are relayed to ours and its exit code is propagated.
//! * Any setup failure fails closed with exit code 125 and a message on stderr.

#[cfg(windows)]
fn main() {
    std::process::exit(windows_main::run());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("rightkit-sandbox-exec: windows only");
    std::process::exit(2);
}

#[cfg(windows)]
mod windows_main {
    use rightkit_sandbox::{
        EnvPolicy, NetworkPolicy, ResourceAuthority, ResourceProfile, Sandbox, SandboxCommand,
        SandboxPolicy,
    };
    use std::ffi::OsString;
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::Duration;

    /// Exit code for every setup failure (fail closed).
    const SETUP_FAILED: i32 = 125;
    /// Upper bound on the child's run time; the broker owns real deadlines.
    const CHILD_DEADLINE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
    /// Per-stream capture cap for relayed output.
    const STREAM_CAP: usize = 64 * 1024 * 1024;

    #[derive(Debug, Default)]
    pub struct Invocation {
        pub read_only: Vec<PathBuf>,
        pub writable: Vec<PathBuf>,
        pub cwd: Option<PathBuf>,
        pub env: Vec<(String, String)>,
        pub program: Option<OsString>,
        pub args: Vec<OsString>,
    }

    /// Parse argv (without the program name). Pure, so it is unit-testable.
    pub fn parse<I>(argv: I) -> Result<Invocation, String>
    where
        I: IntoIterator<Item = OsString>,
    {
        let mut out = Invocation::default();
        let mut it = argv.into_iter();
        while let Some(flag) = it.next() {
            let flag_text = flag.to_string_lossy().into_owned();
            match flag_text.as_str() {
                "--" => {
                    let program = it
                        .next()
                        .ok_or_else(|| "missing command after --".to_string())?;
                    out.program = Some(program);
                    out.args = it.collect();
                    break;
                }
                "--read-only" => out.read_only.push(value_path(&mut it, &flag_text)?),
                "--writable" => out.writable.push(value_path(&mut it, &flag_text)?),
                "--cwd" => {
                    if out.cwd.is_some() {
                        return Err("--cwd given more than once".into());
                    }
                    out.cwd = Some(value_path(&mut it, &flag_text)?);
                }
                "--env" => {
                    let raw = value_string(&mut it, &flag_text)?;
                    out.env.push(parse_env(&raw)?);
                }
                other => return Err(format!("unknown argument: {other}")),
            }
        }
        if out.program.is_none() {
            return Err("no command given (expected `-- <command> [args...]`)".into());
        }
        Ok(out)
    }

    fn value_string(it: &mut impl Iterator<Item = OsString>, flag: &str) -> Result<String, String> {
        let value = it
            .next()
            .ok_or_else(|| format!("{flag} requires a value"))?;
        value
            .into_string()
            .map_err(|_| format!("{flag} value is not valid UTF-8"))
    }

    fn value_path(it: &mut impl Iterator<Item = OsString>, flag: &str) -> Result<PathBuf, String> {
        let value = it
            .next()
            .ok_or_else(|| format!("{flag} requires a value"))?;
        let path = PathBuf::from(value);
        if !path.is_absolute() {
            return Err(format!(
                "{flag} must be an absolute path: {}",
                path.display()
            ));
        }
        Ok(path)
    }

    fn parse_env(raw: &str) -> Result<(String, String), String> {
        let (key, value) = raw
            .split_once('=')
            .ok_or_else(|| format!("--env expects KEY=VALUE, got {raw:?}"))?;
        if key.is_empty() || key.contains('\0') || value.contains('\0') {
            return Err(format!("--env key is empty or value contains NUL: {key:?}"));
        }
        Ok((key.to_string(), value.to_string()))
    }

    fn fail(message: impl std::fmt::Display) -> i32 {
        eprintln!("rightkit-sandbox-exec: {message}");
        SETUP_FAILED
    }

    pub fn run() -> i32 {
        let invocation = match parse(std::env::args_os().skip(1)) {
            Ok(v) => v,
            Err(message) => return fail(message),
        };
        // `parse` guarantees a program is present.
        let program = invocation.program.clone().unwrap_or_default();

        let Some(cwd) = invocation.cwd.clone() else {
            return fail("--cwd is required");
        };

        let mut policy = SandboxPolicy::new()
            .network(NetworkPolicy::Deny)
            .env(EnvPolicy::default());
        for dir in &invocation.read_only {
            policy = policy.allow_read(dir.clone());
        }
        for dir in &invocation.writable {
            policy = policy.allow_write(dir.clone());
        }

        let mut command = SandboxCommand::new(program)
            .args(invocation.args.clone())
            .cwd(cwd);
        for (key, value) in &invocation.env {
            command = command.env(key.clone(), value.clone());
        }

        let sandbox = Sandbox::new(ResourceAuthority::new(ResourceProfile::Expanded));
        let child = match sandbox.spawn(&policy, command) {
            Ok(child) => child,
            Err(error) => return fail(format!("sandbox setup failed: {error}")),
        };
        let output = match child.wait_with_output_capped(CHILD_DEADLINE, STREAM_CAP) {
            Ok(output) => output,
            Err(error) => return fail(format!("waiting for sandboxed child failed: {error}")),
        };

        if output.timed_out {
            return fail("sandboxed child exceeded its deadline and was terminated");
        }
        let _ = std::io::stdout().write_all(&output.stdout);
        let _ = std::io::stderr().write_all(&output.stderr);
        if output.truncated {
            eprintln!(
                "rightkit-sandbox-exec: child output exceeded the capture cap and was truncated"
            );
        }
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        match output.status.and_then(|status| status.code()) {
            Some(code) => code,
            None => fail("sandboxed child ended without an exit code"),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn argv(items: &[&str]) -> Vec<OsString> {
            items.iter().map(OsString::from).collect()
        }

        #[test]
        fn parses_full_invocation() {
            let parsed = parse(argv(&[
                "--read-only",
                "C:\\ro",
                "--writable",
                "C:\\rw",
                "--cwd",
                "C:\\ro",
                "--env",
                "A=b=c",
                "--",
                "cmd.exe",
                "/c",
                "echo hi",
            ]))
            .unwrap();
            assert_eq!(parsed.read_only, vec![PathBuf::from("C:\\ro")]);
            assert_eq!(parsed.writable, vec![PathBuf::from("C:\\rw")]);
            assert_eq!(parsed.env, vec![("A".to_string(), "b=c".to_string())]);
            assert_eq!(parsed.program, Some(OsString::from("cmd.exe")));
            assert_eq!(
                parsed.args,
                vec![OsString::from("/c"), OsString::from("echo hi")]
            );
        }

        #[test]
        fn rejects_missing_command_and_relative_paths() {
            assert!(parse(argv(&["--read-only", "C:\\ro"])).is_err());
            assert!(parse(argv(&["--read-only", "relative", "--", "x"])).is_err());
            assert!(parse(argv(&["--bogus", "--", "x"])).is_err());
            assert!(parse(argv(&["--env", "NOEQUALS", "--", "x"])).is_err());
        }
    }
}
