//! Rotation exclusion across SEPARATE PROCESSES (review finding R4, reopened).
//!
//! A backend call that times out keeps running on its worker thread. If the cross-process
//! rotation lock were released when the timed-out invocation returned, another process could
//! take the lock, abort and re-begin, and the abandoned write would then land over the newer
//! primary. These tests run real processes (this test binary re-invoking itself with a role in
//! the environment) over a file-backed durable test store under a temp directory, with a stall
//! the test controls by sentinel files. No OS credential store and no user credential is touched.
//!
//! Sentinel files in the shared root: `stall-set` (armed: the next write of `db`, by whichever
//! process claims it first, stalls), `entered` (the stalled write started), `release` (let it
//! finish), `set-done` (the stalled write finished), `a-timed-out` (process A saw its Timeout).

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rightkit_credentials::{
    Backend, Config, Error, Interaction, Namespace, Secret, Store, UNIVERSAL_GUARD_ENV,
};

const ROLE_VAR: &str = "RK_ROTATION_PROCESS_ROLE";
const ROOT_VAR: &str = "RK_ROTATION_PROCESS_ROOT";
const NS: &str = "dev.rightkit.secrets.test.rotation-process";
const WAIT: Duration = Duration::from_secs(30);

fn hex(text: &str) -> String {
    text.bytes().map(|b| format!("{b:02x}")).collect()
}

fn io_error() -> Error {
    Error::Backend {
        op: "file-test",
        code: -1,
    }
}

/// A durable test store: one file per item under `<root>/items`, shared by every process that
/// opens the same root. Writes of `db` stall on request (see the module comment).
struct FileBackend {
    root: PathBuf,
    /// The diagnostic `Backend::name`. Two labels over one root are two ALIASES of one native
    /// store, like the macOS default keychain and an explicit file on the same keychain: same
    /// `exclusion_identity` and `credential_identity`, different `name`.
    label: &'static str,
}

impl FileBackend {
    fn new(root: &Path, label: &'static str) -> Self {
        fs::create_dir_all(root.join("items")).unwrap();
        Self {
            root: root.to_path_buf(),
            label,
        }
    }

    fn item(&self, ns: &Namespace, account: &str) -> PathBuf {
        self.root
            .join("items")
            .join(format!("{}-{}", hex(ns.service()), hex(account)))
    }

    /// Only the first writer to claim `stall-set` stalls, and only on the `db` account. Returns
    /// whether THIS call stalled.
    fn stall_if_armed(&self, account: &str) -> rightkit_credentials::Result<bool> {
        if account != "db" || fs::remove_file(self.root.join("stall-set")).is_err() {
            return Ok(false);
        }
        fs::write(self.root.join("entered"), b"").map_err(|_| io_error())?;
        wait_for_file(&self.root.join("release")).map_err(|_| io_error())?;
        Ok(true)
    }
}

fn wait_for_file(path: &Path) -> Result<(), String> {
    let deadline = Instant::now() + WAIT;
    while !path.exists() {
        if Instant::now() >= deadline {
            return Err(format!("{} never appeared", path.display()));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

impl Backend for FileBackend {
    fn name(&self) -> &'static str {
        self.label
    }
    fn exclusion_identity(&self) -> Option<String> {
        // The same in every process, as an OS store's identity would be.
        Some("file-test".into())
    }
    fn durable(&self) -> bool {
        true
    }
    fn get(
        &self,
        ns: &Namespace,
        account: &str,
        _mode: Interaction,
    ) -> rightkit_credentials::Result<Option<Secret>> {
        match fs::read(self.item(ns, account)) {
            Ok(bytes) => Ok(Some(Secret::from_bytes(bytes))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(io_error()),
        }
    }
    fn exists(
        &self,
        ns: &Namespace,
        account: &str,
        _mode: Interaction,
    ) -> rightkit_credentials::Result<bool> {
        Ok(self.item(ns, account).exists())
    }
    fn set(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        _mode: Interaction,
    ) -> rightkit_credentials::Result<()> {
        let stalled = self.stall_if_armed(account)?;
        fs::write(self.item(ns, account), secret.expose_bytes()).map_err(|_| io_error())?;
        // Only the STALLED write signals completion. An unconditional signal made the parent's
        // own seed write of `db` create `set-done` up front, so process A saw it at once, exited,
        // and its death (not the lock) let process B in.
        if stalled {
            let _ = fs::write(self.root.join("set-done"), b"");
        }
        Ok(())
    }
    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        _mode: Interaction,
    ) -> rightkit_credentials::Result<bool> {
        use std::io::Write;
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.item(ns, account))
        {
            Ok(mut file) => {
                file.write_all(secret.expose_bytes())
                    .map_err(|_| io_error())?;
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(_) => Err(io_error()),
        }
    }
    fn delete(
        &self,
        ns: &Namespace,
        account: &str,
        _mode: Interaction,
    ) -> rightkit_credentials::Result<bool> {
        match fs::remove_file(self.item(ns, account)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err(io_error()),
        }
    }
}

const LABEL: &str = "file-test";
const ALIAS_LABEL: &str = "file-test-alias";

fn open_store(root: &Path, timeout: Duration, label: &'static str) -> Store {
    let store = Store::open(
        Config::new(Namespace::new(NS).unwrap())
            .backend(Arc::new(FileBackend::new(root, label)))
            .rotation_lock_dir(root.join("locks"))
            .timeout(timeout),
    );
    assert!(
        !store.guard_active(),
        "a credential guard variable replaced the file-backed test store"
    );
    store
}

fn value_of(store: &Store, account: &str) -> Option<String> {
    store
        .get(account)
        .unwrap()
        .map(|s| s.expose_str().unwrap().to_string())
}

fn pending_of(store: &Store, account: &str) -> Option<String> {
    store
        .pending_rotation(account)
        .unwrap()
        .map(|s| s.expose_str().unwrap().to_string())
}

fn in_progress<T: std::fmt::Debug>(result: rightkit_credentials::Result<T>, what: &str) {
    assert!(
        matches!(result, Err(Error::InProgress { .. })),
        "{what} must be refused while another process's write is in flight, got {result:?}"
    );
}

// ---- the child side ----

/// Process A: begins and commits; the primary write stalls and times out.
fn role_a(root: &Path) {
    let store = open_store(root, Duration::from_millis(300), LABEL);
    let new_a = Secret::from_string("new-a");
    assert!(store.begin_rotation("db", &new_a).unwrap());
    let outcome = store.commit_rotation_of("db", &new_a);
    assert!(
        matches!(outcome, Err(Error::Timeout { .. })),
        "A's stalled commit must time out, got {outcome:?}"
    );
    fs::write(root.join("a-timed-out"), b"").unwrap();
    // Stay alive so the abandoned worker (not process death) is what releases the lock.
    wait_for_file(&root.join("set-done")).unwrap();
}

/// Process B while A's write is still stalled: every rotation call is refused, although B's
/// backend is an alias with another name, so the lock cannot be named by the backend name.
fn role_b_blocked(root: &Path) {
    // An ALIAS of A's store (different backend name, same native store and credential).
    let store = open_store(root, Duration::from_millis(400), ALIAS_LABEL);
    assert_eq!(store.diagnostics().backend, ALIAS_LABEL);
    let new_b = Secret::from_string("new-b");
    in_progress(store.abort_rotation("db"), "abort_rotation");
    in_progress(store.begin_rotation("db", &new_b), "begin_rotation");
    in_progress(store.commit_rotation("db"), "commit_rotation");
    in_progress(store.commit_rotation_of("db", &new_b), "commit_rotation_of");
    // Reads are not blocked, and nothing moved.
    assert_eq!(pending_of(&store, "db").as_deref(), Some("new-a"));
    assert_eq!(value_of(&store, "db").as_deref(), Some("old"));
}

/// Process B taking over with a long timeout: it waits for A's worker, then replaces A's
/// rotation with its own. It may start before or after the release; the outcome is the same.
fn role_b_takeover(root: &Path) {
    let store = open_store(root, Duration::from_secs(20), ALIAS_LABEL);
    let new_a = Secret::from_string("new-a");
    let new_b = Secret::from_string("new-b");
    // Takes the lock only after A's abandoned write returned, so that write has landed.
    assert!(store.abort_rotation_of("db", &new_a).unwrap());
    assert_eq!(
        value_of(&store, "db").as_deref(),
        Some("new-a"),
        "B got the lock before A's abandoned write finished"
    );
    assert!(store.begin_rotation("db", &new_b).unwrap());
    assert!(store.commit_rotation_of("db", &new_b).unwrap());
    assert_eq!(value_of(&store, "db").as_deref(), Some("new-b"));
}

/// Not a test of its own: the entry point the parent test re-invokes. Without the role
/// variable it does nothing.
#[test]
fn process_role_entry() {
    let Ok(role) = std::env::var(ROLE_VAR) else {
        return;
    };
    let root = PathBuf::from(std::env::var(ROOT_VAR).unwrap());
    match role.as_str() {
        "a" => role_a(&root),
        "b-blocked" => role_b_blocked(&root),
        "b-takeover" => role_b_takeover(&root),
        other => panic!("unknown role {other}"),
    }
}

// ---- the parent side ----

/// Kills a still-running child when a failing assertion unwinds past it.
struct Proc {
    role: &'static str,
    child: Option<Child>,
}

impl Proc {
    fn spawn(role: &'static str, root: &Path) -> Self {
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process_role_entry",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(ROLE_VAR, role)
            .env(ROOT_VAR, root)
            .env_remove(UNIVERSAL_GUARD_ENV)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        Self {
            role,
            child: Some(child),
        }
    }

    /// The child is still running: its worker, not its death, must be what holds the lock.
    fn assert_alive(&mut self, when: &str) {
        let exited = self.child.as_mut().unwrap().try_wait().unwrap();
        if let Some(status) = exited {
            let role = self.role;
            let report = self.report();
            panic!("process {role} exited ({status}) {when}; its exit would release the lock by itself: {report}");
        }
    }

    /// Wait for `path`; fail at once with the child's output if it exits first.
    fn wait_for(&mut self, path: &Path) {
        let deadline = Instant::now() + WAIT;
        loop {
            if path.exists() {
                return;
            }
            let exited = self.child.as_mut().unwrap().try_wait().unwrap();
            if exited.is_some() || Instant::now() >= deadline {
                let role = self.role;
                let report = self.report();
                panic!(
                    "{} never appeared; process {role}: {report}",
                    path.display()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn report(&mut self) -> String {
        let mut child = self.child.take().unwrap();
        let _ = child.kill();
        let status = child.wait().unwrap();
        let mut out = String::new();
        if let Some(mut stream) = child.stdout.take() {
            let _ = stream.read_to_string(&mut out);
        }
        if let Some(mut stream) = child.stderr.take() {
            let _ = stream.read_to_string(&mut out);
        }
        format!("{status}\n{out}")
    }

    /// The child must exit successfully.
    fn finish(mut self) {
        let child = self.child.take().unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "process {} failed: {}\n{}{}",
            self.role,
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A temp dir closed to group and others. `tempfile` may create it with the umask's default mode
/// (0755 on macOS), which the rotation lock correctly refuses as not private, so the test owner
/// sets 0700 itself, as a real caller must for its lock directory.
fn private_tempdir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    dir
}

#[test]
fn an_abandoned_write_keeps_the_rotation_lock_across_processes_until_its_worker_returns() {
    let dir = private_tempdir();
    let root = dir.path();
    let parent = open_store(root, Duration::from_secs(5), LABEL);
    parent.set_str("db", "old").unwrap();
    fs::write(root.join("stall-set"), b"").unwrap();

    // A begins, then its commit's primary write stalls inside the backend and A times out.
    let mut a = Proc::spawn("a", root);
    a.wait_for(&root.join("entered"));
    a.wait_for(&root.join("a-timed-out"));

    // A's invocation has returned, but its worker is still inside the backend call. B, a
    // separate process, may not abort, re-begin or commit. A must still be running, and its
    // stalled write must not have signalled completion, so only the worker's hold excludes B.
    assert!(
        !root.join("set-done").exists(),
        "set-done must not exist before A's stalled write is released"
    );
    a.assert_alive("before B ran");
    Proc::spawn("b-blocked", root).finish();
    a.assert_alive("while B ran");
    assert!(
        !root.join("set-done").exists(),
        "A's write must still be stalled while B is refused"
    );
    assert_eq!(value_of(&parent, "db").as_deref(), Some("old"));
    assert_eq!(pending_of(&parent, "db").as_deref(), Some("new-a"));

    // B starts waiting for the lock, then A's worker is released. B proceeds only after A's
    // write has landed and ends with the newest value.
    let b = Proc::spawn("b-takeover", root);
    std::thread::sleep(Duration::from_millis(200));
    fs::write(root.join("release"), b"").unwrap();
    a.finish();
    b.finish();

    assert_eq!(value_of(&parent, "db").as_deref(), Some("new-b"));
    assert_eq!(pending_of(&parent, "db"), None);
    // A's rotation is gone for good: a late commit of it is refused and changes nothing.
    let stale = parent.commit_rotation_of("db", &Secret::from_string("new-a"));
    assert!(
        matches!(
            stale,
            Err(Error::Invalid {
                what: "pending rotation",
                ..
            })
        ),
        "{stale:?}"
    );
    assert_eq!(value_of(&parent, "db").as_deref(), Some("new-b"));
}
