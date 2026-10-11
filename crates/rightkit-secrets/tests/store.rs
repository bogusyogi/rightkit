//! Store behaviour on every platform (memory backend and guards) and a real Windows Credential
//! Manager round trip under a unique, cleaned-up test namespace.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use rightkit_secrets::{
    Backend, ConditionalDelete, Config, Error, GuardMode, Interaction, MemoryBackend,
    MigrationReport, Namespace, Secret, Store, WindowsTarget,
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
        Arc::new(MemoryBackend::new()),
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
