#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_seatbelt_live_smoke_enforces_tiers() {
        use std::net::TcpListener;
        use std::process::{Command, Output};

        fn run_wrapped(
            program: &str,
            args: &[String],
            workspace: &std::path::Path,
            tier: SandboxTier,
        ) -> Output {
            let (wrapped_program, wrapped_args) =
                crate::coding_tools::shell::macos_sandbox_wrap_for_tier(program, args, workspace, tier)
                    .expect("enabled tier must produce a sandbox-exec wrapper");
            Command::new(wrapped_program)
                .args(wrapped_args)
                .current_dir(workspace)
                .output()
                .expect("run sandbox-exec")
        }

        fn diagnostics(output: &Output) -> String {
            format!(
                "status={}; stdout={}; stderr={}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        }

        assert!(crate::coding_tools::shell::macos_sandbox_wrap_for_tier(
            "/usr/bin/true",
            &[],
            std::path::Path::new("/"),
            SandboxTier::Disabled,
        )
        .is_none());

        let current_dir = std::env::current_dir().expect("resolve test working directory");
        let root = tempfile::Builder::new()
            .prefix(".rightkit-seatbelt-live-")
            .tempdir_in(current_dir)
            .expect("create live-smoke root outside the sandbox workspace");
        let workspace = root.path().join("workspace");
        let sibling_temp = root.path().join("sibling-temp");
        std::fs::create_dir_all(&workspace).expect("create sandbox workspace");
        std::fs::create_dir_all(&sibling_temp).expect("create sibling temp directory");

        let workspace_file = workspace.join("allowed");
        let workspace_output = run_wrapped(
            "/usr/bin/touch",
            &[workspace_file.to_string_lossy().into_owned()],
            &workspace,
            SandboxTier::WorkspaceWrite,
        );
        assert!(
            workspace_output.status.success() && workspace_file.exists(),
            "workspace write should succeed: {}",
            diagnostics(&workspace_output)
        );

        let sibling_file = sibling_temp.join("denied");
        let sibling_output = run_wrapped(
            "/usr/bin/touch",
            &[sibling_file.to_string_lossy().into_owned()],
            &workspace,
            SandboxTier::WorkspaceWrite,
        );
        assert!(
            !sibling_output.status.success() && !sibling_file.exists(),
            "write to sibling temp directory should be denied: {}",
            diagnostics(&sibling_output)
        );

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind local network probe");
        let address = listener
            .local_addr()
            .expect("read local network probe address");
        let nc_args = vec![
            "-z".to_string(),
            "-G".to_string(),
            "2".to_string(),
            address.ip().to_string(),
            address.port().to_string(),
        ];
        let baseline = Command::new("/usr/bin/nc")
            .args(&nc_args)
            .output()
            .expect("run unwrapped local network probe");
        assert!(
            baseline.status.success(),
            "unwrapped local network probe must succeed: {}",
            diagnostics(&baseline)
        );
        let strict_output = run_wrapped("/usr/bin/nc", &nc_args, &workspace, SandboxTier::Strict);
        assert!(
            !strict_output.status.success(),
            "strict tier should deny outbound TCP: {}",
            diagnostics(&strict_output)
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_job_can_assign_spawned_child() {
        let mut child = rightkit_child_process::command_std("cmd")
            .args(["/C", "exit", "0"])
            .spawn()
            .expect("spawn cmd child");
        let job = WindowsJob::new(Some(8)).expect("create job");

        job.assign_child(&child).expect("assign child to job");

        assert!(child.wait().expect("wait child").success());
    }

    #[cfg(windows)]
    fn run_confined(container: &AppContainerProfile, cwd: &std::path::Path, args: &[&str]) -> u32 {
        let argv: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let cmd = std::path::PathBuf::from(system_root)
            .join("System32")
            .join("cmd.exe");
        container
            .spawn_confined(&cmd, &argv, cwd, None)
            .expect("spawn confined")
            .wait()
            .expect("wait for confined child")
    }

    #[cfg(windows)]
    #[test]
    fn windows_appcontainer_confines_workspace_without_env_gate() {
        let root = tempfile::tempdir().expect("create AppContainer test root");
        let granted = root.path().join("workspace");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&granted).expect("create workspace");
        std::fs::create_dir_all(&outside).expect("create outside directory");
        let secret = outside.join("secret.txt");
        std::fs::write(&secret, b"secret").expect("seed outside file");

        let container = AppContainerProfile::for_workspace(&granted)
            .expect("create workspace-scoped AppContainer");
        let sid = container.sid_string().expect("sid string");
        assert!(
            sid.starts_with("S-1-15-2-"),
            "not an AppContainer SID: {sid}"
        );
        assert_eq!(
            sid,
            AppContainerProfile::for_workspace(&granted)
                .expect("reopen workspace-scoped AppContainer")
                .sid_string()
                .expect("stable SID")
        );

        assert_eq!(run_confined(&container, &granted, &["/c", "exit", "0"]), 0);

        let inside_probe = granted.join("probe.txt");
        assert_eq!(
            run_confined(
                &container,
                &granted,
                &[
                    "/c",
                    "copy",
                    "/y",
                    "nul",
                    &inside_probe.display().to_string()
                ],
            ),
            0,
            "writes beneath granted workspace must be permitted"
        );

        let outside_probe = outside.join("probe.txt");
        assert_ne!(
            run_confined(
                &container,
                &granted,
                &[
                    "/c",
                    "copy",
                    "/y",
                    "nul",
                    &outside_probe.display().to_string()
                ],
            ),
            0,
            "writes outside workspace must be denied"
        );
        assert_ne!(
            run_confined(
                &container,
                &granted,
                &["/c", "type", &secret.display().to_string()]
            ),
            0,
            "reads outside workspace must be denied"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_appcontainer_denies_outbound_tcp_from_real_child() {
        use std::net::{TcpListener, TcpStream};
        use tokio::io::AsyncReadExt;

        let listener =
            TcpListener::bind(("127.0.0.1", 0)).expect("bind local AppContainer egress probe");
        let address = listener
            .local_addr()
            .expect("read local AppContainer egress probe address");
        TcpStream::connect(address)
            .expect("unconfined local AppContainer egress probe must connect");

        let root = tempfile::tempdir().expect("create AppContainer network root");
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        let container = AppContainerProfile::for_workspace(&workspace)
            .expect("create capability-free AppContainer");
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let powershell = std::path::PathBuf::from(system_root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        let script = format!(
            "$client = [System.Net.Sockets.TcpClient]::new(); [Console]::Out.WriteLine('RIGHTKIT_EGRESS_PROBE_STARTED'); try {{ $client.Connect('127.0.0.1', {}) }} catch {{ [Console]::Out.WriteLine('RIGHTKIT_EGRESS_CONNECT_DENIED'); exit 23 }}; $client.Dispose(); [Console]::Out.WriteLine('RIGHTKIT_EGRESS_CONNECTED'); exit 0",
            address.port()
        );
        let args = vec![
            "-NoLogo".to_string(),
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            script,
        ];

        let baseline = rightkit_child_process::command_std(&powershell)
            .args(&args)
            .output()
            .expect("run unconfined PowerShell network probe");
        assert!(
            baseline.status.success()
                && String::from_utf8_lossy(&baseline.stdout).contains("RIGHTKIT_EGRESS_CONNECTED"),
            "unconfined PowerShell network probe must connect: stdout={} stderr={}",
            String::from_utf8_lossy(&baseline.stdout),
            String::from_utf8_lossy(&baseline.stderr)
        );
        let mut child = crate::coding_tools::sandbox::spawn_confined_piped(
            &container,
            &powershell.display().to_string(),
            &args,
            &workspace,
            false,
        )
        .expect("spawn AppContainer PowerShell network probe");
        let mut stdout = child.take_stdout().expect("AppContainer probe stdout");
        let code = child
            .wait_code()
            .await
            .expect("wait for AppContainer PowerShell network probe");
        let mut output = Vec::new();
        stdout
            .read_to_end(&mut output)
            .await
            .expect("read AppContainer probe stdout");
        let output = String::from_utf8_lossy(&output);
        assert_eq!(
            code, 23,
            "AppContainer network probe must report connection denial: stdout={output}"
        );
        assert!(
            output.contains("RIGHTKIT_EGRESS_PROBE_STARTED")
                && output.contains("RIGHTKIT_EGRESS_CONNECT_DENIED")
                && !output.contains("RIGHTKIT_EGRESS_CONNECTED"),
            "AppContainer child must execute its TCP probe then report denial: stdout={output}"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_appcontainer_piped_spawn_preserves_async_output() {
        use tokio::io::AsyncReadExt;

        let root = tempfile::tempdir().expect("create AppContainer pipe root");
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        let container = AppContainerProfile::for_workspace(&workspace)
            .expect("create workspace-scoped AppContainer");
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let cmd = std::path::PathBuf::from(system_root)
            .join("System32")
            .join("cmd.exe");
        let args = vec!["/c".to_string(), "echo piped-output".to_string()];
        let mut child = crate::coding_tools::sandbox::spawn_confined_piped(
            &container,
            &cmd.display().to_string(),
            &args,
            &workspace,
            true,
        )
        .expect("spawn piped AppContainer child");
        let mut stdout = child.take_stdout().expect("stdout pipe");
        let code = child.wait_code().await.expect("wait piped child");
        let mut output = Vec::new();
        stdout
            .read_to_end(&mut output)
            .await
            .expect("read piped output");
        assert_eq!(code, 0);
        assert!(String::from_utf8_lossy(&output).contains("piped-output"));
    }

    #[cfg(windows)]
    fn powershell_exe() -> std::path::PathBuf {
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        std::path::PathBuf::from(system_root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe")
    }

    /// Pull the `cwd=` marker line out of child output and resolve it through
    /// the filesystem so junction spellings compare equal to the target.
    #[cfg(windows)]
    fn resolve_reported_cwd(output: &str) -> std::path::PathBuf {
        let start = output
            .find("cwd=")
            .map(|index| index + 4)
            .unwrap_or_else(|| panic!("child must report its working directory: {output}"));
        let end = output[start..]
            .find(['\r', '\n'])
            .map(|tail| start + tail)
            .unwrap_or(output.len());
        let reported = output[start..end].trim();
        std::fs::canonicalize(reported)
            .unwrap_or_else(|error| panic!("reported cwd {reported} must resolve: {error}"))
    }

    /// Piped confined execution must land in an explicitly requested
    /// workspace subdirectory, translated through the launch alias rather
    /// than collapsed to the workspace root.
    #[cfg(windows)]
    #[tokio::test]
    async fn windows_appcontainer_piped_spawn_preserves_nested_cwd() {
        use tokio::io::AsyncReadExt;

        let root = tempfile::tempdir().expect("create nested-cwd pipe root");
        let workspace = root.path().join("work space");
        let nested = workspace.join("packages").join("app");
        std::fs::create_dir_all(&nested).expect("create nested workspace dir");
        std::fs::write(nested.join("sentinel.txt"), "nested-content")
            .expect("seed nested sentinel");
        let container = AppContainerProfile::for_workspace(&workspace)
            .expect("create workspace-scoped AppContainer");
        let powershell = powershell_exe();
        let args = vec![
            "-NoLogo".to_string(),
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            "[Console]::Out.WriteLine('cwd=' + (Get-Location).Path); \
             [Console]::Out.WriteLine('sentinel=' + \
             (Get-Content -LiteralPath 'sentinel.txt' -Raw).Trim())"
                .to_string(),
        ];
        let mut child = crate::coding_tools::sandbox::spawn_confined_piped(
            &container,
            &powershell.display().to_string(),
            &args,
            &nested,
            false,
        )
        .expect("spawn piped child in nested workspace dir");
        let mut stdout = child.take_stdout().expect("stdout pipe");
        let mut stderr = child.take_stderr().expect("stderr pipe");
        let code = child.wait_code().await.expect("wait nested-cwd child");
        let mut output = Vec::new();
        stdout
            .read_to_end(&mut output)
            .await
            .expect("read nested-cwd output");
        let mut errors = Vec::new();
        stderr
            .read_to_end(&mut errors)
            .await
            .expect("read nested-cwd stderr");
        let output = format!(
            "{}\n[stderr]\n{}",
            String::from_utf8_lossy(&output),
            String::from_utf8_lossy(&errors)
        );
        assert_eq!(code, 0, "nested-cwd child must succeed: {output}");
        assert!(
            output.contains("sentinel=nested-content"),
            "relative read must observe the nested workspace dir: {output}"
        );
        assert_eq!(
            resolve_reported_cwd(&output),
            std::fs::canonicalize(&nested).unwrap(),
            "confined cwd must resolve to the requested subdirectory: {output}"
        );
    }

    /// The confined ConPTY path must consume the same prepared launch
    /// directory as piped launches: root requests land on the alias root and
    /// authorized subdirectories keep their workspace-relative suffix.
    #[cfg(windows)]
    #[tokio::test]
    async fn windows_appcontainer_pty_root_and_nested_cwd() {
        use rightkit_sandbox::execution::{
            CommandLaunchSpec, ProcessLaunchContext, PtySize, SandboxProcessProof,
        };
        use tokio::io::AsyncReadExt;

        let root = tempfile::tempdir().expect("create PTY cwd root");
        let workspace = root.path().join("work space");
        let nested = workspace.join("packages").join("app");
        std::fs::create_dir_all(&nested).expect("create nested workspace dir");
        std::fs::write(workspace.join("root.txt"), "root-content").expect("seed root sentinel");
        std::fs::write(nested.join("sentinel.txt"), "nested-content")
            .expect("seed nested sentinel");
        let powershell = powershell_exe();
        let size = PtySize {
            rows: 24,
            cols: 400,
            pixel_width: 0,
            pixel_height: 0,
        };
        for (cwd, sentinel, content) in [
            (workspace.clone(), "root.txt", "root-content"),
            (nested.clone(), "sentinel.txt", "nested-content"),
        ] {
            let script = format!(
                "[Console]::Out.WriteLine('cwd=' + (Get-Location).Path); \
                 [Console]::Out.WriteLine('sentinel=' + \
                 (Get-Content -LiteralPath '{sentinel}' -Raw).Trim())"
            );
            let spec = CommandLaunchSpec {
                program: powershell.display().to_string(),
                argv: vec![
                    "-NoLogo".to_string(),
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    script,
                ],
                cwd: cwd.display().to_string(),
                context: ProcessLaunchContext {
                    workspace_root: workspace.display().to_string(),
                    sandbox_profile: None,
                    sandbox_required: true,
                    network_required: false,
                },
                env: Vec::new(),
            };
            let proof = SandboxProcessProof {
                backend: "appcontainer".to_string(),
                profile: "test".to_string(),
                enforced: true,
                network_enforced: true,
            };
            let mut child = crate::coding_tools::managed_pty::spawn(
                &spec,
                size,
                &spec.program,
                &spec.argv,
                proof,
                Some(SandboxTier::WorkspaceWrite),
            )
            .expect("spawn confined PTY child");
            let mut stdout = child.take_stdout().expect("PTY stdout");
            let code = child.wait().await.expect("wait confined PTY child");
            let mut output = Vec::new();
            stdout
                .read_to_end(&mut output)
                .await
                .expect("read confined PTY output");
            let output = String::from_utf8_lossy(&output);
            assert_eq!(code, 0, "confined PTY child must succeed: {output}");
            assert!(
                output.contains(&format!("sentinel={content}")),
                "relative PTY read must observe {}: {output}",
                cwd.display()
            );
            assert_eq!(
                resolve_reported_cwd(&output),
                std::fs::canonicalize(&cwd).unwrap(),
                "confined PTY cwd must resolve to the requested directory: {output}"
            );
        }
    }

    /// Actual child confinement, not just location correctness: a child
    /// confined to workspace A must be denied the second workspace's objects
    /// through both the real path and that workspace's launch alias, while
    /// its own workspace remains usable.
    #[cfg(windows)]
    #[test]
    fn windows_appcontainer_child_denied_second_workspace_and_alias() {
        let root = tempfile::tempdir().expect("create cross-workspace root");
        let workspace_a = root.path().join("workspace a");
        let workspace_b = root.path().join("workspace b");
        std::fs::create_dir_all(&workspace_a).expect("create workspace a");
        std::fs::create_dir_all(&workspace_b).expect("create workspace b");
        let secret = workspace_b.join("secret.txt");
        std::fs::write(&secret, "workspace-b-secret").expect("seed workspace b secret");

        let container_a =
            AppContainerProfile::for_workspace(&workspace_a).expect("profile for workspace a");
        let container_b =
            AppContainerProfile::for_workspace(&workspace_b).expect("profile for workspace b");
        let alias_b = container_b
            .workspace_alias()
            .expect("workspace b launch alias")
            .to_path_buf();
        // Sanity: the alias is a real junction exposing B's content to the
        // (unconfined) test process, so denial below can only come from the
        // child's confinement.
        assert!(
            alias_b.join("secret.txt").is_file(),
            "workspace b alias must resolve to the workspace"
        );

        // Reads of B's secret are denied via its real path…
        assert_ne!(
            run_confined(
                &container_a,
                &workspace_a,
                &["/c", "type", &secret.display().to_string()],
            ),
            0,
            "reads of a second workspace must be denied"
        );
        // …and via the second workspace's own launch-alias spelling.
        assert_ne!(
            run_confined(
                &container_a,
                &workspace_a,
                &[
                    "/c",
                    "type",
                    &alias_b.join("secret.txt").display().to_string()
                ],
            ),
            0,
            "reads of a second workspace through its alias must be denied"
        );
        // Writes through B's alias are denied too.
        let alias_probe = alias_b.join("probe.txt");
        assert_ne!(
            run_confined(
                &container_a,
                &workspace_a,
                &[
                    "/c",
                    "copy",
                    "/y",
                    "nul",
                    &alias_probe.display().to_string()
                ],
            ),
            0,
            "writes to a second workspace through its alias must be denied"
        );
        assert!(
            !workspace_b.join("probe.txt").exists(),
            "denied alias write must not create objects in workspace b"
        );

        // Same executable, same launch path — first prove it launches inside
        // the granted workspace, then prove the only changed input (a cwd
        // outside it) is what gets refused. The command never executes in
        // any directory.
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let cmd = std::path::PathBuf::from(system_root)
            .join("System32")
            .join("cmd.exe");
        assert!(
            cmd.is_file(),
            "test requires a valid executable at {}",
            cmd.display()
        );
        let args = vec!["/c".to_string(), "exit 0".to_string()];
        let launched = container_a
            .spawn_confined(&cmd, &args, &workspace_a, None)
            .expect("the valid executable must launch inside the workspace");
        assert_eq!(
            launched.wait().expect("wait for in-workspace launch"),
            0,
            "in-workspace launch must exit cleanly"
        );
        assert!(
            container_a
                .spawn_confined(&cmd, &args, &workspace_b, None)
                .is_err(),
            "a cwd outside the workspace must refuse the launch entirely"
        );

        // Positive control: the child is confined to A, not universally
        // denied — its own workspace stays readable and writable.
        let inside_probe = workspace_a.join("probe.txt");
        assert_eq!(
            run_confined(
                &container_a,
                &workspace_a,
                &[
                    "/c",
                    "copy",
                    "/y",
                    "nul",
                    &inside_probe.display().to_string()
                ],
            ),
            0,
            "writes beneath the granted workspace must be permitted"
        );
    }

    /// A junction planted inside the workspace that points outside it must not
    /// widen the child's reach, directly or through a grandchild process:
    /// access is decided by the target's DACL, which never grants the
    /// workspace's AppContainer.
    #[cfg(windows)]
    #[test]
    fn windows_appcontainer_junction_and_child_process_cannot_escape_workspace() {
        let root = tempfile::tempdir().expect("create junction escape root");
        let workspace = root.path().join("workspace");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        std::fs::create_dir_all(&outside).expect("create outside directory");
        std::fs::write(outside.join("secret.txt"), "outside-secret").expect("seed secret");

        let container = AppContainerProfile::for_workspace(&workspace).expect("profile");
        let link = workspace.join("escape");
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output()
            .expect("run mklink");
        assert!(made.status.success(), "junction creation failed");

        let via_junction_write = link.join("probe.txt");
        assert_ne!(
            run_confined(
                &container,
                &workspace,
                &[
                    "/c",
                    "copy",
                    "/y",
                    "nul",
                    &via_junction_write.display().to_string()
                ],
            ),
            0,
            "write through a workspace junction to outside must be denied"
        );
        assert_ne!(
            run_confined(
                &container,
                &workspace,
                &["/c", "type", &link.join("secret.txt").display().to_string()],
            ),
            0,
            "read through a workspace junction to outside must be denied"
        );
        // Grandchild: confinement must be inherited by `cmd /c cmd /c ...`.
        let nested = format!(
            "cmd /c copy /y nul \"{}\"",
            outside.join("child-probe.txt").display()
        );
        assert_ne!(
            run_confined(&container, &workspace, &["/c", &nested]),
            0,
            "a child process must not escape the workspace confinement"
        );
        assert!(!outside.join("probe.txt").exists());
        assert!(!outside.join("child-probe.txt").exists());
        // Remove the junction itself (not its target) before tempdir cleanup.
        let _ = std::fs::remove_dir(&link);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_seatbelt_denies_outside_writes_symlink_escape_and_child_escape() {
        let base = std::env::temp_dir().join(format!("cr-sb-{}", std::process::id()));
        let ws = base.join("ws");
        let outside = std::path::PathBuf::from(std::env::var("HOME").unwrap_or("/Users".into()))
            .join(format!(".cr-sb-outside-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, ws.join("link")).unwrap();
        // Temp dirs are an allowed write root, so confine to a workspace that is
        // outside them is not possible here; the escape targets live in HOME.
        let profile = rightkit_sandbox::seatbelt::write_only_profile(
            std::slice::from_ref(&ws),
            rightkit_sandbox::NetworkPolicy::Deny,
        );
        let run = |script: String| {
            std::process::Command::new("/usr/bin/sandbox-exec")
                .args(["-p", &profile, "/bin/sh", "-c", &script])
                .status()
                .expect("spawn sandbox-exec")
        };
        assert!(run(format!("echo x > {}/a", ws.display())).success());
        assert!(!run(format!("echo x > {}/b", outside.display())).success());
        assert!(!run(format!("echo x > {}/link/c", ws.display())).success());
        assert!(!run(format!("/bin/sh -c 'echo x > {}/d'", outside.display())).success());
        for name in ["b", "c", "d"] {
            assert!(!outside.join(name).exists(), "{name} escaped the sandbox");
        }
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
