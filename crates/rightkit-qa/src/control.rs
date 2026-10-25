//! Drive the real Tauri app through the in-app control server (`embedded`, feature `control`).
//!
//! The app is launched hidden or backgrounded with `RIGHTKIT_CONTROL_SERVICE` and an
//! isolated `RIGHTKIT_SUITE_ROOT`, discovers the `rightkit-daemon` record
//! (`service.json`), and talks over the current-user-only socket / named pipe as the
//! allowlisted app `right-qa`. No TCP. A launch that names a per-run control token file
//! ([`token::ENV_AUTH_FILE`], ADR-0001 D-e) attaches the token to every order and the app refuses
//! orders without it; the environment carries only the file's path. The harness never searches for
//! processes by name to kill them: it records the pid it started and kills that
//! tree only.
use crate::process::{is_alive, kill_tree, Tracker};
use crate::util::{err, new_id, sleep_ms, Error, Result};
use crate::workspace::QaWorkspace;
use rightkit_child_process::OwnedChild;
#[cfg(not(target_os = "macos"))]
use rightkit_child_process::OwnedCommand;
use rightkit_daemon::{Client, ServiceError, Suite};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(not(target_os = "macos"))]
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Window never shown (`open -g -j -n` on macOS, hidden window on Windows).
    Hidden,
    /// Window shown but the app is not activated and frontmost is unchanged.
    Background,
    Visible,
}

impl Mode {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "hidden" => Ok(Mode::Hidden),
            "background" => Ok(Mode::Background),
            "visible" => Ok(Mode::Visible),
            other => err(format!(
                "unknown ui mode '{other}' (hidden|background|visible)"
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LaunchSpec {
    /// A `.app` bundle, or a plain executable (wrapped into a bundle at a fixed per-app path on macOS, replaced in place with `.previous` kept).
    pub binary: PathBuf,
    pub mode: Mode,
    pub env: Vec<(String, String)>,
    pub startup_timeout: Duration,
    pub label: String,
    /// Opt-in (default `false`): after the page is ready, make the window key and focused so
    /// `document.hasFocus()`, `:focus` and `:focus-visible` match (keyboard and accessibility
    /// scenarios). macOS activates the app and makes the window key (it may change the
    /// frontmost app); Windows enables Chromium focus emulation and moves WebView2 focus into
    /// the page, with no `SetForegroundWindow`. A focused launch cannot prove
    /// `owned_never_frontmost`, so `ui_stop` skips that proof for it.
    pub focus: bool,
}

/// Env var through which the launcher names the window the in-app control server drives: the
/// config's `[ui] window_label` (`ui_launch` sets it). It is the Tauri window label, never
/// [`LaunchSpec::label`], which only names the process in the tracker.
pub const WINDOW_LABEL_ENV: &str = "RIGHTKIT_QA_WINDOW_LABEL";

/// The window label the in-app control server drives. The app's own `Control::window(..)` wins
/// (`explicit`), then the launcher's `[ui] window_label` carried in [`WINDOW_LABEL_ENV`]
/// (`from_env`), then `"main"`. A blank value counts as absent, and the winner is trimmed.
///
/// This is pure and always compiled: `embedded` exists only in a `control` build (which needs a
/// desktop webview, so Linux CI cannot build it), so the precedence lives and is tested here.
pub fn resolve_window_label(explicit: Option<&str>, from_env: Option<&str>) -> String {
    [explicit, from_env]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|label| !label.is_empty())
        .unwrap_or("main")
        .to_string()
}

#[derive(Debug, Clone)]
pub struct StopReport {
    pub pid: u32,
    pub endpoint_closed: bool,
    pub process_gone: bool,
    pub frontmost_before: Option<String>,
    pub frontmost_after: Option<String>,
    pub frontmost_unchanged: bool,
    /// Every distinct foreground state sampled during the session (empty when unmeasurable).
    pub frontmost_states: Vec<String>,
    /// Every sampled foreground PID belonged to no app process started by this control session.
    /// This is the background invariant; `frontmost_unchanged` remains diagnostic only.
    /// macOS checks the started pid and every process under the bundle executable; Windows
    /// checks the started pid only (the owner of the foreground window, from
    /// foreground notifications + `GetWindowThreadProcessId`). Both fail
    /// closed when a foreground owner exists but cannot be read. Other targets always report
    /// `false` for a non-visible launch.
    pub owned_never_frontmost: bool,
    /// Owned fixture PIDs observed in foreground, if any.
    pub owned_frontmost_pids: Vec<u32>,
    /// Foreground samples whose owner pid could not be read (macOS: the frontmost application;
    /// Windows: the foreground window's owner). Any such sample makes `owned_never_frontmost`
    /// false: an unread owner proves nothing.
    pub frontmost_pid_unavailable_samples: usize,
    /// Why `owned_never_frontmost` is false, in words that tell "an owned app WAS frontmost"
    /// from "the frontmost pid could not be read" and from "the owned executable was never
    /// registered". `None` when the guard holds or was not enforced.
    pub owned_never_frontmost_failure: Option<String>,
}

impl StopReport {
    /// The first sampled foreground name that belongs to a system permission-dialog agent.
    /// Positive evidence only: `None` does not prove no dialog appeared.
    pub fn permission_dialog(&self) -> Option<String> {
        crate::userdata::permission_dialog_in(&self.frontmost_states)
    }
}

pub struct Control {
    pub pid: u32,
    /// `rightkit-daemon` name the app serves (`control-xxxxxxxx`).
    pub service: String,
    pub log_path: PathBuf,
    raw_log: PathBuf,
    suite: Suite,
    endpoint: String,
    client: Mutex<Option<Arc<Client>>>,
    child: Option<OwnedChild>,
    tracker: Tracker,
    frontmost_before: Option<String>,
    focus: Option<FocusMonitor>,
    request_timeout: Duration,
    worker: Mutex<Option<Worker>>,
    /// Gap between native input events asked of the app (`paceMs`); 0 sends none.
    pace_ms: u64,
    stopped: bool,
    /// The launch asked for a key/focused window (`LaunchSpec::focus`).
    focus_requested: bool,
    /// The per-run control token of this launch (read from the file it named in
    /// [`token::ENV_AUTH_FILE`], or minted by [`token::for_launch`]); attached to every order.
    auth: Option<String>,
    /// The token file this launch handed the app, the one value of [`token::ENV_AUTH_FILE`] that
    /// reached it (the world's file on the journeys path). A receipt records this, not a guess.
    auth_file: PathBuf,
}

const APP: &str = "right-qa";

/// The per-run control token (ADR-0001 ruling D-e).
///
/// The runner mints a random token, writes it into a user-only file (`World::control_token_file`)
/// and names that file to the app in [`ENV_AUTH_FILE`](token::ENV_AUTH_FILE). The in-app control server (`embedded`)
/// reads the file once at start and refuses every order that does not carry the same token in
/// [`PARAM`](token::PARAM). The environment carries a path, never the token itself, so the credential-key
/// refusal of `check_env` still holds. Threat model: this stops accidental ad-hoc driving of a
/// QA app (a stray `right-ctl`, a second runner), it is not protection against another process of
/// the same user, who can read the file.
///
/// Fail closed (ADR-0001 item 7): a server launched without [`ENV_AUTH_FILE`](token::ENV_AUTH_FILE) refuses every order,
/// since no token can be required of a caller; a server launched with it but unable to use the file
/// does not start. A launch that names no file mints one ([`for_launch`](token::for_launch)), so the app it starts still
/// accepts its own orders.
pub mod token {
    use crate::util::{err, Error, Result};
    use serde_json::{json, Value};
    use std::path::{Path, PathBuf};

    /// Launcher-set variable naming the user-only token file: the same variable `pool::world_env`
    /// puts in every process environment of a scenario world (`pool::ENV_CONTROL_TOKEN_FILE`), so a
    /// launch that passes the world's environment on needs no translation. Its value is a path,
    /// never the token, which is why `check_env` lets this one key through its credential-name
    /// refusal.
    pub const ENV_AUTH_FILE: &str = "RIGHTKIT_QA_CONTROL_TOKEN_FILE";
    /// Request parameter that carries the token on every order.
    pub const PARAM: &str = "rkAuth";
    /// Error code of a refused order.
    pub const REFUSED_CODE: &str = "unauthorized";
    /// Token length in lowercase hex characters (32 random bytes).
    pub const HEX_LEN: usize = 64;

    /// Why an order was refused.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Refusal {
        /// The order carries no token.
        Missing,
        /// The order carries a token that is not this run's.
        Mismatch,
        /// The server names no token file, so it can authorize no order (fail closed).
        Unnamed,
    }

    impl Refusal {
        pub fn message(self) -> &'static str {
            match self {
                Refusal::Missing => {
                    "this control server requires the per-run control token; the order carries none"
                }
                Refusal::Mismatch => "the order's control token is not this run's token",
                Refusal::Unnamed => {
                    "this control server names no control token file (RIGHTKIT_QA_CONTROL_TOKEN_FILE); it refuses every order"
                }
            }
        }
    }

    fn well_formed(t: &str) -> bool {
        t.len() == HEX_LEN
            && t.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    /// A fresh token: 32 bytes from the operating system's random source, as lowercase hex.
    pub fn mint() -> Result<String> {
        let mut b = [0u8; 32];
        getrandom::fill(&mut b)
            .map_err(|e| Error(format!("no operating system random source: {e}")))?;
        Ok(b.iter().map(|x| format!("{x:02x}")).collect())
    }

    /// Mint a token and write it to `path` as an owner-only file (the temp inode is owner-only from
    /// creation, so the token is never visible with a broader mode). Returns the token. The world
    /// directory must already exist.
    pub fn write_file(path: &Path) -> Result<String> {
        let t = mint()?;
        rightkit_fs::private::write_private_file(path, t.as_bytes()).map_err(|e| {
            Error(format!(
                "cannot write the control token file {}: {e}",
                path.display()
            ))
        })?;
        rightkit_fs::private::verify_owner_only(path).map_err(|e| {
            Error(format!(
                "the control token file {} is not owner-only after writing: {e}",
                path.display()
            ))
        })?;
        Ok(t)
    }

    /// Read the token from `path`. Fails closed: a relative path, a path with `.` or `..`, a file
    /// that is not owner-only and owned by this user, or content that is not a 64-hex token.
    pub fn read_file(path: &Path) -> Result<String> {
        if !path.is_absolute()
            || path.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
        {
            return err(format!(
                "the control token file must be an absolute path without . or .. components: {}",
                path.display()
            ));
        }
        rightkit_fs::private::verify_owner_only(path).map_err(|e| {
            Error(format!(
                "the control token file {} is not usable: {e}",
                path.display()
            ))
        })?;
        let text = std::fs::read_to_string(path).map_err(|e| {
            Error(format!(
                "cannot read the control token file {}: {e}",
                path.display()
            ))
        })?;
        let t = text.trim();
        if !well_formed(t) {
            return err(format!(
                "the control token file {} does not hold a {HEX_LEN}-character lowercase hex token",
                path.display()
            ));
        }
        Ok(t.to_string())
    }

    /// Server side: the token this process must require, from the value of [`ENV_AUTH_FILE`].
    /// `Ok(None)` when it is unset or blank (no token is required); an error when it is set and the
    /// file cannot be used, so the server never starts open by accident.
    pub fn load(env_value: Option<&str>) -> Result<Option<String>> {
        match env_value.map(str::trim).filter(|v| !v.is_empty()) {
            None => Ok(None),
            Some(p) => read_file(Path::new(p)).map(Some),
        }
    }

    /// [`load`] for this process's environment.
    pub fn from_env() -> Result<Option<String>> {
        load(std::env::var(ENV_AUTH_FILE).ok().as_deref())
    }

    /// Client side: `params` with the token attached under [`PARAM`]. `None` leaves them alone.
    pub fn with_token(mut params: Value, token: Option<&str>) -> Value {
        let Some(t) = token else { return params };
        if params.is_null() {
            params = json!({});
        }
        if let Value::Object(m) = &mut params {
            m.insert(PARAM.into(), Value::String(t.to_string()));
        }
        params
    }

    fn eq_constant_time(a: &[u8], b: &[u8]) -> bool {
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
    }

    /// The token file a launch names (N2): the workspace's value first (the world's file on the
    /// journeys path, where `launch_env` copies the workspace environment), then the caller's
    /// `spec_env`. `None` means the launch mints one. Only the path is compared, never read here.
    pub fn named_for_launch(
        ws_env: &std::collections::BTreeMap<String, String>,
        spec_env: &[(String, String)],
    ) -> Option<PathBuf> {
        // A named value is never blanked here: an empty name reaches `read_file` and fails closed.
        ws_env.get(ENV_AUTH_FILE).map(PathBuf::from).or_else(|| {
            spec_env
                .iter()
                .rev()
                .find(|(k, _)| k == ENV_AUTH_FILE)
                .map(|(_, v)| PathBuf::from(v))
        })
    }

    /// Leave exactly one [`ENV_AUTH_FILE`] entry in `env`, naming `path`.
    pub fn set_one(env: &mut Vec<(String, String)>, path: &Path) {
        env.retain(|(k, _)| k != ENV_AUTH_FILE);
        env.push((
            ENV_AUTH_FILE.to_string(),
            path.to_string_lossy().into_owned(),
        ));
    }

    /// Client side, one launch: the token it holds and the file it names to the app (ADR-0001 D-e and
    /// item 7). A launch that names a token file (`named`) reads it, failing closed on an unusable
    /// file. A launch that names none (the legacy, non-journeys path) mints a token into `run_root` as
    /// `control-<stem>.token` and names that file itself. Either way the launch holds a token.
    pub fn for_launch(
        named: Option<&Path>,
        run_root: &Path,
        stem: &str,
    ) -> Result<(String, PathBuf)> {
        if let Some(path) = named {
            return Ok((read_file(path)?, path.to_path_buf()));
        }
        if !run_root.is_absolute() {
            return err(format!(
                "the run root must be an absolute path to hold a control token: {}",
                run_root.display()
            ));
        }
        let path = run_root.join(format!("control-{stem}.token"));
        let t = write_file(&path)?;
        Ok((t, path))
    }

    /// Server side: does this order carry the token the server requires? `expected` `None` means no
    /// token file was named, and every order is refused (fail closed, ADR-0001 item 7). The
    /// comparison does not stop at the first differing byte.
    pub fn authorize(expected: Option<&str>, params: &Value) -> std::result::Result<(), Refusal> {
        let Some(want) = expected else {
            return Err(Refusal::Unnamed);
        };
        match params.get(PARAM).and_then(Value::as_str) {
            None => Err(Refusal::Missing),
            Some(got) if eq_constant_time(got.as_bytes(), want.as_bytes()) => Ok(()),
            Some(_) => Err(Refusal::Mismatch),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// `{ PARAM: v }`.
        fn keyed(v: Value) -> Value {
            let mut m = serde_json::Map::new();
            m.insert(PARAM.into(), v);
            Value::Object(m)
        }

        fn dir(tag: &str) -> std::path::PathBuf {
            let d =
                std::env::temp_dir().join(format!("rkqa-token-{tag}-{}", crate::util::new_id()));
            std::fs::create_dir_all(&d).unwrap();
            d
        }

        #[test]
        fn minted_tokens_are_64_lowercase_hex_and_differ() {
            let (a, b) = (mint().unwrap(), mint().unwrap());
            assert!(well_formed(&a) && well_formed(&b));
            assert_ne!(a, b);
        }

        #[test]
        fn a_written_token_file_reads_back_and_is_owner_only() {
            let d = dir("rw");
            let f = d.join("control.token");
            let t = write_file(&f).unwrap();
            assert_eq!(read_file(&f).unwrap(), t);
            assert!(rightkit_fs::private::is_owner_only(&f).unwrap());
            // load() takes the file named by the environment value
            assert_eq!(load(Some(f.to_str().unwrap())).unwrap(), Some(t));
            let _ = std::fs::remove_dir_all(&d);
        }

        #[test]
        fn an_unset_or_blank_variable_requires_no_token_but_a_bad_file_is_an_error() {
            assert_eq!(load(None).unwrap(), None);
            assert_eq!(load(Some("")).unwrap(), None);
            assert_eq!(load(Some("   ")).unwrap(), None);
            let d = dir("bad");
            // missing file, relative path, `..` path, short content: the server must not start open
            assert!(load(Some(d.join("absent.token").to_str().unwrap())).is_err());
            assert!(load(Some("control.token")).is_err());
            let dotted = d.join("sub").join("..").join("x.token");
            assert!(load(Some(dotted.to_str().unwrap())).is_err());
            let short = d.join("short.token");
            rightkit_fs::private::write_private_file(&short, b"abc123").unwrap();
            assert!(read_file(&short).is_err());
            let upper = d.join("upper.token");
            rightkit_fs::private::write_private_file(&upper, "A".repeat(HEX_LEN).as_bytes())
                .unwrap();
            assert!(
                read_file(&upper).is_err(),
                "uppercase is not the minted form"
            );
            let _ = std::fs::remove_dir_all(&d);
        }

        #[cfg(unix)]
        #[test]
        fn a_world_readable_token_file_is_refused() {
            use std::os::unix::fs::PermissionsExt;
            let d = dir("mode");
            let f = d.join("control.token");
            write_file(&f).unwrap();
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(read_file(&f).is_err());
            let _ = std::fs::remove_dir_all(&d);
        }

        #[test]
        fn a_token_less_order_is_refused_and_a_wrong_one_too() {
            let want = "a".repeat(HEX_LEN);
            assert_eq!(
                authorize(Some(&want), &json!({"x": 1})),
                Err(Refusal::Missing)
            );
            assert_eq!(authorize(Some(&want), &json!({})), Err(Refusal::Missing));
            assert_eq!(authorize(Some(&want), &Value::Null), Err(Refusal::Missing));
            assert_eq!(
                authorize(Some(&want), &keyed(json!(7))),
                Err(Refusal::Missing),
                "a non-string token is no token"
            );
            assert_eq!(
                authorize(Some(&want), &keyed(json!("b".repeat(HEX_LEN)))),
                Err(Refusal::Mismatch)
            );
            assert_eq!(
                authorize(Some(&want), &keyed(json!("a"))),
                Err(Refusal::Mismatch),
                "a prefix is not the token"
            );
            assert_eq!(authorize(Some(&want), &keyed(json!(want.clone()))), Ok(()));
        }

        #[test]
        fn no_token_file_named_refuses_every_order_even_one_that_carries_a_token() {
            // ADR-0001 item 7: a server with no token file (`None`) refuses everything, fail closed.
            let t = "a".repeat(HEX_LEN);
            for params in [json!({}), json!({"x": 1}), Value::Null] {
                assert_eq!(authorize(None, &params), Err(Refusal::Unnamed));
            }
            assert_eq!(
                authorize(None, &keyed(json!(t.clone()))),
                Err(Refusal::Unnamed),
                "a token cannot be checked against nothing"
            );
            // and the client refuses to send an order from a launch holding no token
            let e = crate::control::require_order_token(None, "click").unwrap_err();
            assert!(e.0.contains("click") && e.0.contains("refused"), "{e}");
            assert!(crate::control::require_order_token(Some(&t), "click").is_ok());
        }

        #[test]
        fn a_legacy_launch_mints_a_token_into_the_run_root_and_the_app_accepts_it() {
            // No token file named: the launch mints one, writes it owner-only, and the server it
            // starts reads that same file and accepts the launch's own orders.
            let d = dir("legacy");
            let (t, path) = for_launch(None, &d, "abcd1234").unwrap();
            assert_eq!(path, d.join("control-abcd1234.token"));
            assert!(well_formed(&t));
            assert!(rightkit_fs::private::is_owner_only(&path).unwrap());
            // the app reads the file the launch exported (the env value is the path)
            let served = load(Some(path.to_str().unwrap())).unwrap();
            assert_eq!(served.as_deref(), Some(t.as_str()));
            let order = with_token(json!({"x": 1}), Some(&t));
            assert_eq!(authorize(served.as_deref(), &order), Ok(()));
            // and a token-less order from the same launch is refused
            assert_eq!(
                authorize(served.as_deref(), &json!({"x": 1})),
                Err(Refusal::Missing)
            );
            let _ = std::fs::remove_dir_all(&d);
        }

        #[test]
        fn a_named_token_file_is_read_and_a_relative_run_root_is_refused() {
            let d = dir("named");
            let f = d.join("world-control.token");
            let written = write_file(&f).unwrap();
            let (t, path) = for_launch(Some(&f), &d, "ffffffff").unwrap();
            assert_eq!(t, written);
            assert_eq!(path, f);
            // a named file that is missing refuses the launch (no fallback to a minted token)
            assert!(for_launch(Some(&d.join("absent.token")), &d, "ffffffff").is_err());
            // a legacy launch needs an absolute run root to name the file to the app
            assert!(for_launch(None, Path::new("relative-root"), "ffffffff").is_err());
            let _ = std::fs::remove_dir_all(&d);
        }

        #[test]
        fn the_workspace_names_the_world_token_file_before_the_caller_and_a_mint_only_when_neither_does(
        ) {
            // N2: the journeys launch copies the world's file into the workspace environment; that
            // file wins over a caller's value, and nothing is minted while one of them names a file.
            let world = PathBuf::from(if cfg!(windows) {
                r"C:\w\control.token"
            } else {
                "/w/control.token"
            });
            let other = PathBuf::from(if cfg!(windows) {
                r"C:\o\other.token"
            } else {
                "/o/other.token"
            });
            let mut ws_env = std::collections::BTreeMap::new();
            ws_env.insert(
                ENV_AUTH_FILE.to_string(),
                world.to_string_lossy().into_owned(),
            );
            let spec = vec![(
                ENV_AUTH_FILE.to_string(),
                other.to_string_lossy().into_owned(),
            )];
            assert_eq!(named_for_launch(&ws_env, &spec), Some(world.clone()));
            assert_eq!(named_for_launch(&Default::default(), &spec), Some(other));
            assert_eq!(named_for_launch(&Default::default(), &[]), None);
        }

        #[test]
        fn the_app_is_handed_exactly_one_token_file_value() {
            // N2: launch_env copies the workspace value; the launch replaces it, never appends.
            let mut env = vec![
                ("HOME".to_string(), "/h".to_string()),
                (ENV_AUTH_FILE.to_string(), "/w/control.token".to_string()),
            ];
            let minted = PathBuf::from("/r/control-abcd1234.token");
            set_one(&mut env, &minted);
            let values: Vec<&String> = env
                .iter()
                .filter(|(k, _)| k == ENV_AUTH_FILE)
                .map(|(_, v)| v)
                .collect();
            assert_eq!(values.len(), 1);
            assert_eq!(values[0], &minted.to_string_lossy().into_owned());
            assert!(env.iter().any(|(k, v)| k == "HOME" && v == "/h"));
        }

        #[test]
        fn a_wrong_token_is_refused_by_the_minted_launch_token() {
            let d = dir("wrong");
            let (t, _) = for_launch(None, &d, "00000000").unwrap();
            let other = "b".repeat(HEX_LEN);
            assert_ne!(other, t);
            let wrong = with_token(json!({}), Some(&other));
            assert_eq!(authorize(Some(&t), &wrong), Err(Refusal::Mismatch));
            let _ = std::fs::remove_dir_all(&d);
        }

        #[test]
        fn the_client_attaches_the_token_to_every_order() {
            let t = "c".repeat(HEX_LEN);
            let p = with_token(json!({"selector": "#x"}), Some(&t));
            assert_eq!(p[PARAM], json!(t));
            assert_eq!(p["selector"], json!("#x"));
            let p = with_token(Value::Null, Some(&t));
            assert_eq!(p[PARAM], json!(t));
            let stale = keyed(json!("d".repeat(HEX_LEN)));
            assert_eq!(
                with_token(stale, Some(&t))[PARAM],
                json!(t),
                "the session's token wins"
            );
            assert_eq!(with_token(json!({"k": 1}), None), json!({"k": 1}));
            assert_eq!(
                authorize(Some(&t), &with_token(json!({}), Some(&t))),
                Ok(())
            );
        }
    }
}

fn svc_err(e: ServiceError) -> Error {
    Error(e.to_string())
}

enum CallError {
    TimedOut,
    Service(ServiceError),
}

/// Run `f` on a helper thread and give up after `timeout`; the thread is left to finish
/// on its own (it ends when the app is killed or the connection is aborted).
fn bounded<T: Send + 'static>(
    timeout: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(timeout).ok()
}

type Job = Box<dyn FnOnce() + Send>;

/// A reused call thread: requests are handed to it instead of spawning a thread per call. A
/// call that times out abandons it (the abort ends its call, then it exits) and the next call
/// starts a fresh one.
struct Worker {
    jobs: std::sync::mpsc::Sender<Job>,
}

impl Worker {
    fn spawn() -> Self {
        let (jobs, rx) = std::sync::mpsc::channel::<Job>();
        std::thread::spawn(move || {
            for job in rx {
                job();
            }
        });
        Worker { jobs }
    }
}

/// Client side of ADR-0001 item 7: an order is refused, and never sent, from a launch that holds no
/// control token. [`token::for_launch`] gives every launch one, so this is the last check before the
/// wire, not the first.
fn require_order_token(auth: Option<&str>, method: &str) -> Result<()> {
    match auth {
        Some(_) => Ok(()),
        None => err(format!(
            "{method}: refused, this launch holds no control token (RIGHTKIT_QA_CONTROL_TOKEN_FILE was never named)"
        )),
    }
}

fn call_bounded(
    client: &Arc<Client>,
    slot: &Mutex<Option<Worker>>,
    method: &str,
    params: Value,
    timeout: Duration,
) -> std::result::Result<Value, CallError> {
    let (tx, rx) = std::sync::mpsc::channel();
    let (c, m) = (client.clone(), method.to_string());
    let mut job: Option<Job> = Some(Box::new(move || {
        let _ = tx.send(c.call(&m, params));
    }));
    {
        let mut w = slot.lock().unwrap();
        for _ in 0..2 {
            let worker = w.get_or_insert_with(Worker::spawn);
            match worker.jobs.send(job.take().unwrap()) {
                Ok(()) => break,
                Err(e) => {
                    // The thread is gone: take the job back and start another.
                    job = Some(e.0);
                    *w = None;
                }
            }
        }
        if job.is_some() {
            return Err(CallError::TimedOut);
        }
    }
    match rx.recv_timeout(timeout) {
        Ok(r) => r.map_err(CallError::Service),
        Err(_) => {
            client.abort();
            *slot.lock().unwrap() = None;
            Err(CallError::TimedOut)
        }
    }
}

/// The app's driver predates the `wait` method (or does not allow it).
fn wait_unsupported(msg: &str) -> bool {
    msg.contains("unknown method")
        || msg.contains("request_not_allowed")
        || msg.contains("not allowed")
}

fn connect_bounded(suite: &Suite, service: &str, timeout: Duration) -> Result<Client> {
    let (s, sv) = (suite.clone(), service.to_string());
    match bounded(timeout, move || Client::connect(&s, &sv, APP)) {
        Some(r) => r.map_err(svc_err),
        None => err(format!(
            "connecting to {service} did not finish within {}ms",
            timeout.as_millis()
        )),
    }
}

/// One connect + health attempt. `Err` says why it failed, so a launch timeout can report the
/// last reason instead of nothing.
fn bounded_connect_health(
    suite: &Suite,
    service: &str,
    timeout: Duration,
    auth: Option<&str>,
) -> std::result::Result<Arc<Client>, String> {
    let (s, sv) = (suite.clone(), service.to_string());
    let auth = auth.map(str::to_string);
    match bounded(timeout, move || {
        let c = Client::connect(&s, &sv, APP).map_err(|e| format!("connect: {e}"))?;
        c.call("health", token::with_token(json!({}), auth.as_deref()))
            .map_err(|e| format!("health: {e}"))?;
        Ok(Arc::new(c))
    }) {
        Some(r) => r,
        None => Err(format!(
            "connect/health did not finish within {}ms",
            timeout.as_millis()
        )),
    }
}

/// What the client polled against what the server registered, for a launch that timed out: the
/// suite root and service the client used, the record it would read and the endpoint it names,
/// every service directory the app created under that suite, the last connect error, and the
/// tail of the app's own log (stdout and stderr). Read before the app is stopped.
fn launch_timeout_diagnostics(
    suite: &Suite,
    service: &str,
    pid: u32,
    raw_log: &Path,
    last_error: &str,
) -> String {
    let record = rightkit_daemon::record_path(suite, service);
    let polled = match rightkit_daemon::read_record(suite, service) {
        Some(r) => format!(
            "record present, endpoint {} (answers: {})",
            r.endpoint,
            endpoint_open(&r.endpoint)
        ),
        None => format!("no readable record at {}", record.display()),
    };
    let registered = {
        let runtime = suite.root().join("runtime");
        match fs::read_dir(&runtime) {
            Ok(entries) => {
                let mut names: Vec<String> = entries
                    .flatten()
                    .map(|e| {
                        let has = e.path().join("service.json").is_file();
                        format!(
                            "{}{}",
                            e.file_name().to_string_lossy(),
                            if has {
                                " (service.json)"
                            } else {
                                " (no service.json)"
                            }
                        )
                    })
                    .collect();
                names.sort();
                if names.is_empty() {
                    format!("nothing under {}", runtime.display())
                } else {
                    names.join(", ")
                }
            }
            Err(e) => format!("{} unreadable: {e}", runtime.display()),
        }
    };
    let log = fs::read(raw_log)
        .map(|b| crate::util::tail(&String::from_utf8_lossy(&b), 3000))
        .unwrap_or_else(|e| format!("<log unreadable: {e}>"));
    let log = if log.trim().is_empty() {
        "<log is empty: the app printed nothing>".to_string()
    } else {
        log
    };
    format!(
        "; client polled suite root {} service {service} (expected endpoint {}): {polled}; \
         last attempt: {}; services the app registered under that suite: {registered}; \
         app pid {pid} alive: {}; app log {} tail:\n{log}",
        suite.root().display(),
        rightkit_daemon::endpoint_for(suite, service),
        if last_error.is_empty() {
            "none"
        } else {
            last_error
        },
        is_alive(pid),
        raw_log.display()
    )
}

fn endpoint_open(endpoint: &str) -> bool {
    rightkit_daemon::probe_endpoint(endpoint)
}

/// Foreground app as `name` (macOS) or `pid:hwnd` (Windows). `None` means the
/// foreground could NOT be measured; it is never treated as "unchanged".
pub fn frontmost() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        rightkit_automation::macos::background::frontmost_name()
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetForegroundWindow, GetWindowThreadProcessId,
        };
        unsafe {
            let h = GetForegroundWindow();
            if h.is_null() {
                return None;
            }
            let mut pid = 0u32;
            if GetWindowThreadProcessId(h, &mut pid) == 0 || pid == 0 {
                return None;
            }
            Some(format!("{pid}:{:x}", h as usize))
        }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        None
    }
}

/// What the Windows foreground query found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
enum Foreground {
    /// No foreground window at all (Windows reports this transiently, and on a desktop
    /// with no active window). Nothing we own can be in front, so this is a measurement.
    NoWindow,
    /// The process that owns the foreground window.
    Pid(u32),
    /// A foreground window exists but its owner could not be read.
    Unreadable,
}

/// Interpret `GetForegroundWindow` / `GetWindowThreadProcessId` results.
#[cfg_attr(not(windows), allow(dead_code))]
fn classify_foreground(window_present: bool, thread_id: u32, pid: u32) -> Foreground {
    if !window_present {
        Foreground::NoWindow
    } else if thread_id == 0 || pid == 0 {
        Foreground::Unreadable
    } else {
        Foreground::Pid(pid)
    }
}

/// The Windows background invariant: no sample found a process this session started in
/// the foreground, and every foreground owner that existed could be read. Process
/// identity is the started pid only; helper processes (WebView2 renderers) own no
/// top-level window, so they cannot be the foreground window's owner.
#[cfg_attr(not(windows), allow(dead_code))]
fn windows_owned_never_frontmost(
    enforce_owned: bool,
    unreadable_owners: usize,
    observed: &[u32],
) -> bool {
    !enforce_owned || (unreadable_owners == 0 && observed.is_empty())
}

/// Why the background proof failed, most specific first: an owned process WAS in front, the
/// foreground owner could not be read, the owned executable was never registered, or other samples
/// were unmeasurable. It is only called when the proof already failed, so it never relaxes it.
fn owned_failure_reason(
    observed: &[u32],
    unavailable: usize,
    registered: bool,
    unmeasured: usize,
) -> String {
    if !observed.is_empty() {
        format!("an owned app process was frontmost (pid {observed:?})")
    } else if unavailable > 0 {
        format!("the foreground owner's pid could not be read in {unavailable} sample(s); the background proof fails closed")
    } else if !registered {
        "the owned executable was never registered before launch, so startup samples could not be attributed".to_string()
    } else if unmeasured > 0 {
        format!("{unmeasured} foreground sample(s) were unmeasurable (name or owned-process inventory unavailable); the background proof fails closed")
    } else {
        "the background proof did not hold".to_string()
    }
}

/// Observes the foreground for the whole life of a control session (Windows and macOS use native
/// foreground events; other targets sample), so
/// a transient steal (focus leaves and returns) is caught, and an unmeasurable
/// foreground is reported as such instead of comparing equal.
struct FocusMonitor {
    stop: Arc<std::sync::atomic::AtomicBool>,
    seen: Arc<Mutex<(usize, usize, Vec<String>)>>, // (measured, unavailable, distinct states in order)
    owned: Arc<Mutex<(Vec<u32>, Option<String>)>>, // primary PID(s), exact executable prefix
    enforce_owned: bool,
    owned_frontmost: Arc<Mutex<Vec<u32>>>,
    /// Foreground samples whose owning pid could not be read (Windows).
    #[cfg_attr(not(windows), allow(dead_code))]
    unreadable_owners: Arc<std::sync::atomic::AtomicUsize>,
    thread: Option<std::thread::JoinHandle<()>>,
    #[cfg(windows)]
    notifications: Option<windows_focus::Monitor>,
    #[cfg(target_os = "macos")]
    notifications: Option<macos_focus::Monitor>,
    #[cfg(target_os = "macos")]
    foreground_frames: Arc<Mutex<Vec<macos_focus::Frame>>>,
    #[cfg(windows)]
    foreground_pids: Arc<Mutex<Vec<u32>>>,
}

impl FocusMonitor {
    fn start(enforce_owned: bool) -> FocusMonitor {
        let seen: Arc<Mutex<(usize, usize, Vec<String>)>> = Arc::new(Mutex::new((0, 0, vec![])));
        let owned: Arc<Mutex<(Vec<u32>, Option<String>)>> = Arc::new(Mutex::new((vec![], None)));
        let owned_frontmost = Arc::new(Mutex::new(vec![]));
        let unreadable_owners = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        #[cfg(not(any(windows, target_os = "macos")))]
        let record = move |seen: &Mutex<(usize, usize, Vec<String>)>,
                           _owned: &Mutex<(Vec<u32>, Option<String>)>,
                           _owned_frontmost: &Mutex<Vec<u32>>,
                           _unreadable: &std::sync::atomic::AtomicUsize| {
            {
                let mut g = seen.lock().unwrap_or_else(|e| e.into_inner());
                match frontmost() {
                    Some(f) => {
                        g.0 += 1;
                        if g.2.last() != Some(&f) {
                            g.2.push(f);
                        }
                    }
                    None => g.1 += 1,
                }
            }
        };
        #[cfg(not(any(windows, target_os = "macos")))]
        record(&seen, &owned, &owned_frontmost, &unreadable_owners);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        #[cfg(not(any(windows, target_os = "macos")))]
        let (s2, seen2, owned2, observed2, unreadable2) = (
            stop.clone(),
            seen.clone(),
            owned.clone(),
            owned_frontmost.clone(),
            unreadable_owners.clone(),
        );
        #[cfg(not(any(windows, target_os = "macos")))]
        let thread = Some(std::thread::spawn(move || {
            while !s2.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(20));
                record(&seen2, &owned2, &observed2, &unreadable2);
            }
        }));
        #[cfg(any(windows, target_os = "macos"))]
        let thread = None;
        #[cfg(windows)]
        let foreground_pids = Arc::new(Mutex::new(Vec::new()));
        #[cfg(windows)]
        let notifications = {
            let (seen, owned, observed, unreadable, pids) = (
                seen.clone(),
                owned.clone(),
                owned_frontmost.clone(),
                unreadable_owners.clone(),
                foreground_pids.clone(),
            );
            let failed = unreadable.clone();
            windows_focus::Monitor::start(
                move |hwnd| {
                    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
                    let mut pid = 0;
                    let thread_id = if hwnd.is_null() {
                        0
                    } else {
                        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) }
                    };
                    let foreground = classify_foreground(!hwnd.is_null(), thread_id, pid);
                    let mut samples = seen.lock().unwrap_or_else(|e| e.into_inner());
                    match foreground {
                        Foreground::Pid(pid) => {
                            samples.0 += 1;
                            let state = format!("{pid}:{:x}", hwnd as usize);
                            if samples.2.last() != Some(&state) {
                                samples.2.push(state);
                            }
                            // Retain startup events before track_primary can know the child PID.
                            let mut history = pids.lock().unwrap_or_else(|e| e.into_inner());
                            if !history.contains(&pid) {
                                history.push(pid);
                            }
                            drop(history);
                            if enforce_owned
                                && owned
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .0
                                    .contains(&pid)
                            {
                                let mut observed =
                                    observed.lock().unwrap_or_else(|e| e.into_inner());
                                if !observed.contains(&pid) {
                                    observed.push(pid);
                                }
                            }
                        }
                        Foreground::NoWindow => samples.1 += 1,
                        Foreground::Unreadable => {
                            samples.1 += 1;
                            unreadable.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                },
                move || {
                    failed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                },
            )
        };
        #[cfg(target_os = "macos")]
        let foreground_frames = Arc::new(Mutex::new(Vec::new()));
        #[cfg(target_os = "macos")]
        let notifications = {
            let (seen, owned, observed, history, failed) = (
                seen.clone(),
                owned.clone(),
                owned_frontmost.clone(),
                foreground_frames.clone(),
                unreadable_owners.clone(),
            );
            macos_focus::Monitor::start(
                move |frame| {
                    // Insert before reading ownership. Registration replays this same history,
                    // so an event racing track_primary/track_prefix cannot escape attribution.
                    let mut history = history.lock().unwrap_or_else(|e| e.into_inner());
                    history.push(frame.clone());
                    let ownership = owned.lock().unwrap_or_else(|e| e.into_inner());
                    if enforce_owned
                        && macos_focus::owned_frame(&frame, &ownership.0, ownership.1.as_deref())
                    {
                        if let Some(pid) = frame.pid {
                            let mut observed = observed.lock().unwrap_or_else(|e| e.into_inner());
                            if !observed.contains(&pid) {
                                observed.push(pid);
                            }
                        }
                    }
                    drop(ownership);
                    drop(history);
                    // Keep public name-only states compatible with permission-dialog matching.
                    let mut samples = seen.lock().unwrap_or_else(|e| e.into_inner());
                    match frame.name {
                        Some(name) => {
                            samples.0 += 1;
                            if samples.2.last() != Some(&name) {
                                samples.2.push(name);
                            }
                        }
                        None => samples.1 += 1,
                    }
                },
                move || {
                    failed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                },
            )
        };
        FocusMonitor {
            stop,
            seen,
            owned,
            enforce_owned,
            owned_frontmost,
            unreadable_owners,
            thread,
            #[cfg(any(windows, target_os = "macos"))]
            notifications,
            #[cfg(target_os = "macos")]
            foreground_frames,
            #[cfg(windows)]
            foreground_pids,
        }
    }
    fn track_primary(&self, pid: u32) {
        let mut owned = self.owned.lock().unwrap_or_else(|e| e.into_inner());
        if !owned.0.contains(&pid) {
            owned.0.push(pid);
        }
        drop(owned);
        #[cfg(target_os = "macos")]
        self.reconcile_macos_history();
        #[cfg(windows)]
        if self.enforce_owned
            && self
                .foreground_pids
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&pid)
        {
            let mut observed = self
                .owned_frontmost
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !observed.contains(&pid) {
                observed.push(pid);
            }
        }
    }
    #[cfg(target_os = "macos")]
    fn track_prefix(&self, prefix: String) {
        self.owned.lock().unwrap_or_else(|e| e.into_inner()).1 = Some(prefix);
        self.reconcile_macos_history();
    }
    #[cfg(target_os = "macos")]
    fn reconcile_macos_history(&self) {
        if !self.enforce_owned {
            return;
        }
        let history = self
            .foreground_frames
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let owned = self.owned.lock().unwrap_or_else(|e| e.into_inner());
        let mut observed = self
            .owned_frontmost
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for frame in history.iter() {
            if macos_focus::owned_frame(frame, &owned.0, owned.1.as_deref()) {
                if let Some(pid) = frame.pid {
                    if !observed.contains(&pid) {
                        observed.push(pid);
                    }
                }
            }
        }
    }
    /// `(unchanged, states, owned_never_frontmost, owned_frontmost_pids, pid_unavailable_samples,
    /// failure reason)`.
    fn finish(&mut self) -> (bool, Vec<String>, bool, Vec<u32>, usize, Option<String>) {
        self.shutdown();
        let g = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let unchanged = g.0 > 0
            && g.1 == 0
            && g.2.len() == 1
            && self
                .unreadable_owners
                .load(std::sync::atomic::Ordering::SeqCst)
                == 0;
        #[cfg(target_os = "macos")]
        let unchanged = unchanged && {
            // Equal names do not prove equal applications. Retain PID + executable identity
            // privately while public frontmost_states remains compatible with its callers.
            let frames = self
                .foreground_frames
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            frames.first().is_some_and(|first| {
                frames
                    .iter()
                    .all(|frame| frame.pid == first.pid && frame.executable == first.executable)
            })
        };
        let observed = self
            .owned_frontmost
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        #[cfg(target_os = "macos")]
        // Unverified launch interval: if the executable identity was never registered
        // before launch, startup samples could not be attributed, so never claim success.
        let identity_registered = self
            .owned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .is_some();
        #[cfg(target_os = "macos")]
        let owned_never_frontmost = !self.enforce_owned
            || (identity_registered
                && g.0 > 0
                && g.1 == 0
                && self
                    .unreadable_owners
                    .load(std::sync::atomic::Ordering::SeqCst)
                    == 0
                && observed.is_empty());
        #[cfg(windows)]
        let owned_never_frontmost = windows_owned_never_frontmost(
            self.enforce_owned,
            self.unreadable_owners
                .load(std::sync::atomic::Ordering::SeqCst),
            &observed,
        );
        #[cfg(not(any(target_os = "macos", windows)))]
        // No native foreground-PID sampler exists on this target; background claims fail
        // closed instead of treating name-only sampling as proof.
        let owned_never_frontmost = !self.enforce_owned;
        let unavailable = self
            .unreadable_owners
            .load(std::sync::atomic::Ordering::SeqCst);
        #[cfg(target_os = "macos")]
        let registered = identity_registered;
        #[cfg(not(target_os = "macos"))]
        let registered = true;
        let failure = (!owned_never_frontmost)
            .then(|| owned_failure_reason(&observed, unavailable, registered, g.1));
        (
            unchanged,
            g.2.clone(),
            owned_never_frontmost,
            observed,
            unavailable,
            failure,
        )
    }
    fn shutdown(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        #[cfg(any(windows, target_os = "macos"))]
        drop(self.notifications.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for FocusMonitor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(target_os = "macos")]
mod macos_focus;
#[cfg(windows)]
mod windows_focus;

/// Words that make an environment key name a credential (matched anywhere in the key, ignoring
/// case). The one list for every check: the launch environment ([`env_key_allowed`]) and the keys
/// a config declares in `[ui] env_keys` (`config`).
const CREDENTIAL_WORDS: [&str; 8] = [
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "API_KEY",
    "APIKEY",
    "PRIVATE_KEY",
    "CREDENTIAL",
];

/// True when `key` contains a credential word (`CREDENTIAL_WORDS`), ignoring case.
pub fn key_names_credential(key: &str) -> bool {
    let up = key.to_ascii_uppercase();
    CREDENTIAL_WORDS.iter().any(|w| up.contains(w))
}

/// Judge one caller environment key (pure, so every platform's rules can be tested anywhere).
/// A key naming a credential is refused, declared or not; the one exception is
/// [`token::ENV_AUTH_FILE`], whose value is the path of the per-run control token file (D-e),
/// never the token. With `macos_rules`, `open --env K=V` shows the value in argv, so a key must
/// start with `RIGHTKIT_` (any case) or be one of the `declared` keys (exact match), the app's own
/// non-secret keys that its config lists in `[ui] env_keys`.
pub fn env_key_allowed(key: &str, declared: &[String], macos_rules: bool) -> Result<()> {
    if key == token::ENV_AUTH_FILE {
        return Ok(());
    }
    if key_names_credential(key) {
        return err(format!("environment key {key} names a credential; secrets are never passed to a launched app through its command line or environment"));
    }
    if macos_rules
        && !key.to_ascii_uppercase().starts_with("RIGHTKIT_")
        && !declared.iter().any(|d| d == key)
    {
        return err(format!("environment key {key} would be exposed in `open` arguments; macOS launches carry only RIGHTKIT_* keys and the keys declared in [ui] env_keys"));
    }
    Ok(())
}

/// Caller environment must not put secrets in a process listing: on macOS
/// `open --env K=V` exposes values in argv. Callers may supply only `RIGHTKIT_*` keys and the
/// `declared` app keys (`[ui] env_keys`, see [`launch_declared`]); no key may name a credential;
/// harness-owned HOME/temp/profile paths are added separately.
fn check_env(env: &[(String, String)], declared: &[String]) -> Result<()> {
    for (k, _) in env {
        env_key_allowed(k, declared, cfg!(target_os = "macos"))?;
    }
    Ok(())
}

/// Absolute path of the executable that `open` will actually run for `bundle`, read from
/// `Contents/Info.plist` `CFBundleExecutable`. `None` (fail closed) when the key is absent,
/// names a path component trick, or the file is not a regular file inside `Contents/MacOS`.
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub fn resolve_bundle_executable(bundle: &Path) -> Option<String> {
    let plist = bundle.join("Contents").join("Info.plist");
    let out = crate::process::output_bounded(
        Command::new("plutil")
            .args(["-extract", "CFBundleExecutable", "raw", "-o", "-"])
            .arg(&plist),
        Duration::from_secs(30),
    )
    .ok()??;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8(out.stdout).ok()?;
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        return None;
    }
    let exe = bundle.join("Contents").join("MacOS").join(name);
    let meta = fs::metadata(&exe).ok()?;
    if !meta.is_file() {
        return None;
    }
    Some(exe.to_string_lossy().into_owned())
}

/// Fixed bundle id suffix for a raw QA binary: one id per app, the same on every run
/// (`app.rightkit.qa.<name>`). macOS keys privacy grants to the bundle id and path, so neither may change
/// per run or per build.
#[cfg(target_os = "macos")]
fn qa_bundle_suffix(exe: &str) -> String {
    exe.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// The fixed per-app folder QA bundles are staged in: `<home base>/rightkit-qa-apps/<app>/`, outside the
/// per-run home (which is deleted with the run) and outside the swept `rightkit-qa` homes. The same path
/// on every run; bundles are replaced in place there and keep `<name>.app.previous`.
#[cfg(target_os = "macos")]
fn fixed_bundle_dir(name: &str) -> Result<PathBuf> {
    let dir = crate::storage::home_base()
        .join("rightkit-qa-apps")
        .join(qa_bundle_suffix(name));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Build the bundle for a raw binary at its fixed path `<dir>/<exe>.app` with the fixed id. Written to a
/// staging bundle first (nothing runs from it) and renamed in place, keeping `<exe>.app.previous`; a build
/// whose executable and Info.plist already match is reused untouched.
#[cfg(target_os = "macos")]
fn make_bundle(dir: &Path, binary: &Path, id: &str, background: bool) -> Result<PathBuf> {
    let exe = binary
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error("binary has no file name".into()))?;
    let app = dir.join(format!("{exe}.app"));
    let abs = fs::canonicalize(binary)
        .map_err(|e| Error(format!("app binary not found: {}: {e}", binary.display())))?;
    // Background fixtures must be non-activating before AppKit creates any window.
    let ui_element = if background {
        "<key>LSUIElement</key><true/>\n"
    } else {
        ""
    };
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>CFBundleExecutable</key><string>{exe}</string>\n<key>CFBundleIdentifier</key><string>app.rightkit.qa.{id}</string>\n<key>CFBundleName</key><string>{exe}</string>\n<key>CFBundlePackageType</key><string>APPL</string>\n<key>CFBundleVersion</key><string>1</string>\n{ui_element}</dict></plist>\n"
    );
    let installed_exe = app.join("Contents").join("MacOS").join(exe);
    let installed_plist = app.join("Contents").join("Info.plist");
    if crate::inplace::same_bytes(&abs, &installed_exe)
        && fs::read_to_string(&installed_plist).is_ok_and(|p| p == plist)
    {
        return Ok(app);
    }
    let staging = crate::inplace::staging_path(&app);
    let _ = fs::remove_dir_all(&staging);
    let macos = staging.join("Contents").join("MacOS");
    fs::create_dir_all(&macos)?;
    // Copy into internal storage; never launch a link back to the build volume.
    fs::copy(&abs, macos.join(exe))?;
    fs::write(staging.join("Contents").join("Info.plist"), plist)?;
    crate::inplace::swap_in_place(&staging, &app)?;
    Ok(app)
}

/// Stage the app bundle at its FIXED per-app path (never a per-run folder, never a per-run bundle id) and
/// return it. Replaced in place with `<name>.app.previous` kept; nothing is ever launched from `.previous`.
#[cfg(target_os = "macos")]
fn stage_bundle(spec: &LaunchSpec, _ws: &QaWorkspace, _id: &str) -> Result<PathBuf> {
    if spec.binary.extension().is_some_and(|e| e == "app") {
        let source = fs::canonicalize(&spec.binary)?;
        let file_name = source
            .file_name()
            .ok_or_else(|| Error("bundle has no file name".into()))?;
        let dir = fixed_bundle_dir(file_name.to_string_lossy().trim_end_matches(".app"))?;
        let bundle = dir.join(file_name);
        let staging = crate::inplace::staging_path(&bundle);
        let _ = fs::remove_dir_all(&staging);
        // Preserve bundle resources, framework links, permissions & signatures.
        let status = crate::process::status_bounded(
            Command::new("/usr/bin/ditto").arg(&source).arg(&staging),
            Duration::from_secs(300),
        )?
        .ok_or_else(|| Error("app bundle staging (ditto) did not finish within 300s".into()))?;
        if !status.success() {
            let _ = fs::remove_dir_all(&staging);
            return err(format!("app bundle staging failed: {status}"));
        }
        if let Err(e) = check_bundle_links(&staging, &staging) {
            let _ = fs::remove_dir_all(&staging);
            return Err(e);
        }
        crate::inplace::swap_in_place(&staging, &bundle)?;
        Ok(bundle)
    } else {
        let exe = spec
            .binary
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error("binary has no file name".into()))?;
        let suffix = qa_bundle_suffix(exe);
        let dir = fixed_bundle_dir(exe)?;
        make_bundle(&dir, &spec.binary, &suffix, spec.mode != Mode::Visible)
    }
}

#[cfg(target_os = "macos")]
fn check_bundle_links(bundle: &Path, dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            if !fs::canonicalize(&path)?.starts_with(bundle) {
                return err(format!(
                    "staged app bundle link escapes internal bundle: {}",
                    path.display()
                ));
            }
        } else if kind.is_dir() {
            check_bundle_links(bundle, &path)?;
        }
    }
    Ok(())
}

fn launch_env(
    spec: &LaunchSpec,
    ws: &QaWorkspace,
    suite: &Suite,
    service: &str,
    declared_env_keys: &[String],
) -> Result<Vec<(String, String)>> {
    // Caller values retain the credential/argv restrictions. Workspace values are
    // harness-owned paths, so HOME, TMPDIR & profile keys can safely travel via open.
    check_env(&spec.env, declared_env_keys)?;
    let mut env = ws.env.clone();
    env.insert("RIGHTKIT_CONTROL_SERVICE".into(), service.into());
    env.insert(
        "RIGHTKIT_SUITE_ROOT".into(),
        suite.root().to_string_lossy().into(),
    );
    env.insert(
        "RIGHTKIT_QA_HIDDEN".into(),
        if spec.mode == Mode::Hidden { "1" } else { "0" }.into(),
    );
    env.insert(
        "RIGHTKIT_QA_FOCUS".into(),
        if spec.focus { "1" } else { "0" }.into(),
    );
    env.insert(
        "RIGHTKIT_QA_BACKGROUND".into(),
        if spec.mode != Mode::Visible { "1" } else { "0" }.into(),
    );
    for (key, value) in &spec.env {
        // The token file lives in the world directory, not the app home; `launch` validates and
        // adds it after the guard has judged the rest of the environment.
        if key == token::ENV_AUTH_FILE {
            continue;
        }
        let path = Path::new(value);
        if path.is_absolute()
            && (!path.starts_with(&ws.home)
                || path
                    .components()
                    .any(|c| c == std::path::Component::ParentDir))
        {
            return err(format!(
                "environment path {key} must be staged inside the QA app home"
            ));
        }
        if let Some(owned) = env.get(key) {
            let app_path = ws.env.contains_key(key)
                && key != "RIGHTKIT_SUITE_ROOT"
                && Path::new(owned).is_absolute();
            let internal = Path::new(value).starts_with(&ws.home)
                && !Path::new(value)
                    .components()
                    .any(|c| c == std::path::Component::ParentDir);
            if value != owned && !(app_path && internal) {
                return err(format!(
                    "environment key {key} overrides a harness-owned launch value"
                ));
            }
        }
        env.insert(key.clone(), value.clone());
    }
    Ok(env.into_iter().collect())
}

#[cfg(target_os = "macos")]
fn open_command(
    spec: &LaunchSpec,
    ws: &QaWorkspace,
    env: &[(String, String)],
    log_path: &Path,
    bundle: &Path,
) -> Command {
    let mut cmd = Command::new("open");
    cmd.current_dir(&ws.home);
    match spec.mode {
        Mode::Hidden => cmd.args(["-g", "-j", "-n"]),
        Mode::Background => cmd.args(["-g", "-n"]),
        Mode::Visible => cmd.args(["-n"]),
    };
    for (k, v) in env {
        cmd.env(k, v).arg("--env").arg(format!("{k}={v}"));
    }
    cmd.arg("--stdout")
        .arg(log_path)
        .arg("--stderr")
        .arg(log_path)
        .arg(bundle);
    cmd
}

#[cfg(target_os = "macos")]
/// Pids whose command line STARTS with `prefix` (so `open`, which merely carries the
/// bundle path as an argument, is never mistaken for the app).
fn pids_with_command_prefix(prefix: &str) -> std::result::Result<Vec<u32>, String> {
    let out = crate::process::output_bounded(
        Command::new("ps").args(["-ax", "-o", "pid=,command="]),
        Duration::from_secs(30),
    )
    .map_err(|e| format!("ps PID inventory failed: {e}"))?
    .ok_or_else(|| "ps PID inventory did not finish within 30s".to_string())?;
    if !out.status.success() {
        return Err(format!("ps PID inventory exited with {}", out.status));
    }
    let mut pids = vec![];
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let line = line.trim_start();
        let Some((pid, cmd)) = line.split_once(' ') else {
            continue;
        };
        if cmd.trim_start().starts_with(prefix) {
            pids.push(
                pid.trim()
                    .parse::<u32>()
                    .map_err(|e| format!("invalid PID inventory row: {e}"))?,
            );
        }
    }
    Ok(pids)
}

/// Launch the app with no declared env keys: on macOS the caller's environment carries only
/// `RIGHTKIT_*` keys. See [`launch_declared`].
pub fn launch(spec: &LaunchSpec, ws: &QaWorkspace, tracker: &Tracker) -> Result<Control> {
    launch_declared(spec, ws, tracker, &[])
}

/// [`launch`] for an app whose config declares extra env keys (`[ui] env_keys`): a macOS launch
/// may also carry exactly those keys besides `RIGHTKIT_*`. A key naming a credential is refused
/// whether declared or not. [`LaunchSpec`] has no field for them, so apps that build one with a
/// struct literal keep compiling.
pub fn launch_declared(
    spec: &LaunchSpec,
    ws: &QaWorkspace,
    tracker: &Tracker,
    declared_env_keys: &[String],
) -> Result<Control> {
    // An installed app is never driven unless installed-QA mode is on (see `installed`).
    crate::installed::refuse_installed(&spec.binary, "UI app binary")?;
    if !spec.binary.exists() {
        return err(format!(
            "UI app binary not found: {}",
            spec.binary.display()
        ));
    }
    // Before anything is created or started: the QA home must be a QA root this runner made, on the
    // internal disk, and clear of every user-data location (see `userdata`). No opt-out.
    let guard = crate::userdata::Guard::for_workspace(ws)?;
    let id = new_id().replace('-', "");
    let service = format!("control-{}", &id[..8]);
    // Isolated per-run suite root: the app's service.json never collides with an installed app's.
    let suite =
        Suite::at(ws.home.join(format!("suite-{}", &id[..8]))).map_err(|e| Error(e.to_string()))?;
    let log_path = ws.evidence_dir.join(format!("app-{}.log", &id[..8]));
    // launchd-started apps cannot open log files on external volumes (`open` fails with
    // -10810), so the live log goes to the system temp dir and is copied into the
    // evidence dir when the app stops.
    // Kept in a subdirectory: apps commonly list or watch HOME (file explorers, recents), and
    // a live log written directly into it is a change source that can feed back into the app.
    let log_dir = ws.home.join(".rightkit-qa");
    fs::create_dir_all(&log_dir)?;
    let raw_log = log_dir.join(format!("app-{}.log", &id[..8]));
    let mut env = launch_env(spec, ws, &suite, &service, declared_env_keys)?;
    guard.check_dir(&ws.home)?;
    guard.check_env(&env)?;
    // The app must be told its isolated data directory; otherwise it resolves its own default.
    crate::userdata::check_app_data_dir(&guard, &spec.binary, &env)?;
    // D-e: a launch that names a control token file reads the token now (fail closed: an unusable
    // file refuses the launch) and hands the app only the path.
    // ADR-0001 item 7: a launch that names no token file (the legacy path) mints one into the run
    // root, so every launch holds a token and names its file to the app. Nothing is sent without one.
    // N2: the world's file travels in `ws.env` (journeys), so it is named before `spec.env`; a mint
    // happens only when neither names one.
    let named = token::named_for_launch(&ws.env, &spec.env);
    let (auth, auth_file) = token::for_launch(named.as_deref(), &ws.root, &id[..8])?;
    // Exactly one value of the token file reaches the app: whatever `launch_env` copied from the
    // workspace is replaced, never duplicated (a `open --env` pair per value would race).
    token::set_one(&mut env, &auth_file);
    let frontmost_before = frontmost();
    let focus = FocusMonitor::start(spec.mode != Mode::Visible);
    let deadline = Instant::now() + spec.startup_timeout;

    // The executable identity is registered inside start_process BEFORE `open`, so
    // startup foreground samples are attributed; the pid follows once known.
    let (pid, child) = start_process(spec, ws, &env, &raw_log, &id, tracker, &focus, deadline)?;
    focus.track_primary(pid);
    tracker.register(pid, &spec.label);
    // Disposal may end what this app started and left in the home, and nothing else.
    crate::workspace::record_launched(ws, pid);

    let mut control = Control {
        pid,
        service: service.clone(),
        log_path,
        raw_log,
        suite: suite.clone(),
        endpoint: String::new(),
        client: Mutex::new(None),
        child,
        tracker: tracker.clone(),
        frontmost_before,
        focus: Some(focus),
        request_timeout: Duration::from_secs(20),
        worker: Mutex::new(None),
        pace_ms: std::env::var("RIGHTKIT_QA_PACE_MS")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0),
        stopped: false,
        focus_requested: spec.focus,
        auth: Some(auth),
        auth_file: auth_file.clone(),
    };
    let mut last_error: String;
    loop {
        // Each attempt is bounded by the startup deadline: a hung host must not defeat it.
        let left = deadline.saturating_duration_since(Instant::now());
        match bounded_connect_health(
            &suite,
            &service,
            left.min(Duration::from_secs(5)),
            control.auth.as_deref(),
        ) {
            Ok(c) => {
                control.endpoint = c.record().endpoint.clone();
                *control.client.lock().unwrap() = Some(c);
                break;
            }
            Err(e) => last_error = e,
        }
        if !is_alive(pid) {
            let tail = fs::read(&control.raw_log)
                .map(|b| crate::util::tail(&String::from_utf8_lossy(&b), 1500))
                .unwrap_or_default();
            return err(format!(
                "app exited before its control server came up (pid {pid}); log tail: {tail}"
            ));
        }
        if Instant::now() >= deadline {
            // Read everything while the app is still running, then stop it: `stop` copies the
            // live log into the evidence dir, which outlives the home that holds `raw_log`.
            let diagnostics =
                launch_timeout_diagnostics(&suite, &service, pid, &control.raw_log, &last_error);
            let kept = control.log_path.clone();
            let stopped = control.stop();
            let saved = match &stopped {
                Ok(_) if kept.is_file() => format!("; full app log kept at {}", kept.display()),
                Ok(_) => String::new(),
                Err(e) => format!("; stopping the app: {e}"),
            };
            return err(format!(
                "control service did not answer within {}ms; was the app built with its control feature and launched with RIGHTKIT_CONTROL_SERVICE?{}{diagnostics}{saved}",
                spec.startup_timeout.as_millis(),
                non_interactive_hint()
            ));
        }
        sleep_ms(100);
    }
    // The server comes up before the window exists (hidden launch): wait until the page
    // itself answers, so the first scenario step never races window creation.
    loop {
        // `about:blank` reports "complete" before the app's page has even started loading.
        if let Ok(Value::String(state)) =
            control.eval("return document.readyState + ' ' + location.href;")
        {
            let (ready, href) = state.split_once(' ').unwrap_or((&state, ""));
            if (ready == "complete" || ready == "interactive") && href != "about:blank" {
                break;
            }
        }
        if Instant::now() >= deadline {
            let alive = is_alive(pid);
            let _ = control.stop();
            return err(format!(
                "app window never became ready within {}ms (process alive: {}){}",
                spec.startup_timeout.as_millis(),
                alive,
                non_interactive_hint()
            ));
        }
        sleep_ms(20);
    }
    if spec.focus {
        if let Err(e) = control.focus_window() {
            let _ = control.stop();
            return err(format!(
                "focus requested but the window could not be focused: {}",
                e.0
            ));
        }
    }
    Ok(control)
}

/// Extra explanation for a launch timeout when the harness itself runs outside the
/// interactive desktop (SSH or a service on Windows): a WebView2 app launched from there
/// never gets a ready window. Empty everywhere else.
fn non_interactive_hint() -> String {
    #[cfg(windows)]
    if let Some(why) = windows_non_interactive_reason() {
        return format!(
            ". This harness process is not in the interactive desktop session ({why}); a hidden WebView2 app cannot get a ready window from a non-interactive session (SSH, service). Start the run from the logged-in user's desktop session, for example a one-shot `schtasks /create /it` task (see the rightkit-qa README, Windows section)"
        );
    }
    String::new()
}

/// `Some(reason)` when this process is in session 0 or on a window station that is not
/// visible (the shape of an SSH or service logon); `None` on an interactive desktop or when
/// it cannot be determined.
#[cfg(windows)]
fn windows_non_interactive_reason() -> Option<String> {
    use std::ffi::c_void;
    #[repr(C)]
    struct UserObjectFlags {
        inherit: i32,
        reserved: i32,
        flags: u32,
    }
    #[link(name = "user32")]
    extern "system" {
        fn GetProcessWindowStation() -> *mut c_void;
        fn GetUserObjectInformationW(
            h: *mut c_void,
            index: i32,
            info: *mut c_void,
            len: u32,
            needed: *mut u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcessId() -> u32;
        fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> i32;
    }
    const UOI_FLAGS: i32 = 1;
    const WSF_VISIBLE: u32 = 1;
    let mut session = 0u32;
    // SAFETY: plain Win32 queries on this process with correctly sized out-parameters.
    unsafe {
        if ProcessIdToSessionId(GetCurrentProcessId(), &mut session) != 0 && session == 0 {
            return Some("session 0".into());
        }
        let station = GetProcessWindowStation();
        if station.is_null() {
            return None;
        }
        let mut info = UserObjectFlags {
            inherit: 0,
            reserved: 0,
            flags: 0,
        };
        let mut needed = 0u32;
        if GetUserObjectInformationW(
            station,
            UOI_FLAGS,
            &mut info as *mut _ as *mut c_void,
            std::mem::size_of::<UserObjectFlags>() as u32,
            &mut needed,
        ) != 0
            && info.flags & WSF_VISIBLE == 0
        {
            return Some(format!("session {session}, non-visible window station"));
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn start_process(
    spec: &LaunchSpec,
    ws: &QaWorkspace,
    env: &[(String, String)],
    log_path: &Path,
    id: &str,
    _tracker: &Tracker,
    focus: &FocusMonitor,
    deadline: Instant,
) -> Result<(u32, Option<OwnedChild>)> {
    #[cfg(target_os = "macos")]
    {
        let bundle = stage_bundle(spec, ws, id)?;
        let needle = format!("{}/Contents/MacOS/", bundle.display());
        // Register the REAL executable (CFBundleExecutable) before `open`. When it cannot be
        // resolved and validated, register nothing: the launch interval stays unverified and
        // `owned_never_frontmost` fails closed instead of matching a guessed name.
        if let Some(executable) = resolve_bundle_executable(&bundle) {
            focus.track_prefix(executable);
        }
        let mut cmd = open_command(spec, ws, env, log_path, &bundle);
        let st = crate::process::status_bounded(&mut cmd, Duration::from_secs(60))?
            .ok_or_else(|| Error("`open` did not return within 60s".into()))?;
        if !st.success() {
            return err(format!("open exited {st}"));
        }
        loop {
            if let Some(pid) = pids_with_command_prefix(&needle)
                .ok()
                .and_then(|pids| pids.into_iter().next())
            {
                return Ok((pid, None));
            }
            if Instant::now() >= deadline {
                return err("launched app pid not found");
            }
            sleep_ms(100);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (ws, id);
        let log = fs::File::create(log_path)?;
        let log2 = log.try_clone()?;
        let mut cmd = Command::new(crate::util::plain_path(&spec.binary));
        cmd.current_dir(&ws.home)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log2));
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut owned = OwnedCommand::from_command(cmd);
        if spec.mode != Mode::Visible {
            owned.windows_hide();
        }
        let _ = deadline;
        let child = owned
            .spawn()
            .map_err(|e| Error(format!("failed to start {}: {e}", spec.binary.display())))?;
        // Attribute foreground samples to the app from its first instant.
        focus.track_primary(child.id());
        Ok((child.id(), Some(child)))
    }
}

impl Control {
    /// Fail when a system permission dialog is known to be showing for this app. TODO: the
    /// automation layer cannot enumerate windows by owner (`UserNotificationCenter`,
    /// `CoreServicesUIAgent`), so this is a no-op that returns `Ok` today; see
    /// [`crate::userdata::live_permission_dialog_check`]. Use
    /// [`StopReport::permission_dialog`] for the evidence that does exist.
    pub fn assert_no_permission_dialog(&self) -> Result<()> {
        match crate::userdata::live_permission_dialog_check(self.pid) {
            crate::userdata::DialogCheck::Dialog(owner) => err(format!(
                "a system permission dialog ({owner}) appeared for the app under test"
            )),
            _ => Ok(()),
        }
    }

    pub fn set_request_timeout(&mut self, t: Duration) {
        self.request_timeout = t;
    }

    /// One bounded request. The deadline is real: when it passes the connection is
    /// aborted (unblocking the read) and the call fails. A request that may already have
    /// reached the app is replayed only when the method is a pure read; effects
    /// (click, key, type, command, eval, ...) are never silently executed twice.
    fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        self.rpc_within(method, params, self.request_timeout)
    }

    fn rpc_within(&self, method: &str, params: Value, request_timeout: Duration) -> Result<Value> {
        require_order_token(self.auth.as_deref(), method)?;
        let replayable = matches!(method, "health" | "dom" | "ax" | "screenshot" | "move");
        let mut g = self.client.lock().unwrap();
        for attempt in 0..2 {
            let client = match g.as_ref() {
                Some(c) => c.clone(),
                None => {
                    // Not yet sent anything: reconnecting is always safe.
                    let c = Arc::new(connect_bounded(
                        &self.suite,
                        &self.service,
                        request_timeout,
                    )?);
                    *g = Some(c.clone());
                    c
                }
            };
            match call_bounded(
                &client,
                &self.worker,
                method,
                token::with_token(params.clone(), self.auth.as_deref()),
                request_timeout,
            ) {
                Ok(v) => return Ok(v),
                Err(CallError::TimedOut) => {
                    *g = None;
                    return err(format!(
                        "{method}: no reply within {}ms; connection aborted",
                        request_timeout.as_millis()
                    ));
                }
                Err(CallError::Service(e)) if e.retryable && replayable && attempt == 0 => {
                    *g = None
                }
                Err(CallError::Service(e)) => {
                    if e.retryable {
                        *g = None;
                    }
                    let mut m = svc_err(e);
                    if !replayable {
                        m = Error(format!("{} (not replayed: {method} has effects and the first attempt may have been applied)", m.0));
                    }
                    return Err(m);
                }
            }
        }
        err("control rpc failed")
    }

    fn json(&self, method: &str, params: Value) -> Result<Value> {
        let v = self.rpc(method, params)?;
        Self::ok_or_error(method, v)
    }

    fn ok_or_error(method: &str, v: Value) -> Result<Value> {
        if v.get("ok") == Some(&Value::Bool(false)) {
            return err(format!(
                "{method}: {}",
                v.get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
            ));
        }
        Ok(v)
    }

    /// Add the session's input pacing (`paceMs`), when one is set, to an input request.
    fn paced(&self, mut p: Value) -> Value {
        if self.pace_ms > 0 {
            p["paceMs"] = json!(self.pace_ms);
        }
        p
    }
    /// Gap between native input events the app inserts (default 0, no gap). Also set from
    /// `RIGHTKIT_QA_PACE_MS` at launch. The former fixed pauses were about click 40-50, drag 16,
    /// key 20, type 8 ms; use this for a page that needs real time between events.
    pub fn set_pace_ms(&mut self, ms: u64) {
        self.pace_ms = ms;
    }
    /// Run `steps` (`(method, params)`) in ONE round trip, stopping at the first failure. Every step is
    /// checked against this app's allowlist and passes the effect gate and input lease on its own.
    /// Returns each step's result; the error names the failing step. A batch is never replayed.
    pub fn batch(&self, steps: &[(&str, Value)]) -> Result<Vec<Value>> {
        let steps: Vec<Value> = steps
            .iter()
            .map(|(m, p)| {
                let p = if matches!(*m, "click" | "drag" | "key" | "type") {
                    self.paced(p.clone())
                } else {
                    p.clone()
                };
                json!({"method": m, "params": p})
            })
            .collect();
        // Allow the whole sequence: the per-request bound applies to each step, not the sum.
        let budget = self.request_timeout * (steps.len().max(1) as u32).min(30);
        let v = self.rpc_within("batch", json!({"steps": steps}), budget)?;
        let v = Self::ok_or_error("batch", v)?;
        Ok(v.get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    pub fn health(&self) -> Result<Value> {
        self.rpc("health", json!({}))
    }
    /// Resize hidden native content without screen clamping or activation.
    /// Waits for two matching page measurements 500 ms apart, after shell placement.
    /// Returns measured CSS dimensions; Windows or visible windows may report OS size limits.
    /// Unsettled sizes or placement timeouts are errors; this effect is never replayed.
    pub fn set_viewport(&self, width: u32, height: u32) -> Result<(u32, u32)> {
        let v = self.json("set_viewport", json!({"width": width, "height": height}))?;
        settled_viewport_size(&v)
    }
    /// Make the window key and focused (see `LaunchSpec::focus`). Opt-in; also callable mid-run.
    pub fn focus_window(&self) -> Result<Value> {
        self.json("focus", json!({}))
    }
    /// True when this launch asked for a key/focused window, so the never-frontmost proof does not apply.
    pub fn focus_requested(&self) -> bool {
        self.focus_requested
    }
    /// True when this launch handed the app a per-run control token (D-e) and attaches it to every
    /// order. It does not prove the app enforces it: see [`Control::probe_token_required`].
    pub fn token_attached(&self) -> bool {
        self.auth.is_some()
    }
    /// The token file this launch handed the app (the only [`token::ENV_AUTH_FILE`] value it received).
    pub fn control_token_file(&self) -> &Path {
        &self.auth_file
    }
    /// Prove the app enforces the control token: a fresh connection as the allowlisted app sends a
    /// token-less `health`, and the server must refuse it. Returns the refusal code. An app built
    /// before the token check answers instead, which is an error here, never a pass. Errors when this
    /// launch attached no token.
    pub fn probe_token_required(&self) -> Result<String> {
        if self.auth.is_none() {
            return err("this launch named no control token file, so there is no token to require");
        }
        let (suite, service, timeout) = (
            self.suite.clone(),
            self.service.clone(),
            self.request_timeout,
        );
        let reply = bounded(
            timeout,
            move || -> std::result::Result<Value, ServiceError> {
                let client = Client::connect(&suite, &service, APP)?;
                client.call("health", json!({}))
            },
        );
        match reply {
            None => err(format!(
                "token-less probe: no reply within {}ms",
                timeout.as_millis()
            )),
            Some(Ok(v)) => err(format!(
                "the control server answered a token-less order ({v}): the per-run token is not enforced; the app predates the token check and must re-pin rightkit-qa"
            )),
            Some(Err(e)) if e.code == token::REFUSED_CODE => Ok(e.code),
            Some(Err(e)) => err(format!(
                "token-less probe failed for a reason other than refusal: {e}"
            )),
        }
    }
    /// Prove the allowlist: an app that is not on it is refused at hello. Returns the error code.
    pub fn unlisted_app_probe(&self) -> Result<String> {
        match Client::connect(&self.suite, &self.service, "not-allowlisted-probe") {
            Ok(_) => err("an unlisted app was accepted by the control service"),
            Err(e) => Ok(e.code),
        }
    }
    pub fn click(&self, x: f64, y: f64, button: &str, count: u32) -> Result<()> {
        self.json(
            "click",
            self.paced(json!({"x": x, "y": y, "button": button, "count": count})),
        )
        .map(|_| ())
    }
    /// [`click`](Self::click) holding modifiers (`"shift"`, `"cmd"`, `"alt"`, `"ctrl"`). They
    /// travel on the native mouse events, so the page sees `e.shiftKey` etc. as real input.
    pub fn click_with(
        &self,
        x: f64,
        y: f64,
        button: &str,
        count: u32,
        modifiers: &[&str],
    ) -> Result<()> {
        self.json(
            "click",
            self.paced(
                json!({"x": x, "y": y, "button": button, "count": count, "modifiers": modifiers}),
            ),
        )
        .map(|_| ())
    }
    pub fn move_to(&self, x: f64, y: f64) -> Result<()> {
        self.json("move", json!({"x": x, "y": y})).map(|_| ())
    }
    /// One native pointer phase (`down`, `drag`, `up`, `move`) in viewport CSS
    /// pixels. Use down/drag/up for a scrub split across requests; keep the same
    /// button and modifiers on each phase. Hidden launches use the embedded path.
    pub fn pointer(
        &self,
        phase: &str,
        x: f64,
        y: f64,
        button: &str,
        modifiers: &[&str],
    ) -> Result<()> {
        self.json(
            "pointer",
            json!({"phase": phase, "x": x, "y": y, "button": button, "modifiers": modifiers}),
        )
        .map(|_| ())
    }
    pub fn drag(&self, from: (f64, f64), to: (f64, f64), steps: u32) -> Result<()> {
        self.json(
            "drag",
            self.paced(json!({"x1": from.0, "y1": from.1, "x2": to.0, "y2": to.1, "steps": steps})),
        )
        .map(|_| ())
    }
    pub fn wheel(&self, x: f64, y: f64, dx: f64, dy: f64) -> Result<()> {
        self.json("wheel", json!({"x": x, "y": y, "dx": dx, "dy": dy}))
            .map(|_| ())
    }
    pub fn key(&self, spec: &str) -> Result<()> {
        self.json("key", self.paced(json!({"key": spec})))
            .map(|_| ())
    }
    pub fn type_text(&self, text: &str) -> Result<()> {
        self.json("type", self.paced(json!({"text": text})))
            .map(|_| ())
    }
    /// Evaluate a JS function body (`return ...`) in the page; returns its JSON value.
    pub fn eval(&self, js: &str) -> Result<Value> {
        Ok(self
            .json("eval", json!({"js": js}))?
            .get("value")
            .cloned()
            .unwrap_or(Value::Null))
    }
    /// QA IPC fixture (needs the app's `control` build with the service active, which injects
    /// `window.__RK_QA_IPC__`): answer the app's Tauri IPC command `rule["cmd"]` from the page
    /// instead of its engine. `rule` is `{cmd, args?: subset match, replies: [{kind: "ok"|"error",
    /// value}], times?}`; replies are used in order and the last repeats. Unmatched commands reach
    /// the real engine. Rules live in the page: a reload or navigation drops them, so add them again
    /// after it. Returns the number of rules now installed.
    pub fn ipc_fixture_add(&self, rule: &Value) -> Result<u64> {
        let js = format!(
            "if (!window.__RK_QA_IPC__) return null; return window.__RK_QA_IPC__.add({rule});"
        );
        self.eval(&js)?.as_u64().ok_or_else(|| {
            Error("ipc fixture not installed (no control build, or the page reloaded)".into())
        })
    }
    /// Every IPC call the page made since the fixture was installed or cleared, oldest first, as
    /// `{cmd, args}` (matched or not; at most the last 1000). Empty before the fixture exists.
    pub fn ipc_fixture_log(&self) -> Result<Vec<Value>> {
        let v = self.eval("return window.__RK_QA_IPC__ ? window.__RK_QA_IPC__.log() : [];")?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }
    /// Remove all fixture rules and the log. A reload drops them anyway.
    pub fn ipc_fixture_clear(&self) -> Result<()> {
        self.eval("if (window.__RK_QA_IPC__) window.__RK_QA_IPC__.clear(); return null;")
            .map(|_| ())
    }
    pub fn dom(&self, selector: &str) -> Result<Vec<Value>> {
        let v = self.json("dom", json!({"selector": selector}))?;
        Ok(v.get("elements")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }
    pub fn ax(&self) -> Result<Vec<Value>> {
        let v = self.json("ax", json!({}))?;
        Ok(v.get("nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }
    pub fn screenshot_png(&self) -> Result<Vec<u8>> {
        let v = self.json("screenshot", json!({"inline": true}))?;
        // The server masks credential elements before the image leaves the page; a server that
        // does not report `masked` predates redaction and its captures are refused.
        if v.get("masked").and_then(Value::as_u64).is_none() {
            return err("control server does not redact screenshots; capture refused");
        }
        let body = match v.get("data").and_then(Value::as_str) {
            Some(b64) => crate::util::base64_decode(b64)
                .ok_or_else(|| Error("screenshot data is not valid base64".into()))?,
            // A server older than the inline reply writes the PNG and returns its path.
            None => {
                let path = PathBuf::from(
                    v.get("path")
                        .and_then(Value::as_str)
                        .ok_or_else(|| Error("screenshot returned no data or path".into()))?,
                );
                let body = fs::read(&path)?;
                let _ = fs::remove_file(&path);
                body
            }
        };
        if body.len() < 8 || &body[1..4] != b"PNG" {
            return err("screenshot method did not return a PNG");
        }
        Ok(body)
    }
    pub fn screenshot_to(&self, path: &Path) -> Result<()> {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)?;
        }
        fs::write(path, self.screenshot_png()?)?;
        Ok(())
    }
    /// A typed app command registered with `Control::command(name, f)`. The server
    /// hands the command its `args` string verbatim.
    pub fn command(&self, name: &str, args: &Value) -> Result<Value> {
        let args = match args {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        };
        let v = self.json("command", json!({"name": name, "args": args}))?;
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Centre of the first element matching `selector`, in viewport CSS px.
    pub fn center_of(&self, selector: &str) -> Result<(f64, f64)> {
        let els = self.dom(selector)?;
        let first = els
            .first()
            .ok_or_else(|| Error(format!("selector matched no element: {selector}")))?;
        let r: Vec<f64> = first
            .get("rect")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default();
        if r.len() < 4 || r[2] <= 0.0 || r[3] <= 0.0 {
            return err(format!("{selector} has no visible box"));
        }
        Ok((r[0] + r[2] / 2.0, r[1] + r[3] / 2.0))
    }
    pub fn click_selector(&self, selector: &str) -> Result<()> {
        let (x, y) = self.center_of(selector)?;
        self.click(x, y, "left", 1)
    }

    /// Click `selector`, then wait for `done_js` (a JS body, see [`Control::wait_eval`]) to become
    /// truthy; when the wait times out, click again. For controls that drop the first click
    /// (an unfocused window, a control still mounting). Gives up after `attempts` clicks.
    pub fn click_until(
        &self,
        selector: &str,
        done_js: &str,
        attempts: u32,
        wait: Duration,
    ) -> Result<Value> {
        let mut last = String::new();
        for _ in 0..attempts.max(1) {
            self.click_selector(selector)?;
            match self.wait_eval(done_js, wait) {
                Ok(v) => return Ok(v),
                Err(e) => last = e.0,
            }
        }
        err(format!(
            "click_until `{selector}`: `{done_js}` never became true after {} click(s): {last}",
            attempts.max(1)
        ))
    }

    /// The live app log (stdout and stderr) while the app runs. [`Control::stop`] copies it into
    /// the evidence directory; this path is the source.
    pub fn live_log_path(&self) -> &Path {
        &self.raw_log
    }

    /// The last `max_chars` bytes of the live app log (empty when it is unreadable).
    pub fn log_tail(&self, max_chars: usize) -> String {
        fs::read(&self.raw_log)
            .map(|b| crate::util::tail(&String::from_utf8_lossy(&b), max_chars))
            .unwrap_or_default()
    }

    /// Start recording uncaught page errors and unhandled promise rejections into
    /// `window.__rkPageErrors`. Idempotent. Errors raised before this call are not recorded.
    pub fn record_page_errors(&self) -> Result<()> {
        self.eval(
            "if (!window.__rkPageErrorsInstalled) { window.__rkPageErrorsInstalled = true; \
             window.__rkPageErrors = window.__rkPageErrors || []; \
             window.addEventListener('error', function (e) { window.__rkPageErrors.push({kind: 'error', \
             message: String(e.message || ''), source: String(e.filename || ''), line: e.lineno || 0}); }); \
             window.addEventListener('unhandledrejection', function (e) { var r = e.reason; \
             window.__rkPageErrors.push({kind: 'unhandledrejection', message: String((r && r.message) || r)}); }); } \
             return true;",
        )?;
        Ok(())
    }

    /// Page errors recorded by [`Control::record_page_errors`], oldest first.
    pub fn page_errors(&self) -> Result<Vec<Value>> {
        let v = self.eval("return window.__rkPageErrors || [];")?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    /// Failure evidence for a scenario, written to `dir/<label>-failure.json`: the page URL, the
    /// recorded page errors, the live log tail and, when `screenshot` is true, a PNG next to it.
    /// Pass `screenshot = false` in a functional run: a capture is refused there. Returns the JSON path.
    pub fn failure_evidence(&self, dir: &Path, label: &str, screenshot: bool) -> Result<PathBuf> {
        fs::create_dir_all(dir)?;
        let url = self.eval("return location.href;").unwrap_or(Value::Null);
        let page_errors = self.page_errors().unwrap_or_default();
        let shot = if screenshot {
            let png = dir.join(format!("{label}-failure.png"));
            self.screenshot_to(&png)?;
            Some(png.display().to_string())
        } else {
            None
        };
        let body = json!({
            "url": url,
            "pageErrors": page_errors,
            "logTail": self.log_tail(4000),
            "screenshot": shot,
        });
        let path = dir.join(format!("{label}-failure.json"));
        fs::write(&path, serde_json::to_vec_pretty(&body).unwrap_or_default())?;
        Ok(path)
    }

    /// Wait until a JS function body returns something truthy; bounded. One round trip: the app
    /// waits in the page (DOM mutations, events, a short timer) and replies the moment it holds.
    /// An app on an older driver without the `wait` method is polled every 25 ms instead.
    pub fn wait_eval(&self, js: &str, timeout: Duration) -> Result<Value> {
        let ms = timeout.as_millis().clamp(1, 600_000) as u64;
        let reply = self.rpc_within(
            "wait",
            json!({"js": js, "timeoutMs": ms}),
            timeout + self.request_timeout,
        );
        match reply {
            Ok(v) => {
                let v = Self::ok_or_error("wait", v)?;
                if v.get("matched") == Some(&Value::Bool(true)) {
                    return Ok(v.get("value").cloned().unwrap_or(Value::Null));
                }
                let last = match v.get("error") {
                    Some(e) if !e.is_null() => e.clone(),
                    _ => v.get("last").cloned().unwrap_or(Value::Null),
                };
                err(format!("timeout waiting for `{js}` (last: {last})"))
            }
            Err(e) if wait_unsupported(&e.0) => self.wait_eval_polling(js, timeout),
            Err(e) => Err(e),
        }
    }

    fn wait_eval_polling(&self, js: &str, timeout: Duration) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        let mut last;
        loop {
            match self.eval(js) {
                Ok(v) => {
                    let truthy = !matches!(v, Value::Null | Value::Bool(false))
                        && v != json!(0)
                        && v != json!("");
                    if truthy {
                        return Ok(v);
                    }
                    last = v;
                }
                Err(e) => last = json!(e.0),
            }
            if Instant::now() >= deadline {
                return err(format!("timeout waiting for `{js}` (last: {last})"));
            }
            sleep_ms(25);
        }
    }

    /// Wait until `text` appears in the visible text (`innerText`) of the first element
    /// matching `selector` (`None` = the whole body); see [`wait_eval`](Self::wait_eval).
    pub fn wait_for_text(
        &self,
        selector: Option<&str>,
        text: &str,
        timeout: Duration,
    ) -> Result<()> {
        let sel = serde_json::to_string(selector.unwrap_or("body")).unwrap_or_default();
        let needle = serde_json::to_string(text).unwrap_or_default();
        let js = format!(
            "return ((document.querySelector({sel}) || {{}}).innerText || '').includes({needle});"
        );
        self.wait_eval(&js, timeout).map(|_| ())
    }

    /// Stop the app: kill the recorded pid tree, prove the port closed, and check
    /// we did not change the frontmost app.
    pub fn stop(&mut self) -> Result<StopReport> {
        if self.stopped {
            return err("control session already stopped");
        }
        self.stopped = true;
        if let Some(mut c) = self.child.take() {
            if let (_, Some(note)) = crate::process::terminate_bounded(&mut c) {
                eprintln!("{note}");
            }
        }
        kill_tree(self.pid);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && (is_alive(self.pid) || endpoint_open(&self.endpoint)) {
            sleep_ms(50);
        }
        // Keep the live log until workspace finish/disposal; failed evidence copies
        // must not destroy the only readable startup diagnostics.
        let log_copy = fs::copy(&self.raw_log, &self.log_path);
        let after = frontmost();
        let (
            steady,
            states,
            owned_never_frontmost,
            owned_frontmost_pids,
            frontmost_pid_unavailable_samples,
            owned_never_frontmost_failure,
        ) = self.focus.take().map(|mut f| f.finish()).unwrap_or((
            false,
            vec![],
            false,
            vec![],
            0,
            Some("no focus monitor ran".to_string()),
        ));
        let process_gone = !is_alive(self.pid);
        let endpoint_closed = self.endpoint.is_empty() || !endpoint_open(&self.endpoint);
        if process_gone {
            self.tracker.forget(self.pid);
        }
        log_copy.map_err(|e| {
            Error(format!(
                "cannot preserve app log {}: {e}",
                self.raw_log.display()
            ))
        })?;
        Ok(StopReport {
            pid: self.pid,
            endpoint_closed,
            process_gone,
            frontmost_unchanged: steady
                && self.frontmost_before.is_some()
                && self.frontmost_before == after,
            frontmost_states: states,
            frontmost_before: self.frontmost_before.clone(),
            frontmost_after: after,
            owned_never_frontmost,
            owned_frontmost_pids,
            frontmost_pid_unavailable_samples,
            owned_never_frontmost_failure,
        })
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        if !self.stopped {
            let _ = self.stop();
        }
    }
}

// Keep wire validation shared by the public client and protocol regression tests.
fn settled_viewport_size(v: &Value) -> Result<(u32, u32)> {
    if v.get("settled").and_then(Value::as_bool) != Some(true)
        || v.get("placementTimedOut").and_then(Value::as_bool) != Some(false)
    {
        return err(format!("set_viewport did not settle after placement: {v}"));
    }
    let dimension = |key: &str| {
        v.get(key)
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
            .ok_or_else(|| Error(format!("set_viewport returned invalid {key}")))
    };
    Ok((dimension("innerWidth")?, dimension("innerHeight")?))
}

#[cfg(test)]
mod launch_path_tests {
    use super::*;

    #[test]
    fn viewport_requires_confirmed_settling_and_placement() {
        let good = json!({"innerWidth": 1440, "innerHeight": 789, "settled": true, "placementTimedOut": false});
        assert_eq!(settled_viewport_size(&good).unwrap(), (1440, 789));
        for (key, value) in [
            ("settled", json!(false)),
            ("settled", Value::Null),
            ("placementTimedOut", json!(true)),
            ("placementTimedOut", Value::Null),
            ("innerWidth", json!(0)),
            ("innerHeight", json!(4294967296u64)),
        ] {
            let mut reply = good.clone();
            reply[key] = value;
            assert!(settled_viewport_size(&reply).is_err(), "{reply}");
        }
    }

    #[test]
    fn windows_foreground_is_classified_without_a_windows_host() {
        assert_eq!(classify_foreground(false, 0, 0), Foreground::NoWindow);
        assert_eq!(classify_foreground(true, 7, 4242), Foreground::Pid(4242));
        // A foreground window whose owner cannot be read is not "nothing in front".
        assert_eq!(classify_foreground(true, 0, 0), Foreground::Unreadable);
        assert_eq!(classify_foreground(true, 7, 0), Foreground::Unreadable);
        assert_eq!(classify_foreground(true, 0, 4242), Foreground::Unreadable);
    }

    #[test]
    fn background_failure_reason_tells_frontmost_from_unreadable() {
        let r = owned_failure_reason(&[42], 3, true, 3);
        assert!(r.contains("was frontmost") && r.contains("42"), "{r}");
        let r = owned_failure_reason(&[], 2, true, 2);
        assert!(r.contains("pid could not be read in 2"), "{r}");
        assert!(!r.contains("was frontmost"), "{r}");
        assert!(owned_failure_reason(&[], 0, false, 0).contains("never registered"));
        assert!(owned_failure_reason(&[], 0, true, 4).contains("unmeasurable"));
    }

    #[test]
    fn windows_background_invariant_fails_closed() {
        // Visible launches enforce nothing.
        assert!(windows_owned_never_frontmost(false, 3, &[9]));
        // Background: the owned pid in front, or an unreadable owner, both fail.
        assert!(windows_owned_never_frontmost(true, 0, &[]));
        assert!(!windows_owned_never_frontmost(true, 0, &[9]));
        assert!(!windows_owned_never_frontmost(true, 1, &[]));
    }

    #[cfg(windows)]
    #[test]
    fn windows_startup_focus_is_attributed_when_primary_pid_arrives() {
        // A foreground event can arrive after spawn but before its PID is registered.
        let mut monitor = FocusMonitor {
            stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            seen: Arc::new(Mutex::new((
                3,
                0,
                vec!["1:a".into(), "42:b".into(), "1:a".into()],
            ))),
            owned: Arc::new(Mutex::new((vec![], None))),
            enforce_owned: true,
            owned_frontmost: Arc::new(Mutex::new(vec![])),
            unreadable_owners: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            thread: None,
            notifications: None,
            foreground_pids: Arc::new(Mutex::new(vec![1, 42])),
        };
        monitor.track_primary(42);
        let (unchanged, _, never_frontmost, observed, _, _) = monitor.finish();
        assert!(!unchanged);
        assert!(!never_frontmost);
        assert_eq!(observed, vec![42]);
    }

    #[test]
    fn launch_timeout_diagnostics_name_the_suite_the_service_and_the_log() {
        let (ws, _spec) = fixture();
        let suite = Suite::at(ws.home.join("suite-aaaaaaaa")).unwrap();
        // The app registered under another service name than the one the client polled.
        let other = suite.service_dir("control-other");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("service.json"), "{}").unwrap();
        let log = ws.home.join("app.log");
        fs::write(
            &log,
            format!("{}\nRIGHTKIT QA CONTROL SERVER ACTIVE", "x".repeat(5000)),
        )
        .unwrap();
        let text = launch_timeout_diagnostics(
            &suite,
            "control-aaaaaaaa",
            std::process::id(),
            &log,
            "connect: no service.json for control-aaaaaaaa",
        );
        assert!(text.contains(&suite.root().display().to_string()), "{text}");
        assert!(text.contains("control-aaaaaaaa"), "{text}");
        assert!(text.contains("no readable record"), "{text}");
        assert!(text.contains("control-other (service.json)"), "{text}");
        assert!(
            text.contains("no service.json for control-aaaaaaaa"),
            "{text}"
        );
        assert!(text.contains("RIGHTKIT QA CONTROL SERVER ACTIVE"), "{text}");
        assert!(text.contains(&log.display().to_string()), "{text}");
        // Only the tail of a long log is quoted.
        assert!(text.len() < 5000, "{}", text.len());
        // A log that was never written says so instead of printing nothing.
        let none = launch_timeout_diagnostics(&suite, "s", 1, &ws.home.join("missing.log"), "");
        assert!(none.contains("log unreadable"), "{none}");
        cleanup(&ws);
    }

    fn fixture() -> (QaWorkspace, LaunchSpec) {
        let root = crate::storage::app_temp_dir().join(format!("rkqa-launch-{}", new_id()));
        let ws = crate::workspace::create(&root, None, "control-test-app").unwrap();
        let binary = ws.root.join("control-test-app");
        fs::write(&binary, "fixture binary").unwrap();
        let spec = LaunchSpec {
            binary,
            mode: Mode::Hidden,
            env: vec![],
            startup_timeout: Duration::from_secs(90),
            label: "fixture".into(),
            focus: false,
        };
        (ws, spec)
    }

    fn cleanup(ws: &QaWorkspace) {
        let root = ws
            .root
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let report = crate::workspace::dispose(ws, false, 1, 0);
        assert!(report.retained.is_empty(), "{report:?}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn launch_environment_keeps_all_owned_paths_internal_and_rejects_external_overrides() {
        let (ws, mut spec) = fixture();
        let suite = Suite::at(ws.home.join("suite-12345678")).unwrap();
        let env: std::collections::BTreeMap<_, _> =
            launch_env(&spec, &ws, &suite, "control-12345678", &[])
                .unwrap()
                .into_iter()
                .collect();
        for (key, value) in &env {
            if Path::new(value).is_absolute() {
                assert!(Path::new(value).starts_with(&ws.home), "{key}={value}");
                assert!(!Path::new(value).starts_with(&ws.root), "{key}={value}");
            }
        }
        assert_eq!(env["HOME"], ws.home.to_string_lossy());
        assert_eq!(env["TMPDIR"], ws.home.join("tmp").to_string_lossy());
        assert_eq!(env["RIGHTKIT_SUITE_ROOT"], suite.root().to_string_lossy());
        assert_eq!(env["RIGHTKIT_QA_HIDDEN"], "1");
        assert_eq!(env["RIGHTKIT_QA_BACKGROUND"], "1");
        // Default launch is unchanged: no focus.
        assert!(!spec.focus);
        assert_eq!(env["RIGHTKIT_QA_FOCUS"], "0");
        let focused = LaunchSpec {
            focus: true,
            ..spec.clone()
        };
        let focused_env: std::collections::BTreeMap<_, _> =
            launch_env(&focused, &ws, &suite, "control-12345678", &[])
                .unwrap()
                .into_iter()
                .collect();
        assert_eq!(focused_env["RIGHTKIT_QA_FOCUS"], "1");
        // Focus does not turn a hidden launch into a visible one.
        assert_eq!(focused_env["RIGHTKIT_QA_HIDDEN"], "1");
        // rightkit-daemon falls back to TMPDIR/rk-<suite-hash>-<service>.sock
        // when its runtime socket exceeds 100 bytes. App TMPDIR must fit too.
        #[cfg(target_os = "macos")]
        assert!(
            Path::new(&env["TMPDIR"])
                .join("rk-12345678-control-12345678.sock")
                .as_os_str()
                .len()
                <= 100
        );
        assert!(suite
            .service_dir("control-12345678")
            .join("service.json")
            .starts_with(&ws.home));
        for key in [
            "RIGHTKIT_SUITE_ROOT",
            "RIGHTKIT_QA_DATA_DIR",
            "RIGHTKIT_RUN_ROOT",
        ] {
            spec.env = vec![(
                key.into(),
                ws.root.join("external").to_string_lossy().into(),
            )];
            assert!(
                launch_env(&spec, &ws, &suite, "control-12345678", &[]).is_err(),
                "{key}"
            );
        }
        spec.env = vec![("RIGHTKIT_SECRET".into(), "redacted".into())];
        assert!(launch_env(&spec, &ws, &suite, "control-12345678", &[]).is_err());
        cleanup(&ws);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn raw_binary_and_supplied_bundle_are_staged_at_a_fixed_path_before_open() {
        let (ws, mut spec) = fixture();
        let id = "1234567890abcdef";
        let bundle = stage_bundle(&spec, &ws, id).unwrap();
        // One fixed path and one fixed bundle id per app, never a per-run folder or id.
        assert_eq!(
            bundle,
            crate::storage::home_base()
                .join("rightkit-qa-apps/control-test-app/control-test-app.app")
        );
        assert!(!bundle.starts_with(&ws.home) && !bundle.starts_with(&ws.root));
        let again = stage_bundle(&spec, &ws, "fedcba0987654321").unwrap();
        assert_eq!(again, bundle);
        let plist_text = fs::read_to_string(bundle.join("Contents/Info.plist")).unwrap();
        assert!(plist_text.contains("<string>app.rightkit.qa.control-test-app</string>"));
        assert!(!bundle
            .with_file_name("control-test-app.app.previous")
            .exists());
        // A different build replaces the bundle in place and keeps no `.previous` copy (D1, #771).
        fs::write(&spec.binary, "second build").unwrap();
        let next = stage_bundle(&spec, &ws, "0123456789abcdef").unwrap();
        assert_eq!(next, bundle);
        assert_eq!(
            fs::read(bundle.join("Contents/MacOS/control-test-app")).unwrap(),
            b"second build"
        );
        assert!(!bundle
            .with_file_name("control-test-app.app.previous")
            .exists());
        fs::write(&spec.binary, "fixture binary").unwrap();
        let bundle = stage_bundle(&spec, &ws, id).unwrap();
        assert_eq!(
            fs::read(bundle.join("Contents/MacOS/control-test-app")).unwrap(),
            b"fixture binary"
        );
        let plist = fs::read_to_string(bundle.join("Contents/Info.plist")).unwrap();
        assert!(plist.contains("<key>LSUIElement</key><true/>"));
        let suite = Suite::at(ws.home.join("suite-12345678")).unwrap();
        let env = launch_env(&spec, &ws, &suite, "control-12345678", &[]).unwrap();
        let log = ws.home.join("app-12345678.log");
        let cmd = open_command(&spec, &ws, &env, &log, &bundle);
        assert_eq!(cmd.get_current_dir(), Some(ws.home.as_path()));
        let args: Vec<_> = cmd
            .get_args()
            .map(|v| v.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&args[..3], &["-g", "-j", "-n"]);
        for (key, value) in &env {
            assert!(
                args.windows(2)
                    .any(|pair| pair[0] == "--env" && pair[1] == format!("{key}={value}")),
                "{key}"
            );
        }
        for flag in ["--stdout", "--stderr"] {
            assert!(args
                .windows(2)
                .any(|pair| pair[0] == flag && Path::new(&pair[1]) == log));
        }
        assert_eq!(args.last().unwrap(), &bundle.to_string_lossy());
        // A provided .app must be copied too; it may live on the external build volume.
        spec.binary = make_bundle(&ws.root, &spec.binary, "original", true).unwrap();
        fs::create_dir_all(spec.binary.join("Contents/Resources")).unwrap();
        fs::write(
            spec.binary.join("Contents/Resources/fixture.txt"),
            "resource",
        )
        .unwrap();
        let supplied = stage_bundle(&spec, &ws, "fedcba0987654321").unwrap();
        assert!(!supplied.starts_with(&ws.home));
        assert_ne!(supplied, spec.binary);
        assert_eq!(
            fs::read_to_string(supplied.join("Contents/Resources/fixture.txt")).unwrap(),
            "resource"
        );
        std::os::unix::fs::symlink(
            ws.root.join("control-test-app"),
            spec.binary.join("Contents/Resources/external"),
        )
        .unwrap();
        assert!(stage_bundle(&spec, &ws, "0123456789abcdef")
            .unwrap_err()
            .0
            .contains("escapes internal bundle"));
        let _ = fs::remove_dir_all(
            crate::storage::home_base().join("rightkit-qa-apps/control-test-app"),
        );
        cleanup(&ws);
    }

    fn keys(list: &[&str]) -> Vec<String> {
        list.iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn a_declared_key_is_allowed_under_the_macos_rules() {
        let declared = keys(&["MYAPP_MODE", "MYAPP_PROFILE"]);
        env_key_allowed("MYAPP_MODE", &declared, true).unwrap();
        env_key_allowed("MYAPP_PROFILE", &declared, true).unwrap();
    }

    #[test]
    fn an_undeclared_non_rightkit_key_is_refused_under_the_macos_rules() {
        let declared = keys(&["MYAPP_MODE"]);
        let refused = env_key_allowed("MYAPP_OTHER", &declared, true).unwrap_err();
        assert!(refused.0.contains("MYAPP_OTHER"), "{refused}");
        assert!(refused.0.contains("env_keys"), "{refused}");
        assert!(env_key_allowed("MYAPP_MODE", &[], true).is_err());
        // A declaration is the exact key: neither another case nor a longer name matches it.
        assert!(env_key_allowed("myapp_mode", &declared, true).is_err());
        assert!(env_key_allowed("MYAPP_MODE_X", &declared, true).is_err());
        // Off macOS the argv rule does not apply, as before.
        env_key_allowed("MYAPP_OTHER", &[], false).unwrap();
    }

    #[test]
    fn a_rightkit_key_needs_no_declaration() {
        env_key_allowed("RIGHTKIT_QA_FOCUS", &[], true).unwrap();
        env_key_allowed("RIGHTKIT_QA_FOCUS", &[], false).unwrap();
        // The launcher's own window label key must pass the macOS rules without a declaration.
        env_key_allowed(WINDOW_LABEL_ENV, &[], true).unwrap();
    }

    #[test]
    fn a_credential_named_key_is_refused_even_when_declared_and_off_macos() {
        let declared = keys(&["MYAPP_API_KEY", "MYAPP_TOKEN"]);
        for macos_rules in [true, false] {
            for key in [
                "MYAPP_API_KEY",
                "MYAPP_TOKEN",
                "RIGHTKIT_SECRET",
                "myapp_password",
            ] {
                let refused = env_key_allowed(key, &declared, macos_rules).unwrap_err();
                assert!(refused.0.contains("names a credential"), "{key}: {refused}");
            }
        }
        assert!(env_key_allowed("MYAPP_API_KEY", &[], false).is_err());
        assert!(key_names_credential("Db_PassWd"));
        assert!(!key_names_credential("MYAPP_MODE"));
    }

    #[test]
    fn the_token_file_key_is_allowed_with_or_without_a_declaration() {
        for macos_rules in [true, false] {
            env_key_allowed(token::ENV_AUTH_FILE, &[], macos_rules).unwrap();
            env_key_allowed(token::ENV_AUTH_FILE, &keys(&["X"]), macos_rules).unwrap();
        }
    }

    #[test]
    fn check_env_applies_the_declared_keys_and_the_credential_rule_to_every_pair() {
        let env = vec![
            ("RIGHTKIT_QA_FOCUS".to_string(), "1".to_string()),
            ("MYAPP_MODE".to_string(), "qa".to_string()),
        ];
        check_env(&env, &keys(&["MYAPP_MODE"])).unwrap();
        // Undeclared, only macOS refuses it (an `open` argument there).
        assert_eq!(check_env(&env, &[]).is_ok(), !cfg!(target_os = "macos"));
        let secret = vec![("MYAPP_TOKEN".to_string(), "x".to_string())];
        assert!(check_env(&secret, &keys(&["MYAPP_TOKEN"])).is_err());
    }

    #[test]
    fn launch_env_carries_a_declared_key_and_still_refuses_a_declared_credential() {
        let (ws, mut spec) = fixture();
        let suite = Suite::at(ws.home.join("suite-12345678")).unwrap();
        let declared = keys(&["MYAPP_MODE", "MYAPP_API_KEY"]);
        spec.env = vec![("MYAPP_MODE".into(), "qa".into())];
        let env: std::collections::BTreeMap<_, _> =
            launch_env(&spec, &ws, &suite, "control-12345678", &declared)
                .unwrap()
                .into_iter()
                .collect();
        assert_eq!(env["MYAPP_MODE"], "qa");
        // Without the declaration the launch is refused on macOS and unchanged elsewhere.
        assert_eq!(
            launch_env(&spec, &ws, &suite, "control-12345678", &[]).is_ok(),
            !cfg!(target_os = "macos")
        );
        spec.env = vec![("MYAPP_API_KEY".into(), "x".into())];
        let refused = launch_env(&spec, &ws, &suite, "control-12345678", &declared).unwrap_err();
        assert!(refused.0.contains("names a credential"), "{refused}");
        cleanup(&ws);
    }

    #[test]
    fn the_window_label_is_the_apps_own_then_the_launchers_then_main() {
        // The app's own `.window(..)` wins over the launcher's label.
        assert_eq!(resolve_window_label(Some("editor"), Some("side")), "editor");
        // No explicit label: the launcher's `[ui] window_label` from the environment.
        assert_eq!(resolve_window_label(None, Some("side")), "side");
        // Neither: the default.
        assert_eq!(resolve_window_label(None, None), "main");
    }

    #[test]
    fn a_blank_window_label_counts_as_absent_and_the_winner_is_trimmed() {
        assert_eq!(resolve_window_label(Some("  "), Some("side")), "side");
        assert_eq!(resolve_window_label(Some(""), None), "main");
        assert_eq!(resolve_window_label(None, Some("\t\n")), "main");
        assert_eq!(resolve_window_label(Some("  "), Some(" ")), "main");
        assert_eq!(resolve_window_label(Some(" editor "), None), "editor");
        assert_eq!(resolve_window_label(None, Some(" side ")), "side");
        assert_eq!(WINDOW_LABEL_ENV, "RIGHTKIT_QA_WINDOW_LABEL");
    }
}
