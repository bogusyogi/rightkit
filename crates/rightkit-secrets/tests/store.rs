//! Store behaviour on every platform (memory backend and guards) and a real Windows Credential
//! Manager round trip under a unique, cleaned-up test namespace.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use rightkit_secrets::{
    macos_credential_identity, windows_credential_identity, Backend, ConditionalDelete, Config,
    Error, GuardMode, Interaction, MemoryBackend, MigrationReport, Namespace, Secret, Store,
    WindowsTarget, WriteIntent,
};

fn memory_store(service: &str) -> Store {
    Store::open(
        Config::new(Namespace::new(service).unwrap()).backend(Arc::new(MemoryBackend::new())),
    )
}

#[test]
fn set_get_exists_delete_round_trip() {
    let store = memory_store("dev.rightkit.secrets.test.roundtrip");
    assert!(store.get("acct").unwrap().is_none());
    store.set_str("acct", "value-1").unwrap();
    assert!(store.exists("acct").unwrap());
    assert_eq!(
        store.get("acct").unwrap().unwrap().expose_str().unwrap(),
        "value-1"
    );
    assert!(store.delete("acct").unwrap());
    assert!(!store.delete("acct").unwrap());
    assert!(store.get("acct").unwrap().is_none());
}

#[test]
fn add_if_absent_and_get_or_create_key_never_overwrite() {
    let store = memory_store("dev.rightkit.secrets.test.create");
    assert!(store
        .add_if_absent("a", &Secret::from_string("first"))
        .unwrap());
    assert!(!store
        .add_if_absent("a", &Secret::from_string("second"))
        .unwrap());
    assert_eq!(
        store.get("a").unwrap().unwrap().expose_str().unwrap(),
        "first"
    );
    let (k1, created1) = store.get_or_create_key("k", 32).unwrap();
    let (k2, created2) = store.get_or_create_key("k", 32).unwrap();
    assert!(created1 && !created2);
    assert_eq!(k1.len(), 44, "32 random bytes, stored base64");
    assert!(k1.ct_eq(&k2));
}

#[test]
fn a_deny_guard_refuses_without_touching_the_os_store() {
    let guard = format!("RK_SECRETS_TEST_DENY_{}", std::process::id());
    std::env::set_var(&guard, "1");
    let store = Store::open(
        Config::new(Namespace::new("dev.rightkit.secrets.test.guard").unwrap())
            .guard_env(&guard)
            .guard_mode(GuardMode::Deny),
    );
    assert!(store.guard_active());
    assert!(matches!(store.get("x"), Err(Error::Guarded { .. })));
    assert!(matches!(
        store.set_str("x", "v"),
        Err(Error::Guarded { .. })
    ));
    std::env::remove_var(&guard);
}

#[test]
fn secret_debug_output_is_redacted() {
    let s = Secret::from_string("do-not-print-me");
    assert!(!format!("{s:?}").contains("do-not-print-me"));
}

#[cfg(windows)]
#[test]
fn windows_credential_manager_real_round_trip() {
    let service = format!("dev.rightkit.secrets.e2e.{}", std::process::id());
    let store = Store::open(
        Config::new(Namespace::new(&service).unwrap()).backend(rightkit_secrets::system_backend()),
    );
    let _ = store.delete("probe");
    store.set_str("probe", "credential-manager-value").unwrap();
    assert_eq!(
        store.get("probe").unwrap().unwrap().expose_str().unwrap(),
        "credential-manager-value"
    );
    assert!(!store
        .add_if_absent("probe", &Secret::from_string("other"))
        .unwrap());
    store.set_str("probe", "replaced").unwrap();
    assert_eq!(
        store.get("probe").unwrap().unwrap().expose_str().unwrap(),
        "replaced"
    );
    assert!(store.delete("probe").unwrap());
    assert!(store.get("probe").unwrap().is_none());
}

/// Reads back only the first 32 bytes, as a lossy destination would.
struct TruncatingBackend(MemoryBackend);

impl rightkit_secrets::Backend for TruncatingBackend {
    fn name(&self) -> &'static str {
        "truncating-test"
    }
    fn durable(&self) -> bool {
        // A destination that would keep the value, so only its read-back can fail the migration.
        true
    }
    fn get(
        &self,
        ns: &Namespace,
        account: &str,
        mode: rightkit_secrets::Interaction,
    ) -> rightkit_secrets::Result<Option<Secret>> {
        Ok(self
            .0
            .get(ns, account, mode)?
            .map(|s| Secret::from_bytes(&s.expose_bytes()[..32.min(s.len())])))
    }
    fn exists(
        &self,
        ns: &Namespace,
        account: &str,
        mode: rightkit_secrets::Interaction,
    ) -> rightkit_secrets::Result<bool> {
        self.0.exists(ns, account, mode)
    }
    fn set(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: rightkit_secrets::Interaction,
    ) -> rightkit_secrets::Result<()> {
        self.0.set(ns, account, secret, mode)
    }
    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: rightkit_secrets::Interaction,
    ) -> rightkit_secrets::Result<bool> {
        self.0.add_if_absent(ns, account, secret, mode)
    }
    fn delete(
        &self,
        ns: &Namespace,
        account: &str,
        mode: rightkit_secrets::Interaction,
    ) -> rightkit_secrets::Result<bool> {
        self.0.delete(ns, account, mode)
    }
}

#[test]
fn migration_does_not_accept_a_destination_that_returns_a_32_byte_prefix_of_a_288_byte_secret() {
    let from = memory_store("dev.rightkit.secrets.test.mig-len-from");
    let to = Store::open(
        Config::new(Namespace::new("dev.rightkit.secrets.test.mig-len-to").unwrap())
            .backend(Arc::new(TruncatingBackend(MemoryBackend::new()))),
    );
    from.set("acct", &Secret::from_bytes(vec![7u8; 288]))
        .unwrap();
    let report = rightkit_secrets::migrate_between(&from, &to, &["acct"]);
    assert!(report.moved.is_empty(), "{report:?}");
    assert_eq!(report.failed, vec!["acct"]);
    assert_eq!(
        from.get("acct").unwrap().unwrap().len(),
        288,
        "original deleted"
    );
}

#[test]
fn memory_and_unavailable_backends_reject_invalid_accounts_at_the_boundary() {
    use rightkit_secrets::UnavailableBackend;
    let ns = Namespace::new("dev.rightkit.secrets.test.boundary").unwrap();
    let ni = Interaction::NonInteractive;
    let mem = MemoryBackend::new();
    let un = UnavailableBackend("test");
    for b in [&mem as &dyn Backend, &un as &dyn Backend] {
        for acct in ["", "bad\naccount"] {
            assert!(matches!(b.get(&ns, acct, ni), Err(Error::Invalid { .. })));
            assert!(matches!(
                b.exists(&ns, acct, ni),
                Err(Error::Invalid { .. })
            ));
            assert!(matches!(
                b.set(&ns, acct, &Secret::from_string("x"), ni),
                Err(Error::Invalid { .. })
            ));
            assert!(matches!(
                b.add_if_absent(&ns, acct, &Secret::from_string("x"), ni),
                Err(Error::Invalid { .. })
            ));
            assert!(matches!(
                b.delete(&ns, acct, ni),
                Err(Error::Invalid { .. })
            ));
        }
    }
    // Valid account on the unavailable backend stays Unavailable.
    assert!(matches!(un.get(&ns, "ok", ni), Err(Error::Unavailable(_))));
}

// ---------------------------------------------------------------------------------------------
// Second independent read (2026-10-11)
// ---------------------------------------------------------------------------------------------

fn store_for(ns: Namespace, backend: Arc<dyn Backend>) -> Store {
    Store::open(Config::new(ns).backend(backend))
}

// ---- finding 7: a Windows prefix may not truncate or escape the native target name ----

#[test]
fn a_windows_prefix_with_nul_is_refused_by_the_constructor_and_by_every_operation() {
    let base = Namespace::new("dev.rightkit.secrets.test.nul-prefix").unwrap();
    for bad in ["victim\0ignored", "line\nbreak", ""] {
        let style = WindowsTarget::Slash {
            prefix: Some(bad.to_string()),
        };
        // Validated at construction ...
        assert!(
            matches!(
                base.clone().try_windows_target(style.clone()),
                Err(Error::Invalid { .. })
            ),
            "{bad:?}"
        );
        // ... and again at the backend boundary for a namespace built without validation.
        let store = store_for(
            base.clone().windows_target(style),
            Arc::new(MemoryBackend::new()),
        );
        let secret = Secret::from_string("v");
        assert!(matches!(store.get("acct"), Err(Error::Invalid { .. })));
        assert!(matches!(store.exists("acct"), Err(Error::Invalid { .. })));
        assert!(matches!(
            store.set("acct", &secret),
            Err(Error::Invalid { .. })
        ));
        assert!(matches!(
            store.add_if_absent("acct", &secret),
            Err(Error::Invalid { .. })
        ));
        assert!(matches!(store.delete("acct"), Err(Error::Invalid { .. })));
        assert!(matches!(
            store.delete_if_equals("acct", &secret),
            Err(Error::Invalid { .. })
        ));
    }
    // A valid prefix is accepted.
    assert!(base
        .try_windows_target(WindowsTarget::Slash {
            prefix: Some("Valid Prefix".into())
        })
        .is_ok());
}

#[test]
fn backends_called_directly_also_refuse_a_bad_windows_prefix() {
    let ns = Namespace::new("dev.rightkit.secrets.test.nul-direct")
        .unwrap()
        .windows_target(WindowsTarget::Slash {
            prefix: Some("victim\0ignored".into()),
        });
    let ni = Interaction::NonInteractive;
    let mem = MemoryBackend::new();
    assert!(matches!(
        mem.delete(&ns, "acct", ni),
        Err(Error::Invalid { .. })
    ));
    assert!(matches!(
        mem.set(&ns, "acct", &Secret::from_string("v"), ni),
        Err(Error::Invalid { .. })
    ));
}

// ---- finding 6: a timed-out mutation must not overwrite its retry ----

/// The first `set` blocks until released (an OS call stuck behind a prompt); every later call is
/// immediate.
struct GatedBackend {
    inner: MemoryBackend,
    gate: Mutex<Option<mpsc::Receiver<()>>>,
    entered: Mutex<mpsc::Sender<()>>,
    finished: Mutex<mpsc::Sender<()>>,
    log: Mutex<Vec<String>>,
}

impl GatedBackend {
    fn new(
        release: mpsc::Receiver<()>,
        entered: mpsc::Sender<()>,
        finished: mpsc::Sender<()>,
    ) -> Self {
        Self {
            inner: MemoryBackend::new(),
            gate: Mutex::new(Some(release)),
            entered: Mutex::new(entered),
            finished: Mutex::new(finished),
            log: Mutex::new(Vec::new()),
        }
    }
    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

impl Backend for GatedBackend {
    fn name(&self) -> &'static str {
        "gated-test"
    }
    fn get(
        &self,
        ns: &Namespace,
        account: &str,
        mode: Interaction,
    ) -> rightkit_secrets::Result<Option<Secret>> {
        self.inner.get(ns, account, mode)
    }
    fn exists(
        &self,
        ns: &Namespace,
        account: &str,
        mode: Interaction,
    ) -> rightkit_secrets::Result<bool> {
        self.inner.exists(ns, account, mode)
    }
    fn set(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> rightkit_secrets::Result<()> {
        let gate = self.gate.lock().unwrap().take();
        let held = gate.is_some();
        if let Some(release) = gate {
            self.entered.lock().unwrap().send(()).unwrap();
            let _ = release.recv();
        }
        self.log
            .lock()
            .unwrap()
            .push(format!("set:{}", secret.expose_str().unwrap()));
        let result = self.inner.set(ns, account, secret, mode);
        if held {
            let _ = self.finished.lock().unwrap().send(());
        }
        result
    }
    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> rightkit_secrets::Result<bool> {
        self.inner.add_if_absent(ns, account, secret, mode)
    }
    fn delete(
        &self,
        ns: &Namespace,
        account: &str,
        mode: Interaction,
    ) -> rightkit_secrets::Result<bool> {
        self.inner.delete(ns, account, mode)
    }
}

struct Gated {
    store: Store,
    backend: Arc<GatedBackend>,
    release: mpsc::Sender<()>,
    entered: mpsc::Receiver<()>,
    finished: mpsc::Receiver<()>,
}

fn gated_store(service: &str, timeout: Duration) -> Gated {
    let (release, release_rx) = mpsc::channel();
    let (entered_tx, entered) = mpsc::channel();
    let (finished_tx, finished) = mpsc::channel();
    let backend = Arc::new(GatedBackend::new(release_rx, entered_tx, finished_tx));
    let store = Store::open(
        Config::new(Namespace::new(service).unwrap())
            .backend(backend.clone())
            .timeout(timeout),
    );
    Gated {
        store,
        backend,
        release,
        entered,
        finished,
    }
}

#[test]
fn a_retry_after_a_timed_out_mutation_is_refused_and_cannot_be_overwritten_by_it() {
    let Gated {
        store,
        backend,
        release,
        entered,
        finished,
    } = gated_store(
        "dev.rightkit.secrets.test.inprogress",
        Duration::from_millis(250),
    );

    // A stalls inside the OS call and the caller gives up.
    assert!(matches!(
        store.set_str("acct", "A"),
        Err(Error::Timeout { .. })
    ));
    entered.recv_timeout(Duration::from_secs(5)).unwrap();

    // A different credential is not held up by it.
    store.set_str("other", "O").unwrap();

    // The retry must not run beside the stalled call, which would then overwrite it.
    let retry = store.set_str("acct", "B");
    assert!(matches!(retry, Err(Error::InProgress { .. })), "{retry:?}");
    assert!(
        !backend.log().contains(&"set:B".to_string()),
        "the retry reached the backend while the earlier call was still running"
    );
    assert!(Error::InProgress { op: "set" }.is_unavailable_or_blocked());

    // The stalled call finishes; only then can the retry go through.
    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut attempts = 0;
    loop {
        match store.set_str("acct", "B") {
            Ok(()) => break,
            // The stalled call has returned but its worker may not have released the target yet.
            Err(Error::InProgress { .. }) if attempts < 500 => {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("retry failed: {error:?}"),
        }
    }
    assert_eq!(
        store.get("acct").unwrap().unwrap().expose_str().unwrap(),
        "B",
        "the stalled call overwrote its retry"
    );
    assert_eq!(backend.log(), ["set:O", "set:A", "set:B"]);
}

#[test]
fn a_stalled_mutation_blocks_every_store_that_shares_its_native_target() {
    let Gated {
        store,
        release,
        entered,
        finished,
        ..
    } = gated_store(
        "dev.rightkit.secrets.test.shared-a",
        Duration::from_millis(250),
    );
    let shared = |service: &str| {
        Namespace::new(service)
            .unwrap()
            .windows_target(WindowsTarget::Slash {
                prefix: Some("Shared".into()),
            })
    };
    let a = store.with_namespace(shared("dev.rightkit.secrets.test.shared-a"));
    let b = store.with_namespace(shared("dev.rightkit.secrets.test.shared-b"));

    assert!(matches!(a.set_str("acct", "A"), Err(Error::Timeout { .. })));
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    // Same backend, same Windows prefix, same account: the same native entry.
    assert!(matches!(
        b.set_str("acct", "B"),
        Err(Error::InProgress { .. })
    ));
    assert!(matches!(b.delete("acct"), Err(Error::InProgress { .. })));
    // Reads are not mutations and are never refused for this reason.
    assert!(b.get("acct").is_ok());

    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(5)).unwrap();
}

/// A second, separately allocated handle on one underlying store, standing in for a reopened
/// `Store` over a fresh `system_backend()` instance. Both report the same exclusion identity.
struct SameStore(Arc<GatedBackend>);

impl Backend for SameStore {
    fn name(&self) -> &'static str {
        "same-store-test"
    }
    fn exclusion_identity(&self) -> Option<String> {
        Some("test:one-shared-os-store".to_string())
    }
    fn get(
        &self,
        ns: &Namespace,
        account: &str,
        mode: Interaction,
    ) -> rightkit_secrets::Result<Option<Secret>> {
        self.0.get(ns, account, mode)
    }
    fn exists(
        &self,
        ns: &Namespace,
        account: &str,
        mode: Interaction,
    ) -> rightkit_secrets::Result<bool> {
        self.0.exists(ns, account, mode)
    }
    fn set(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> rightkit_secrets::Result<()> {
        self.0.set(ns, account, secret, mode)
    }
    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> rightkit_secrets::Result<bool> {
        self.0.add_if_absent(ns, account, secret, mode)
    }
    fn delete(
        &self,
        ns: &Namespace,
        account: &str,
        mode: Interaction,
    ) -> rightkit_secrets::Result<bool> {
        self.0.delete(ns, account, mode)
    }
}

/// Audit finding: the exclusion key was the backend's `Arc` address, so a reopened `Store` (a
/// fresh backend instance over the same OS store) bypassed the timeout protection.
#[test]
fn a_reopened_store_over_the_same_os_store_is_also_refused_while_a_timed_out_call_runs() {
    let (release, release_rx) = mpsc::channel();
    let (entered_tx, entered) = mpsc::channel();
    let (finished_tx, finished) = mpsc::channel();
    let gated = Arc::new(GatedBackend::new(release_rx, entered_tx, finished_tx));
    let open = || {
        Store::open(
            Config::new(Namespace::new("dev.rightkit.secrets.test.reopened").unwrap())
                .backend(Arc::new(SameStore(gated.clone())))
                .timeout(Duration::from_millis(250)),
        )
    };
    let first = open();
    assert!(matches!(
        first.set_str("acct", "A"),
        Err(Error::Timeout { .. })
    ));
    entered.recv_timeout(Duration::from_secs(5)).unwrap();

    let reopened = open();
    let retry = reopened.set_str("acct", "B");
    assert!(matches!(retry, Err(Error::InProgress { .. })), "{retry:?}");
    assert!(
        !gated.log().contains(&"set:B".to_string()),
        "the reopened store reached the backend beside the stalled call"
    );

    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(5)).unwrap();
}

/// Records how many mutations ran at the same moment.
struct OverlapBackend {
    inner: MemoryBackend,
    running: AtomicUsize,
    most: AtomicUsize,
}

impl OverlapBackend {
    fn mutate<T>(&self, run: impl FnOnce() -> T) -> T {
        let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(15));
        let result = run();
        self.running.fetch_sub(1, Ordering::SeqCst);
        result
    }
}

impl Backend for OverlapBackend {
    fn name(&self) -> &'static str {
        "overlap-test"
    }
    fn get(
        &self,
        ns: &Namespace,
        account: &str,
        mode: Interaction,
    ) -> rightkit_secrets::Result<Option<Secret>> {
        self.inner.get(ns, account, mode)
    }
    fn exists(
        &self,
        ns: &Namespace,
        account: &str,
        mode: Interaction,
    ) -> rightkit_secrets::Result<bool> {
        self.inner.exists(ns, account, mode)
    }
    fn set(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> rightkit_secrets::Result<()> {
        self.mutate(|| self.inner.set(ns, account, secret, mode))
    }
    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> rightkit_secrets::Result<bool> {
        self.mutate(|| self.inner.add_if_absent(ns, account, secret, mode))
    }
    fn delete(
        &self,
        ns: &Namespace,
        account: &str,
        mode: Interaction,
    ) -> rightkit_secrets::Result<bool> {
        self.mutate(|| self.inner.delete(ns, account, mode))
    }
}

#[test]
fn mutations_of_one_credential_run_one_at_a_time() {
    let backend = Arc::new(OverlapBackend {
        inner: MemoryBackend::new(),
        running: AtomicUsize::new(0),
        most: AtomicUsize::new(0),
    });
    let store = store_for(
        Namespace::new("dev.rightkit.secrets.test.overlap").unwrap(),
        backend.clone(),
    );
    std::thread::scope(|scope| {
        for n in 0..6 {
            let store = &store;
            scope.spawn(move || match n % 3 {
                0 => store.set_str("acct", &format!("v{n}")).unwrap(),
                1 => {
                    let _ = store
                        .add_if_absent("acct", &Secret::from_string(format!("a{n}")))
                        .unwrap();
                }
                _ => {
                    let _ = store.delete("acct").unwrap();
                }
            });
        }
    });
    assert_eq!(
        backend.most.load(Ordering::SeqCst),
        1,
        "two mutations of the same credential ran at once"
    );
}

// ---- finding 5: migration never deletes a source it did not copy ----

/// A source whose value another writer replaces right after the migration reads it.
struct ReplacedAfterRead {
    inner: MemoryBackend,
    replacement: Secret,
    armed: AtomicBool,
    conditional_delete: bool,
}

impl Backend for ReplacedAfterRead {
    fn name(&self) -> &'static str {
        "replaced-after-read-test"
    }
    fn get(
        &self,
        ns: &Namespace,
        account: &str,
        mode: Interaction,
    ) -> rightkit_secrets::Result<Option<Secret>> {
        let read = self.inner.get(ns, account, mode)?;
        if self.armed.swap(false, Ordering::SeqCst) {
            self.inner.set(ns, account, &self.replacement, mode)?;
        }
        Ok(read)
    }
    fn exists(
        &self,
        ns: &Namespace,
        account: &str,
        mode: Interaction,
    ) -> rightkit_secrets::Result<bool> {
        self.inner.exists(ns, account, mode)
    }
    fn set(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> rightkit_secrets::Result<()> {
        self.inner.set(ns, account, secret, mode)
    }
    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> rightkit_secrets::Result<bool> {
        self.inner.add_if_absent(ns, account, secret, mode)
    }
    fn delete(
        &self,
        ns: &Namespace,
        account: &str,
        mode: Interaction,
    ) -> rightkit_secrets::Result<bool> {
        self.inner.delete(ns, account, mode)
    }
    fn delete_if_equals(
        &self,
        ns: &Namespace,
        account: &str,
        expected: &Secret,
        mode: Interaction,
    ) -> rightkit_secrets::Result<ConditionalDelete> {
        if self.conditional_delete {
            self.inner.delete_if_equals(ns, account, expected, mode)
        } else {
            // A store with no atomic compare-and-delete keeps the trait default.
            Ok(ConditionalDelete::Unsupported)
        }
    }
}

struct Migrated {
    report: MigrationReport,
    source: Option<String>,
    destination: Option<String>,
}

fn migrate_one(conditional_delete: bool, replace_after_read: bool) -> Migrated {
    let ni = Interaction::NonInteractive;
    let source_ns = Namespace::new("dev.rightkit.secrets.test.mig-race-from").unwrap();
    let inner = MemoryBackend::new();
    inner
        .set(&source_ns, "acct", &Secret::from_string("older"), ni)
        .unwrap();
    let from = store_for(
        source_ns,
        Arc::new(ReplacedAfterRead {
            inner,
            replacement: Secret::from_string("newer"),
            armed: AtomicBool::new(replace_after_read),
            conditional_delete,
        }),
    );
    let to = store_for(
        Namespace::new("dev.rightkit.secrets.test.mig-race-to").unwrap(),
        Arc::new(MemoryBackend::simulating_durable()),
    );
    let report = rightkit_secrets::migrate_between(&from, &to, &["acct"]);
    let text = |s: Option<Secret>| s.map(|s| s.expose_str().unwrap().to_string());
    Migrated {
        report,
        source: text(from.get("acct").unwrap()),
        destination: text(to.get("acct").unwrap()),
    }
}

#[test]
fn migration_keeps_a_source_that_a_writer_replaced_after_it_was_read() {
    let m = migrate_one(true, true);
    assert!(m.report.moved.is_empty(), "{:?}", m.report);
    assert_eq!(m.report.retained, vec!["acct"]);
    assert_eq!(
        m.source.as_deref(),
        Some("newer"),
        "the newer value was deleted"
    );
    assert_eq!(m.destination.as_deref(), Some("older"));
}

#[test]
fn migration_is_copy_only_when_the_source_cannot_delete_conditionally() {
    let m = migrate_one(false, false);
    assert!(m.report.moved.is_empty(), "{:?}", m.report);
    assert_eq!(m.report.retained, vec!["acct"]);
    assert_eq!(
        m.source.as_deref(),
        Some("older"),
        "the source must be kept"
    );
    assert_eq!(m.destination.as_deref(), Some("older"));
}

#[test]
fn migration_removes_the_source_when_the_backend_can_delete_conditionally() {
    let m = migrate_one(true, false);
    assert_eq!(m.report.moved, vec!["acct"]);
    assert!(m.report.retained.is_empty() && m.report.failed.is_empty());
    assert_eq!(m.source, None);
    assert_eq!(m.destination.as_deref(), Some("older"));
}

#[test]
fn the_default_conditional_delete_removes_nothing() {
    // A backend that implements only the required methods (the older trait) is "unsupported".
    struct Minimal(MemoryBackend);
    impl Backend for Minimal {
        fn name(&self) -> &'static str {
            "minimal-test"
        }
        fn get(
            &self,
            ns: &Namespace,
            a: &str,
            m: Interaction,
        ) -> rightkit_secrets::Result<Option<Secret>> {
            self.0.get(ns, a, m)
        }
        fn exists(
            &self,
            ns: &Namespace,
            a: &str,
            m: Interaction,
        ) -> rightkit_secrets::Result<bool> {
            self.0.exists(ns, a, m)
        }
        fn set(
            &self,
            ns: &Namespace,
            a: &str,
            s: &Secret,
            m: Interaction,
        ) -> rightkit_secrets::Result<()> {
            self.0.set(ns, a, s, m)
        }
        fn add_if_absent(
            &self,
            ns: &Namespace,
            a: &str,
            s: &Secret,
            m: Interaction,
        ) -> rightkit_secrets::Result<bool> {
            self.0.add_if_absent(ns, a, s, m)
        }
        fn delete(
            &self,
            ns: &Namespace,
            a: &str,
            m: Interaction,
        ) -> rightkit_secrets::Result<bool> {
            self.0.delete(ns, a, m)
        }
    }
    let store = store_for(
        Namespace::new("dev.rightkit.secrets.test.default-cond").unwrap(),
        Arc::new(Minimal(MemoryBackend::new())),
    );
    store.set_str("acct", "v").unwrap();
    assert_eq!(
        store
            .delete_if_equals("acct", &Secret::from_string("v"))
            .unwrap(),
        ConditionalDelete::Unsupported
    );
    assert!(store.exists("acct").unwrap());
}

#[test]
fn memory_conditional_delete_compares_and_removes_in_one_step() {
    let store = memory_store("dev.rightkit.secrets.test.cond-memory");
    assert_eq!(
        store
            .delete_if_equals("acct", &Secret::from_string("v"))
            .unwrap(),
        ConditionalDelete::Absent
    );
    store.set_str("acct", "newer").unwrap();
    assert_eq!(
        store
            .delete_if_equals("acct", &Secret::from_string("older"))
            .unwrap(),
        ConditionalDelete::Changed
    );
    assert!(store.exists("acct").unwrap());
    assert_eq!(
        store
            .delete_if_equals("acct", &Secret::from_string("newer"))
            .unwrap(),
        ConditionalDelete::Deleted
    );
    assert!(!store.exists("acct").unwrap());
}

// ---- fourth read, finding 10: the exclusion key is the BACKEND's native credential ----

/// A backend that addresses by Keychain service and account, as the macOS one does, over the
/// gated fake.
struct MacNamed(Arc<GatedBackend>);

impl Backend for MacNamed {
    fn name(&self) -> &'static str {
        "mac-named-test"
    }
    fn exclusion_identity(&self) -> Option<String> {
        Some("mac-named-test:one-keychain".into())
    }
    fn credential_identity(
        &self,
        ns: &Namespace,
        account: &str,
    ) -> rightkit_secrets::Result<String> {
        macos_credential_identity(ns, account)
    }
    fn get(
        &self,
        ns: &Namespace,
        account: &str,
        mode: Interaction,
    ) -> rightkit_secrets::Result<Option<Secret>> {
        self.0.get(ns, account, mode)
    }
    fn exists(
        &self,
        ns: &Namespace,
        account: &str,
        mode: Interaction,
    ) -> rightkit_secrets::Result<bool> {
        self.0.exists(ns, account, mode)
    }
    fn set(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> rightkit_secrets::Result<()> {
        self.0.set(ns, account, secret, mode)
    }
    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> rightkit_secrets::Result<bool> {
        self.0.add_if_absent(ns, account, secret, mode)
    }
    fn delete(
        &self,
        ns: &Namespace,
        account: &str,
        mode: Interaction,
    ) -> rightkit_secrets::Result<bool> {
        self.0.delete(ns, account, mode)
    }
}

#[test]
fn stores_that_differ_only_in_windows_prefix_exclude_each_other_on_a_service_account_backend() {
    let (release, release_rx) = mpsc::channel();
    let (entered_tx, entered) = mpsc::channel();
    let (finished_tx, finished) = mpsc::channel();
    let gated = Arc::new(GatedBackend::new(release_rx, entered_tx, finished_tx));
    let mac: Arc<dyn Backend> = Arc::new(MacNamed(gated.clone()));
    let open = |prefix: &str| {
        Store::open(
            Config::new(
                Namespace::new("dev.rightkit.secrets.test.mac-alias")
                    .unwrap()
                    .windows_target(WindowsTarget::Slash {
                        prefix: Some(prefix.into()),
                    }),
            )
            .backend(mac.clone())
            .timeout(Duration::from_millis(250)),
        )
    };
    let (a, b) = (open("PrefixA"), open("PrefixB"));

    // A stalls in the native call and its caller gives up.
    assert!(matches!(a.set_str("acct", "A"), Err(Error::Timeout { .. })));
    entered.recv_timeout(Duration::from_secs(5)).unwrap();

    // B addresses the SAME Keychain item through a different Windows prefix: it must be refused,
    // not run beside A and be overwritten when A resumes.
    assert!(matches!(
        b.set_str("acct", "B"),
        Err(Error::InProgress { .. })
    ));
    assert!(matches!(b.delete("acct"), Err(Error::InProgress { .. })));
    assert!(matches!(
        b.put("acct", &Secret::from_string("B"), WriteIntent::CreateOnly),
        Err(Error::InProgress { .. })
    ));
    assert!(
        !gated.log().contains(&"set:B".to_string()),
        "B reached the backend while A was still running"
    );
    // A different account is a different Keychain item and is not held up.
    b.set_str("other", "O").unwrap();

    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut attempts = 0;
    loop {
        match b.set_str("acct", "B") {
            Ok(()) => break,
            Err(Error::InProgress { .. }) if attempts < 500 => {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("retry failed: {error:?}"),
        }
    }
    assert_eq!(
        a.get("acct").unwrap().unwrap().expose_str().unwrap(),
        "B",
        "the stalled call overwrote the other store's write"
    );
}

#[test]
fn the_native_naming_functions_are_pinned_to_exact_strings() {
    let ns = Namespace::new("Dev.App")
        .unwrap()
        .windows_target(WindowsTarget::Slash {
            prefix: Some("Shared".into()),
        });
    assert_eq!(
        macos_credential_identity(&ns, "Key").unwrap(),
        "macos:dev.app\u{1f}key"
    );
    assert_eq!(
        windows_credential_identity(&ns, "Key").unwrap(),
        "windows:shared/key"
    );
    // The built-in memory backend names credentials by its own exact key.
    assert_eq!(
        MemoryBackend::new()
            .credential_identity(&ns, "Key")
            .unwrap(),
        "memory:Dev.App\u{1f}Key"
    );
}

// ---- fifth read, finding 11: the store a mutation locks is the store it writes ----

/// Two stores and a "default" that selects one of them, like the macOS user-default keychain: a
/// call that resolves the default afresh can land on a different store than an earlier call did.
/// The first `set` stalls, before it resolves anything, until released (an OS call stuck behind
/// a prompt, or a worker that has not been scheduled yet).
struct TwoStores {
    stores: [MemoryBackend; 2],
    default: AtomicUsize,
    /// `pinned()` fails: the default cannot be resolved.
    unresolvable: AtomicBool,
    /// A pinned view reports no `exclusion_identity`.
    anonymous: AtomicBool,
    gate: Mutex<Option<mpsc::Receiver<()>>>,
    entered: Mutex<mpsc::Sender<()>>,
    finished: Mutex<mpsc::Sender<()>>,
}

impl TwoStores {
    fn value(&self, index: usize, ns: &Namespace, account: &str) -> Option<String> {
        self.stores[index]
            .get(ns, account, Interaction::NonInteractive)
            .unwrap()
            .map(|s| s.expose_str().unwrap().to_string())
    }
    /// Stall the first caller until released; `true` for that caller.
    fn stall(&self) -> bool {
        let Some(release) = self.gate.lock().unwrap().take() else {
            return false;
        };
        self.entered.lock().unwrap().send(()).unwrap();
        let _ = release.recv();
        true
    }
}

/// A view of [`TwoStores`]. `fixed: None` resolves the default at every call; `Some(i)` is bound
/// to store `i`, which is what `pinned()` returns.
struct View {
    shared: Arc<TwoStores>,
    fixed: Option<usize>,
}

impl View {
    fn index(&self) -> usize {
        self.fixed
            .unwrap_or_else(|| self.shared.default.load(Ordering::SeqCst))
    }
}

impl Backend for View {
    fn name(&self) -> &'static str {
        "two-stores-test"
    }
    fn exclusion_identity(&self) -> Option<String> {
        // Only a view bound to one store can name it.
        if self.shared.anonymous.load(Ordering::SeqCst) {
            return None;
        }
        self.fixed.map(|i| format!("two-stores-test:{i}"))
    }
    fn pinned(&self) -> rightkit_secrets::Result<Option<Arc<dyn Backend>>> {
        if self.fixed.is_some() {
            return Ok(None);
        }
        if self.shared.unresolvable.load(Ordering::SeqCst) {
            return Err(Error::Unavailable("no default store".into()));
        }
        // The default as it is right now, decided once.
        let index = self.shared.default.load(Ordering::SeqCst);
        Ok(Some(Arc::new(View {
            shared: self.shared.clone(),
            fixed: Some(index),
        })))
    }
    fn get(
        &self,
        ns: &Namespace,
        account: &str,
        mode: Interaction,
    ) -> rightkit_secrets::Result<Option<Secret>> {
        self.shared.stores[self.index()].get(ns, account, mode)
    }
    fn exists(
        &self,
        ns: &Namespace,
        account: &str,
        mode: Interaction,
    ) -> rightkit_secrets::Result<bool> {
        self.shared.stores[self.index()].exists(ns, account, mode)
    }
    fn set(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> rightkit_secrets::Result<()> {
        let stalled = self.shared.stall();
        // An unbound view resolves the default only now, after the stall.
        let result = self.shared.stores[self.index()].set(ns, account, secret, mode);
        if stalled {
            let _ = self.shared.finished.lock().unwrap().send(());
        }
        result
    }
    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> rightkit_secrets::Result<bool> {
        self.shared.stores[self.index()].add_if_absent(ns, account, secret, mode)
    }
    fn delete(
        &self,
        ns: &Namespace,
        account: &str,
        mode: Interaction,
    ) -> rightkit_secrets::Result<bool> {
        self.shared.stores[self.index()].delete(ns, account, mode)
    }
}

struct TwoStoresRig {
    shared: Arc<TwoStores>,
    store: Store,
    ns: Namespace,
    release: mpsc::Sender<()>,
    entered: mpsc::Receiver<()>,
    finished: mpsc::Receiver<()>,
}

fn two_stores_rig(service: &str) -> TwoStoresRig {
    let (release, release_rx) = mpsc::channel();
    let (entered_tx, entered) = mpsc::channel();
    let (finished_tx, finished) = mpsc::channel();
    let shared = Arc::new(TwoStores {
        stores: [MemoryBackend::new(), MemoryBackend::new()],
        default: AtomicUsize::new(0),
        unresolvable: AtomicBool::new(false),
        anonymous: AtomicBool::new(false),
        gate: Mutex::new(Some(release_rx)),
        entered: Mutex::new(entered_tx),
        finished: Mutex::new(finished_tx),
    });
    let ns = Namespace::new(service).unwrap();
    let store = Store::open(
        Config::new(ns.clone())
            .backend(Arc::new(View {
                shared: shared.clone(),
                fixed: None,
            }))
            .timeout(Duration::from_millis(250)),
    );
    TwoStoresRig {
        shared,
        store,
        ns,
        release,
        entered,
        finished,
    }
}

#[test]
fn a_late_write_lands_on_the_store_it_was_locked_for_even_if_the_default_changed() {
    let rig = two_stores_rig("dev.rightkit.secrets.test.switch-default");

    // The default is store 0. A is admitted (store 0 is locked for it), stalls, and gives up.
    assert!(matches!(
        rig.store.set_str("acct", "A"),
        Err(Error::Timeout { .. })
    ));
    rig.entered.recv_timeout(Duration::from_secs(5)).unwrap();
    // While the default is still store 0 a retry is refused: that is the store A holds.
    assert!(matches!(
        rig.store.set_str("acct", "retry"),
        Err(Error::InProgress { .. })
    ));

    // The default switches to store 1. A mutation there is a different store and is not held up.
    rig.shared.default.store(1, Ordering::SeqCst);
    rig.store.set_str("acct", "B").unwrap();
    assert_eq!(rig.shared.value(1, &rig.ns, "acct").as_deref(), Some("B"));

    // A resumes. It must finish on store 0, the store it was locked for, and leave B alone.
    rig.release.send(()).unwrap();
    rig.finished.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        rig.shared.value(0, &rig.ns, "acct").as_deref(),
        Some("A"),
        "the stalled write did not finish on the store it was locked for"
    );
    assert_eq!(
        rig.shared.value(1, &rig.ns, "acct").as_deref(),
        Some("B"),
        "the stalled write landed on the new default and overwrote B"
    );
}

#[test]
fn a_mutation_whose_store_cannot_be_decided_is_refused_before_it_locks_or_writes_anything() {
    let rig = two_stores_rig("dev.rightkit.secrets.test.unresolvable-default");
    rig.shared.unresolvable.store(true, Ordering::SeqCst);
    let attempts = [
        rig.store.set_str("acct", "A"),
        rig.store.delete("acct").map(|_| ()),
        rig.store
            .add_if_absent("acct", &Secret::from_string("A"))
            .map(|_| ()),
        rig.store
            .delete_if_equals("acct", &Secret::from_string("A"))
            .map(|_| ()),
    ];
    for attempt in attempts {
        assert!(matches!(attempt, Err(Error::Unavailable(_))), "{attempt:?}");
    }
    assert_eq!(rig.shared.value(0, &rig.ns, "acct"), None);
    assert_eq!(rig.shared.value(1, &rig.ns, "acct"), None);
    // A read resolves the default inside its own call and is not a mutation.
    assert!(rig.store.get("acct").unwrap().is_none());

    // Nothing was claimed: once the default resolves again the same call goes through at once
    // (the stall is released up front), and is not refused as `InProgress`.
    rig.shared.unresolvable.store(false, Ordering::SeqCst);
    rig.release.send(()).unwrap();
    rig.store.set_str("acct", "A").unwrap();
    assert_eq!(rig.shared.value(0, &rig.ns, "acct").as_deref(), Some("A"));
}

#[test]
fn a_pinned_backend_that_cannot_name_its_store_is_refused() {
    let rig = two_stores_rig("dev.rightkit.secrets.test.anonymous-pin");
    rig.shared.anonymous.store(true, Ordering::SeqCst);
    let attempt = rig.store.set_str("acct", "A");
    assert!(matches!(attempt, Err(Error::Unavailable(_))), "{attempt:?}");
    assert_eq!(rig.shared.value(0, &rig.ns, "acct"), None);
    assert_eq!(rig.shared.value(1, &rig.ns, "acct"), None);
}

// ---- fourth read, finding 8: destructive migration needs a durable destination ----

fn durable_source(service: &str) -> Store {
    let from = store_for(
        Namespace::new(service).unwrap(),
        Arc::new(MemoryBackend::simulating_durable()),
    );
    from.set_str("acct", "persistent-value").unwrap();
    from
}

#[test]
fn migrating_into_a_guarded_destination_never_removes_the_persistent_source() {
    for mode in [GuardMode::Memory, GuardMode::Deny] {
        let from = durable_source("dev.rightkit.secrets.test.guarded-dest-from");
        let to = Store::open(
            Config::new(Namespace::new("dev.rightkit.secrets.test.guarded-dest-to").unwrap())
                .guard_when(true, mode),
        );
        assert!(!to.is_durable());
        let report = rightkit_secrets::migrate_between(&from, &to, &["acct"]);
        assert_eq!(report.refused, vec!["acct"], "{mode:?}: {report:?}");
        assert!(
            report.moved.is_empty() && report.retained.is_empty() && report.failed.is_empty(),
            "{mode:?}: {report:?}"
        );
        assert_eq!(
            from.get("acct").unwrap().unwrap().expose_str().unwrap(),
            "persistent-value",
            "{mode:?}: the only persistent copy was removed"
        );
        if mode == GuardMode::Memory {
            assert!(to.get("acct").unwrap().is_none(), "nothing was copied");
        }
    }
}

#[test]
fn migrating_into_a_backend_that_does_not_report_durability_removes_nothing() {
    let from = durable_source("dev.rightkit.secrets.test.nondurable-dest-from");
    let to = memory_store("dev.rightkit.secrets.test.nondurable-dest-to");
    assert!(!to.is_durable());
    let report = rightkit_secrets::migrate_between(&from, &to, &["acct", "other"]);
    assert_eq!(report.refused, vec!["acct", "other"]);
    assert!(from.exists("acct").unwrap() && !to.exists("acct").unwrap());
}

#[test]
fn a_refused_legacy_migration_writes_no_marker_and_reports_the_refusal() {
    let from = durable_source("dev.rightkit.secrets.test.marker-refused-from");
    let to = memory_store("dev.rightkit.secrets.test.marker-refused-to");
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("migrated.json");
    // The legacy namespace is read through `to`'s own backend, which is not durable, so the
    // account is refused before anything is read or copied.
    let report = to
        .migrate_legacy_namespaces(&[from.namespace().clone()], &["acct"], Some(&marker))
        .unwrap()
        .unwrap();
    assert_eq!(report.refused, vec!["acct"]);
    assert!(!marker.exists(), "a refused migration must be retried");
}

#[test]
fn a_durable_destination_still_moves_the_source() {
    let from = durable_source("dev.rightkit.secrets.test.durable-dest-from");
    let to = store_for(
        Namespace::new("dev.rightkit.secrets.test.durable-dest-to").unwrap(),
        Arc::new(MemoryBackend::simulating_durable()),
    );
    assert!(to.is_durable());
    let report = rightkit_secrets::migrate_between(&from, &to, &["acct"]);
    assert_eq!(report.moved, vec!["acct"], "{report:?}");
    assert!(report.refused.is_empty());
    assert!(from.get("acct").unwrap().is_none());
    assert!(to.exists("acct").unwrap());
}

#[test]
fn put_create_only_never_replaces_and_replace_always_does() {
    let store = memory_store("dev.rightkit.secrets.test.put");
    let v = |s: &str| Secret::from_string(s);
    assert!(store
        .put("a", &v("first"), WriteIntent::CreateOnly)
        .unwrap());
    assert!(!store
        .put("a", &v("second"), WriteIntent::CreateOnly)
        .unwrap());
    assert_eq!(
        store.get("a").unwrap().unwrap().expose_str().unwrap(),
        "first"
    );
    assert!(store.put("a", &v("third"), WriteIntent::Replace).unwrap());
    assert_eq!(
        store.get("a").unwrap().unwrap().expose_str().unwrap(),
        "third"
    );
    for intent in [WriteIntent::CreateOnly, WriteIntent::Replace] {
        assert!(matches!(
            store.put("a", &Secret::from_bytes(Vec::new()), intent),
            Err(Error::Invalid { .. })
        ));
    }
}

/// `src/migrate.rs` without its test module, for the two source guards below. Line endings are
/// normalised first: a Windows checkout has CRLF, and the test-module marker would then not be
/// found, so the guards would read the tests as production code. The marker must be there.
fn migrate_production_source() -> String {
    const TESTS: &str = "#[cfg(test)]\nmod tests";
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/migrate.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let (production, _tests) = src
        .split_once(TESTS)
        .expect("migrate.rs ends with its test module");
    production.to_string()
}

/// Guard for `migrate.rs` ONLY, not for the crate: the migration module reaches the store's
/// writers only through the guarded primitives. A new `.set(`, `.delete(` or bare
/// `.add_if_absent(` there fails this test. (`apple_helper::AppleHelper` is a separate client of
/// the Swift helper and is outside `Store`.)
#[test]
fn migration_code_reaches_the_store_only_through_the_guarded_primitives() {
    let production = migrate_production_source();
    for banned in [".set(", ".set_str(", ".delete(", ".add_if_absent("] {
        assert!(
            !production.contains(banned),
            "migrate.rs calls `{banned}` outside Store::put / remove_copied_source"
        );
    }
    assert_eq!(
        production.matches(".delete_if_equals(").count(),
        1,
        "source removal must stay in remove_copied_source"
    );
    assert_eq!(production.matches("remove_copied_source(").count(), 2); // definition + one call
}

/// The plaintext migration only COPIES: retiring a legacy file (wipe, move aside, delete) was
/// removed on purpose, because no step of it can be made safe against the file's writers. A
/// file-mutating call reappearing in `migrate.rs` fails this test.
#[test]
fn the_plaintext_migration_has_no_code_that_modifies_or_removes_a_file() {
    let production = migrate_production_source();
    for banned in [
        "remove_file",
        "OpenOptions",
        "rename(",
        "hard_link",
        "set_len",
        "File::create",
        ".write_all(",
    ] {
        assert!(
            !production.contains(banned),
            "migrate.rs contains `{banned}`: the legacy file must only be read"
        );
    }
}

// ---- fourth read, finding 9: import_env cannot overwrite a concurrent save ----

/// A store where the credential is saved by someone else in the instant between the importer's
/// presence view and its write: `exists` still says "absent", and the user's value B lands first.
struct SavedBetweenCheckAndWrite {
    inner: MemoryBackend,
    concurrent: Secret,
}

impl Backend for SavedBetweenCheckAndWrite {
    fn name(&self) -> &'static str {
        "saved-between-test"
    }
    fn get(
        &self,
        ns: &Namespace,
        account: &str,
        mode: Interaction,
    ) -> rightkit_secrets::Result<Option<Secret>> {
        self.inner.get(ns, account, mode)
    }
    fn exists(&self, _: &Namespace, _: &str, _: Interaction) -> rightkit_secrets::Result<bool> {
        Ok(false)
    }
    fn set(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> rightkit_secrets::Result<()> {
        // An unconditional write lands on top of the user's value.
        self.inner.set(ns, account, &self.concurrent, mode)?;
        self.inner.set(ns, account, secret, mode)
    }
    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> rightkit_secrets::Result<bool> {
        self.inner.set(ns, account, &self.concurrent, mode)?;
        self.inner.add_if_absent(ns, account, secret, mode)
    }
    fn delete(
        &self,
        ns: &Namespace,
        account: &str,
        mode: Interaction,
    ) -> rightkit_secrets::Result<bool> {
        self.inner.delete(ns, account, mode)
    }
}

#[test]
fn import_env_without_overwrite_keeps_a_credential_saved_just_before_its_write() {
    let store = store_for(
        Namespace::new("dev.rightkit.secrets.test.import-race").unwrap(),
        Arc::new(SavedBetweenCheckAndWrite {
            inner: MemoryBackend::new(),
            concurrent: Secret::from_string("B-saved-by-user"),
        }),
    );
    let env = |name: &str| (name == "KEY").then(|| "A-stale-from-env".to_string());
    let map: rightkit_secrets::EnvMap = &[("acct", &["KEY"])];
    let report = store
        .import_env(map, WriteIntent::CreateOnly, &env)
        .unwrap();
    assert_eq!(report.skipped_present, vec!["acct"], "{report:?}");
    assert!(report.imported.is_empty());
    assert_eq!(
        store.get("acct").unwrap().unwrap().expose_str().unwrap(),
        "B-saved-by-user",
        "the import overwrote a concurrently saved credential"
    );
}
