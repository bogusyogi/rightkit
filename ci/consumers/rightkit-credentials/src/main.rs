use std::error::Error as StdError;
#[cfg(all(feature = "db-session", any(windows, target_os = "macos")))]
use std::path::PathBuf;
#[cfg(any(target_os = "macos", all(feature = "db-session", windows)))]
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use rightkit_credentials::apple_helper::{AppleHelper, HelperTrust};
use rightkit_credentials::Error;

type Failure = Box<dyn StdError + Send + Sync>;
type Result<T> = std::result::Result<T, Failure>;

#[cfg(all(feature = "sqlcipher", feature = "sqlcipher-system"))]
compile_error!("choose exactly one SQLCipher provider");

fn require(ok: bool, message: &'static str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(std::io::Error::other(message).into())
    }
}

fn unique() -> Result<String> {
    Ok(format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ))
}

fn core() -> Result<()> {
    require(
        matches!(
            HelperTrust::team("invalid"),
            Err(Error::Invalid {
                what: "team id",
                ..
            })
        ),
        "invalid team identifier was not rejected with its typed error",
    )?;
    let trust = HelperTrust::team("RKTEST1234")?;
    require(trust.team_id() == "RKTEST1234", "team identity changed")?;
    let missing = std::env::current_dir()?.join(format!("missing-helper-{}", unique()?));
    require(
        !missing.exists(),
        "negative helper fixture unexpectedly exists",
    )?;
    require(
        matches!(
            AppleHelper::new_trusted(missing, trust).status(),
            Err(Error::Helper(_))
        ),
        "missing trusted helper was not refused",
    )?;
    #[cfg(target_os = "macos")]
    require(
        matches!(
            rightkit_credentials::apple_helper::resolve_bundled_login_item("../escape"),
            Err(Error::Invalid { .. })
        ),
        "invalid bundled helper name was not rejected",
    )?;
    println!("core consumer assertions passed");
    Ok(())
}

#[cfg(target_os = "macos")]
fn native_helper() -> Result<()> {
    use rightkit_credentials::apple_helper::resolve_bundled_login_item;

    let name = std::env::var("RK_CONSUMER_HELPER_NAME")?;
    let team = std::env::var("RK_CONSUMER_HELPER_TEAM_ID")?;
    let helper = resolve_bundled_login_item(&name)?;
    let trust = HelperTrust::team(&team)?;
    // Presence/status only: no read, write, delete, authentication or secret payload.
    let status = AppleHelper::new_trusted(&helper, trust)
        .timeout(Duration::from_secs(10))
        .status()?;
    require(status.ready, "signed nested helper did not report ready")?;
    // Same installed helper must be rejected under another valid team requirement.
    let wrong_team = if team == "AAAAAAAAAA" {
        "BBBBBBBBBB"
    } else {
        "AAAAAAAAAA"
    };
    require(
        matches!(
            AppleHelper::new_trusted(helper, HelperTrust::team(wrong_team)?).status(),
            Err(Error::Helper(_))
        ),
        "wrong signing team was not refused",
    )?;
    println!("native bundled helper consumer assertions passed");
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn native_helper() -> Result<()> {
    require(
        false,
        "native-helper requires macOS & signed nested LoginItems helper",
    )
}

#[cfg(all(feature = "db-session", any(windows, target_os = "macos")))]
fn native_db() -> Result<()> {
    use rightkit_credentials::db_adapter::CredentialsKeyStore;
    use rightkit_credentials::{Config, GuardMode, Interaction, Namespace, Store};
    use rightkit_db::encrypted_lifecycle::{
        EncryptedStore, EncryptedStoreConfig, Key, KeyStoreErrorKind, LifecycleError,
        PendingRevision, ProtectedKeyStore, RotationReport, RotationRequest,
    };
    use rightkit_db::DbOptions;
    use std::sync::Arc;

    require(
        std::env::var("RIGHTKIT_TEST_REAL_USER_STORES").as_deref() == Ok("1"),
        "native-db requires RIGHTKIT_TEST_REAL_USER_STORES=1",
    )?;
    let root = PathBuf::from(
        std::env::var_os("RK_CONSUMER_RUN_ROOT")
            .ok_or_else(|| std::io::Error::other("RK_CONSUMER_RUN_ROOT is required"))?,
    );
    let github = std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true");
    let scratch = if github {
        let runner =
            PathBuf::from(std::env::var_os("RUNNER_TEMP").ok_or_else(|| {
                std::io::Error::other("RUNNER_TEMP is required on GitHub Actions")
            })?);
        require(
            runner.is_absolute() && runner.is_dir(),
            "RUNNER_TEMP must be an existing absolute directory",
        )?;
        require(
            root.is_absolute() && root.is_dir(),
            "run root must be an existing absolute directory",
        )?;
        runner
    } else if cfg!(windows) {
        PathBuf::from("D:/scratch")
    } else {
        PathBuf::from("/Volumes/D/scratch")
    };
    let root = root.canonicalize()?;
    let scratch = scratch.canonicalize()?;
    require(
        root.starts_with(&scratch) && (!github || root != scratch),
        "run root must be inside designated scratch directory",
    )?;
    let id = unique()?;
    let run = root.join(format!("credentials-r2-{id}"));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&run)?;
    // Preserve fixture location on failure so an unresolved transition remains recoverable.
    println!(
        "native fixture: {} / namespace rightkit.r2.credentials.{id}",
        run.display()
    );
    let store = Store::open(
        Config::new(Namespace::new(format!("rightkit.r2.credentials.{id}"))?)
            .guard_mode(GuardMode::Deny)
            .interaction(Interaction::NonInteractive)
            .timeout(Duration::from_secs(5))
            .rotation_lock_dir(run.join("rotation-locks")),
    );
    require(
        store.is_durable(),
        "native durable store unavailable; memory fallback forbidden",
    )?;
    let old = Store::generate_key(32)?;
    require(
        store.add_if_absent("db-key", &old)?,
        "unique native slot already exists",
    )?;
    let adapter = Arc::new(CredentialsKeyStore::new(
        store.clone(),
        "db-key",
        "db-key.recovery",
        vec!["SQLCipher/DBKey.staging".into()],
    )?);
    let staged = Store::generate_key(32)?;
    let mut session = adapter.lock_rotation(Duration::from_secs(2))?;
    let revision = session.write_pending(&Key::new(staged.expose_str()?.to_owned()))?;
    match adapter.lock_rotation(Duration::from_millis(20)) {
        Err(error) => require(
            error.kind == KeyStoreErrorKind::Unsettled,
            "wrong lock refusal kind",
        )?,
        Ok(_) => return require(false, "second session acquired held rotation fence"),
    }
    require(
        session
            .discard_pending(&PendingRevision("wrong-revision".into()))
            .is_err(),
        "stale revision was accepted",
    )?;
    require(
        session
            .read_pending()?
            .is_some_and(|p| p.revision == revision),
        "stale discard changed pending key",
    )?;
    // Move owned capability across a real thread, then release its staged key & fence.
    std::thread::spawn(move || -> Result<()> {
        session.discard_pending(&revision)?;
        require(
            session.read_pending()?.is_none(),
            "pending cleanup did not settle",
        )?;
        require(session.read_primary()?.is_some(), "primary key was lost")
    })
    .join()
    .map_err(|_| std::io::Error::other("session worker panicked"))??;

    let db_path = run.join("consumer.sqlite");
    let config = EncryptedStoreConfig::new(DbOptions::new(&db_path), adapter.clone());
    require(
        matches!(
            EncryptedStore::open(config.clone()),
            Err(LifecycleError::StoreMissing { .. })
        ),
        "missing database did not return StoreMissing",
    )?;
    require(
        !db_path.exists(),
        "missing-database refusal created database",
    )?;
    let live = EncryptedStore::open(config.clone().with_create_if_missing(true))?;
    live.with_writer(|connection| connection.execute_batch(
        "CREATE TABLE qualification(value INTEGER NOT NULL); INSERT INTO qualification VALUES(831);",
    ))??;
    let next = Store::generate_key(32)?;
    require(
        matches!(
            live.rotate_key(RotationRequest::new(next.expose_str()?.to_owned())),
            RotationReport::Committed
        ),
        "rotation did not fully commit & settle",
    )?;
    drop(live);
    let reopened = EncryptedStore::open(config)?;
    let value: i64 = reopened.with_writer(|connection| {
        connection.query_row("SELECT value FROM qualification", [], |row| row.get(0))
    })??;
    require(value == 831, "reopen lost committed application data")?;
    drop(reopened);
    let mut settled = adapter.lock_rotation(Duration::from_secs(2))?;
    require(
        settled.read_pending()?.is_none(),
        "pending marker remains after full commit",
    )?;
    require(
        settled.read_recovery()?.is_none(),
        "recovery remains after full commit",
    )?;
    drop(settled);
    // Success only: delete this run's synthetic credentials. Retain DB/lock evidence in scratch.
    for account in [
        "db-key",
        "db-key.rotation-pending",
        "db-key.recovery",
        "SQLCipher/DBKey.staging",
    ] {
        store.delete(account)?;
    }
    println!("native owned session / SQLCipher rotate / reopen consumer assertions passed");
    Ok(())
}

#[cfg(not(all(feature = "db-session", any(windows, target_os = "macos"))))]
fn native_db() -> Result<()> {
    require(
        false,
        "native-db requires Windows/macOS & exactly one SQLCipher provider feature",
    )
}

fn main() -> Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("core") => core(),
        Some("native-helper") => {
            core()?;
            native_helper()
        }
        Some("native-db") => {
            core()?;
            native_db()
        }
        _ => require(false, "usage: consumer core|native-helper|native-db"),
    }
}
