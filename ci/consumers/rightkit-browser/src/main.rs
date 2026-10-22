use rightkit_browser::{
    actions_digest, chrome_for_testing_path, parse_netscape, BrowserError, BrowserSession,
    ClearOutcome, CookieExport, CookieExportError, FixtureAction, FixtureAttestation,
    FixtureBundle, FixtureCancellation, FixtureConfinementBackend, FixtureError, FixtureFile,
    FixtureIntent, FixtureOutcomeStatus, FixtureServer, FixtureSession, LaunchOptions, ManagedRoot,
    NetscapeCookie, FIXTURE_SCHEMA,
};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fs;
use std::io::{self, Read};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

fn require(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(message).into())
    }
}

fn required_env(key: &str) -> Result<String> {
    let value = std::env::var(key)?;
    require(
        !value.trim().is_empty(),
        "required qualification input is empty",
    )?;
    Ok(value)
}

fn hash_matches(path: &Path, expected: &str) -> Result<()> {
    require(
        expected.len() == 64 && expected.bytes().all(|b| b.is_ascii_hexdigit()),
        "executable SHA-256 must be exactly 64 hex digits",
    )?;
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    require(
        format!("{:x}", digest.finalize()).eq_ignore_ascii_case(expected),
        "pinned executable SHA-256 mismatch",
    )
}

/// Run before creating runtime threads. No fallback to user Chrome, caches or downloads.
fn bind_managed_assets() -> Result<PathBuf> {
    let root = ManagedRoot::at(PathBuf::from(required_env("RK_R2_CFT_ROOT")?));
    require(
        root.path().is_absolute(),
        "managed CFT root must be absolute",
    )?;
    let current = fs::canonicalize(root.current())?;
    let executable = fs::canonicalize(root.executable())?;
    require(
        executable.starts_with(&current),
        "CFT executable escapes managed current",
    )?;
    hash_matches(&executable, &required_env("RK_R2_CFT_SHA256")?)?;
    std::env::set_var("RIGHTKIT_CHROME_FOR_TESTING_ROOT", root.path());
    std::env::set_var("RIGHTKIT_CHROME_FOR_TESTING", &executable);
    std::env::set_var("RIGHTKIT_REQUIRE_ASSETS", "1");
    #[cfg(windows)]
    {
        let shell = fs::canonicalize(
            root.current()
                .join("chrome-headless-shell-win64/chrome-headless-shell.exe"),
        )?;
        require(
            shell.starts_with(&current),
            "headless shell escapes managed current",
        )?;
        hash_matches(&shell, &required_env("RK_R2_HEADLESS_SHELL_SHA256")?)?;
        std::env::set_var("RIGHTKIT_CHROME_HEADLESS_SHELL", shell);
    }
    require(
        fs::canonicalize(chrome_for_testing_path(None)?)? == executable,
        "public resolver did not select pinned managed executable",
    )?;
    println!("proof managed_cft_resolver=pass executable_sha256=verified");
    Ok(executable)
}

fn main() -> Result<()> {
    let executable = bind_managed_assets()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let scratch_root = PathBuf::from(required_env("RK_R2_SCRATCH")?);
        require(scratch_root.is_absolute(), "scratch root must be absolute")?;
        let scratch = tempfile::Builder::new()
            .prefix("browser-r2-")
            .tempdir_in(scratch_root)?;
        browser_tabs_and_export(&executable, scratch.path()).await?;
        fixture_case(false).await?;
        fixture_case(true).await?;
        println!("proof browser_r2=pass");
        Ok(())
    })
}

async fn browser_tabs_and_export(executable: &Path, scratch: &Path) -> Result<()> {
    let mut options = LaunchOptions::default();
    options.chrome_path = Some(executable.to_path_buf());
    options.launch_timeout = Duration::from_secs(30);
    options.request_timeout = Duration::from_secs(5);
    let session =
        tokio::time::timeout(Duration::from_secs(45), BrowserSession::launch(options)).await??;
    let profile = session.profile_dir().to_path_buf();
    let operation = tokio::time::timeout(Duration::from_secs(30), async {
        let first = session.new_page("about:blank").await?;
        let second = session.new_page("about:blank").await?;
        require(
            first.id() != second.id(),
            "tabs must have distinct identities",
        )?;
        session.select_page(first.id()).await?;
        require(
            session.current()?.id() == first.id(),
            "tab selection failed",
        )?;
        session.close_page(second.id()).await?;
        require(
            matches!(session.page(second.id()), Err(BrowserError::UnknownPage(_))),
            "closed tab remains registered",
        )?;

        let seed = vec![
            NetscapeCookie::new(".example.test", "sid", "r2-session-value")?
                .with_secure(true)
                .with_http_only(true),
            NetscapeCookie::new("sub.example.test", "pref", "r2-preference")?.with_path("/app")?,
            NetscapeCookie::new("unrelated.example", "outside", "r2-outside")?,
        ];
        require(
            first.import_cookies(&seed).await? == seed.len(),
            "cookie import count",
        )?;
        let stored = first.raw().get_cookies().await?;
        let mut observed = Vec::new();
        for cookie in stored {
            // These are session-cookie vectors. Refuse an unexpected persistent cookie
            // rather than silently discarding its expiry during conversion.
            require(
                cookie.session,
                "unexpected persistent cookie in isolated profile",
            )?;
            observed.push(
                NetscapeCookie::new(&cookie.domain, &cookie.name, &cookie.value)?
                    .with_path(&cookie.path)?
                    .with_secure(cookie.secure)
                    .with_http_only(cookie.http_only),
            );
        }
        require(
            observed.len() == seed.len(),
            "browser did not retain imported cookies",
        )?;
        export_contract(scratch, &observed)?;
        session.close_page(first.id()).await?;
        require(
            session.pages().is_empty(),
            "tab registry not empty after close",
        )?;
        println!("proof real_tabs_import_close=pass");
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    })
    .await;

    // Every success/error/operation-timeout takes owner-scoped shutdown. Only public
    // session ownership can terminate its process tree; never issue a PID/global kill.
    let graceful = tokio::time::timeout(Duration::from_secs(20), session.shutdown()).await;
    if graceful.is_err() {
        tokio::time::timeout(Duration::from_secs(15), session.cancel()).await?;
    }
    drop(session);
    require(
        !profile.exists(),
        "owned browser profile remained after shutdown",
    )?;
    println!("proof browser_owned_shutdown_profile_removed=pass");
    operation??;
    Ok(())
}

fn export_contract(scratch: &Path, observed: &[NetscapeCookie]) -> Result<()> {
    let path = scratch.join("auth/cookies.txt");
    let export = CookieExport::new(["example.test"])?.mark_http_only(true);
    let report = export.export_to(&path, observed)?;
    require(
        report.exported == 2 && report.out_of_scope == 1,
        "declared-domain subset count",
    )?;
    let old = fs::read(&path)?;
    let parsed = parse_netscape(std::str::from_utf8(&old)?)?;
    require(parsed.len() == 2, "Netscape round-trip count")?;
    require(
        parsed.iter().all(|cookie| export.matches(&cookie.domain)),
        "out-of-scope cookie escaped export filter",
    )?;
    require(
        parsed.iter().any(|cookie| {
            cookie.name == "sid" && cookie.secure && cookie.http_only && cookie.include_subdomains
        }),
        "secure, HttpOnly or domain field lost",
    )?;
    require(
        parsed
            .iter()
            .any(|cookie| cookie.name == "pref" && cookie.path == "/app"),
        "cookie path lost",
    )?;
    rightkit_fs::private::verify_owner_only(&path)?;
    require(
        rightkit_fs::private::is_owner_only_strict(&path)?,
        "cookie artifact is not strictly owner-only",
    )?;
    rightkit_fs::private::verify_owner_only(path.parent().ok_or("artifact parent missing")?)?;

    // Persistent expiry is an additional format vector, independent of live session
    // cookies above. Do not invent a persistent expiry for browser session cookies.
    let persistent_path = scratch.join("auth/persistent.txt");
    let persistent = NetscapeCookie::new("example.test", "persistent", "r2-persistent")?
        .with_expires(Some(1_900_000_000))?;
    export.export_to(&persistent_path, &[persistent.clone()])?;
    require(
        parse_netscape(&fs::read_to_string(&persistent_path)?)? == vec![persistent],
        "persistent expiry did not survive Netscape round-trip",
    )?;
    rightkit_fs::private::verify_owner_only(&persistent_path)?;
    CookieExport::clear(&persistent_path)?;

    let empty = export.export_to(&path, &[]);
    require(
        matches!(empty, Err(CookieExportError::NothingToExport { .. })),
        "empty export was not refused",
    )?;
    require(
        fs::read(&path)? == old,
        "empty export replaced old artifact",
    )?;
    let mut invalid = NetscapeCookie::new("example.test", "invalid", "r2-invalid")?;
    invalid.path = "relative".into();
    require(
        matches!(
            export.export_to(&path, &[invalid]),
            Err(CookieExportError::InvalidCookie { .. })
        ),
        "invalid export was not refused",
    )?;
    require(
        fs::read(&path)? == old,
        "invalid export replaced old artifact",
    )?;
    let mut engine_snapshot = Some(parsed);
    let cleared = CookieExport::clear_with(&path, |_| engine_snapshot = None)?;
    require(
        cleared == ClearOutcome::Removed && engine_snapshot.is_none(),
        "snapshot invalidation failed",
    )?;
    require(!path.exists(), "cleared cookie artifact remains")?;
    let mut absent_callback = false;
    let absent = CookieExport::clear_with(&path, |_| absent_callback = true)?;
    require(
        absent == ClearOutcome::Absent && absent_callback,
        "absent artifact did not invalidate snapshot",
    )?;
    println!("proof cookie_export_scope_netscape_private_preservation_clear=pass");
    Ok(())
}

fn fixture_files() -> Vec<FixtureFile> {
    vec![
        FixtureFile {
            path: "/index.html".into(),
            content_type: "text/html; charset=utf-8".into(),
            body: br#"<!doctype html><meta charset="utf-8"><title>R2 fixture</title>
<link rel="icon" href="data:,"><button id="commit">Commit</button>
<p id="result">pending</p><script src="/fixture.js"></script>"#
                .to_vec(),
        },
        FixtureFile {
            path: "/fixture.js".into(),
            content_type: "application/javascript; charset=utf-8".into(),
            body: br#"document.getElementById('commit').addEventListener('click', event => {
document.getElementById('result').innerText = event.isTrusted ? 'trusted' : 'untrusted';
});"#
                .to_vec(),
        },
    ]
}

async fn fixture_case(cancel: bool) -> Result<()> {
    let files = fixture_files();
    let digest = FixtureBundle::source_digest(&files)?;
    let bundle = FixtureBundle::verify("r2-consumer", &digest, files)?;
    let server = FixtureServer::prepare(bundle).await?;
    let actions = if cancel {
        vec![FixtureAction::AssertVisible {
            selector: "#never-created".into(),
            timeout: Some(5_000),
        }]
    } else {
        vec![
            FixtureAction::Click {
                selector: "#commit".into(),
            },
            FixtureAction::AssertText {
                selector: "#result".into(),
                text: "trusted".into(),
            },
        ]
    };
    let expected_actions = actions.len();
    let intent = FixtureIntent {
        schema: FIXTURE_SCHEMA.into(),
        fixture_id: "r2-consumer".into(),
        fixture_source_digest: digest,
        probe_id: if cancel { "r2-cancel" } else { "r2-pass" }.into(),
        actions_digest: actions_digest(&actions)?,
        exact_url: server.url("/index.html")?,
        actions,
        expires_after_ms: 30_000,
    };
    let launched = tokio::time::timeout(
        Duration::from_secs(45),
        FixtureSession::launch(server, intent),
    )
    .await?;
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        return match launched {
            Err(FixtureError::ConfinementUnsupported(detail)) => {
                eprintln!("proof fixture_native=unsupported typed=ConfinementUnsupported");
                Err(io::Error::new(io::ErrorKind::Unsupported, detail).into())
            }
            _ => Err(
                io::Error::other("unsupported native target did not return typed refusal").into(),
            ),
        };
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        let session = launched?;
        let initial = session.attestation().clone();
        let signal = FixtureCancellation::new();
        let cancel_signal = signal.clone();
        let mut execution = Box::pin(session.run_with_cancel(signal));
        // Keep running cleanup future after requesting cancellation; never abort it.
        let result = if cancel {
            tokio::select! {
                result = &mut execution => result,
                _ = tokio::time::sleep(Duration::from_millis(250)) => {
                    cancel_signal.cancel();
                    tokio::time::timeout(Duration::from_secs(45), &mut execution).await?
                }
            }
        } else {
            match tokio::time::timeout(Duration::from_secs(35), &mut execution).await {
                Ok(result) => result,
                Err(_) => {
                    cancel_signal.cancel();
                    let result =
                        tokio::time::timeout(Duration::from_secs(45), &mut execution).await?;
                    match &result {
                        Ok(outcome) => assert_terminal(&initial, &outcome.attestation).await?,
                        Err(failure) => assert_terminal(&initial, &failure.attestation).await?,
                    }
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "fixture exceeded normal run bound",
                    )
                    .into());
                }
            }
        };
        match result {
            Ok(outcome) => {
                assert_terminal(&initial, &outcome.attestation).await?;
                require(!cancel, "cancellation fixture unexpectedly passed")?;
                require(
                    outcome.status == FixtureOutcomeStatus::Passed && outcome.cleanup_complete,
                    "fixture outcome did not pass with cleanup",
                )?;
                require(
                    outcome.completed_actions == expected_actions,
                    "fixture actions not completed",
                )?;
            }
            Err(failure) => {
                assert_terminal(&initial, &failure.attestation).await?;
                require(
                    failure.cleanup_complete && failure.cleanup_error.is_none(),
                    "fixture failure lacks cleanup proof",
                )?;
                require(
                    cancel && matches!(failure.cause, FixtureError::Cancelled(_)),
                    "unexpected fixture failure class",
                )?;
                require(
                    failure.completed_actions == 0,
                    "cancelled missing action completed",
                )?;
            }
        }
        println!(
            "proof fixture_{}=pass terminal_identity_cleanup=verified",
            if cancel { "cancel" } else { "trusted_action" }
        );
        Ok(())
    }
}

async fn assert_terminal(
    initial: &FixtureAttestation,
    terminal: &FixtureAttestation,
) -> Result<()> {
    require(
        initial.browser_process_instance == terminal.browser_process_instance
            && initial.browser_session_id == terminal.browser_session_id
            && initial.profile_identity == terminal.profile_identity
            && initial.fixture_source_digest == terminal.fixture_source_digest
            && initial.actions_digest == terminal.actions_digest
            && initial.host_binding_id == terminal.host_binding_id
            && initial.browser_pid == terminal.browser_pid
            && initial.server_endpoint == terminal.server_endpoint
            && initial.server_instance_id == terminal.server_instance_id
            && initial.fixture_id == terminal.fixture_id
            && initial.probe_id == terminal.probe_id
            && initial.policy_digest == terminal.policy_digest
            && initial.page_id == terminal.page_id
            && initial.frame_id == terminal.frame_id
            && initial.loader_id == terminal.loader_id
            && initial.unique_context_id == terminal.unique_context_id,
        "terminal evidence changed owned fixture identity",
    )?;
    #[cfg(windows)]
    require(
        terminal.confinement_backend
            == FixtureConfinementBackend::WindowsAppcontainerPipeFulfillment,
        "Windows fixture did not attest AppContainer backend",
    )?;
    #[cfg(target_os = "macos")]
    require(
        terminal.confinement_backend == FixtureConfinementBackend::MacosSeatbeltProxy,
        "macOS fixture did not attest Seatbelt backend",
    )?;
    let cleanup = &terminal.output_cleanup;
    require(
        cleanup.browser_stopped
            && cleanup.transport_stopped
            && cleanup.proxy_stopped
            && cleanup.server_stopped
            && cleanup.profile_removed,
        "terminal cleanup evidence incomplete",
    )?;
    require(
        !Path::new(&terminal.profile_identity).exists(),
        "fixture profile remained",
    )?;
    let endpoint: SocketAddr = terminal.server_endpoint.parse()?;
    require(
        TcpStream::connect_timeout(&endpoint, Duration::from_millis(300)).is_err(),
        "fixture server still accepts connections",
    )?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        match TcpListener::bind(endpoint) {
            Ok(listener) => {
                drop(listener);
                break;
            }
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
