//! Mutations of one credential target, each under that target's lock.
//!
//! The Windows Credential Manager has no atomic "write if absent" and no "delete if equal", so a
//! backend that wants either must exclude every other writer of the same native target while it
//! checks and acts. Two rules make that sound, and this module is where both live (it compiles on
//! every platform so they are tested everywhere; only the Windows backend uses it):
//!
//! * **Every mutator takes the lock.** If only `add_if_absent` locked, a concurrent `set` between
//!   its check and its write would be overwritten and `add_if_absent` would still report that it
//!   created the item.
//! * **The lock is named by the native target.** Two namespaces that share a Windows prefix write
//!   the same Credential Manager entry, so they must take the same lock; it is keyed on the
//!   normalised target name ([`Namespace::windows_lock_target`]), not on the service.
//!
//! The functions here hold the lock for the whole operation and call the *unlocked* primitives of
//! [`CredentialOps`], so nothing re-takes a lock it already holds.

use crate::backend::ConditionalDelete;
use crate::error::Result;
use crate::namespace::Namespace;
use crate::secret::Secret;

/// The unlocked primitives of one credential store, plus its lock.
pub(crate) trait CredentialOps {
    /// Held for the duration of one mutation; released on drop.
    type Guard;
    /// Block until this process holds the lock for the normalised native target name, excluding
    /// other threads and other processes.
    fn lock(&self, normalized_target: &str) -> Result<Self::Guard>;
    /// Read without locking. `want_blob == false` is a presence check.
    fn read(&self, ns: &Namespace, account: &str, want_blob: bool) -> Result<Option<Secret>>;
    /// Create or replace without locking.
    fn write(&self, ns: &Namespace, account: &str, secret: &Secret) -> Result<()>;
    /// Remove without locking; `Ok(false)` when there was nothing.
    fn remove(&self, ns: &Namespace, account: &str) -> Result<bool>;
}

fn lock_target<O: CredentialOps>(ops: &O, ns: &Namespace, account: &str) -> Result<O::Guard> {
    // The validated, normalised name (the same one the store's exclusion identity is built
    // from): a prefix with a NUL would otherwise lock (and write) a truncated target.
    let target = ns.windows_lock_target(account)?;
    ops.lock(&target)
}

pub(crate) fn set<O: CredentialOps>(
    ops: &O,
    ns: &Namespace,
    account: &str,
    secret: &Secret,
) -> Result<()> {
    let _held = lock_target(ops, ns, account)?;
    ops.write(ns, account, secret)
}

pub(crate) fn add_if_absent<O: CredentialOps>(
    ops: &O,
    ns: &Namespace,
    account: &str,
    secret: &Secret,
) -> Result<bool> {
    let _held = lock_target(ops, ns, account)?;
    if ops.read(ns, account, false)?.is_some() {
        return Ok(false);
    }
    ops.write(ns, account, secret)?;
    Ok(true)
}

pub(crate) fn delete<O: CredentialOps>(ops: &O, ns: &Namespace, account: &str) -> Result<bool> {
    let _held = lock_target(ops, ns, account)?;
    ops.remove(ns, account)
}

pub(crate) fn delete_if_equals<O: CredentialOps>(
    ops: &O,
    ns: &Namespace,
    account: &str,
    expected: &Secret,
) -> Result<ConditionalDelete> {
    let _held = lock_target(ops, ns, account)?;
    match ops.read(ns, account, true)? {
        None => Ok(ConditionalDelete::Absent),
        Some(current) if current.ct_eq(expected) => {
            ops.remove(ns, account)?;
            Ok(ConditionalDelete::Deleted)
        }
        Some(_) => Ok(ConditionalDelete::Changed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::namespace::WindowsTarget;
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Condvar, Mutex};
    use std::time::Duration;

    #[derive(Default)]
    struct Locks {
        held: Mutex<HashSet<String>>,
        freed: Condvar,
        log: Mutex<Vec<String>>,
    }

    struct Guard {
        locks: Arc<Locks>,
        key: String,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            self.locks
                .log
                .lock()
                .unwrap()
                .push(format!("unlock {}", self.key));
            self.locks.held.lock().unwrap().remove(&self.key);
            self.locks.freed.notify_all();
        }
    }

    type Pause = (mpsc::Sender<()>, mpsc::Receiver<()>);

    /// An in-memory credential store with a real blocking per-target lock.
    struct Fake {
        locks: Arc<Locks>,
        items: Mutex<HashMap<String, Vec<u8>>>,
        pause_after_presence_check: Mutex<Option<Pause>>,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                locks: Arc::new(Locks::default()),
                items: Mutex::new(HashMap::new()),
                pause_after_presence_check: Mutex::new(None),
            }
        }

        fn log(&self, line: String) {
            self.locks.log.lock().unwrap().push(line);
        }

        fn lines(&self) -> Vec<String> {
            self.locks.log.lock().unwrap().clone()
        }

        fn item(&self, target: &str) -> Option<Vec<u8>> {
            self.items.lock().unwrap().get(target).cloned()
        }
    }

    impl CredentialOps for Fake {
        type Guard = Guard;

        fn lock(&self, normalized_target: &str) -> Result<Guard> {
            let mut held = self.locks.held.lock().unwrap();
            while held.contains(normalized_target) {
                held = self.locks.freed.wait(held).unwrap();
            }
            held.insert(normalized_target.to_string());
            drop(held);
            self.log(format!("lock {normalized_target}"));
            Ok(Guard {
                locks: self.locks.clone(),
                key: normalized_target.to_string(),
            })
        }

        fn read(&self, ns: &Namespace, account: &str, want_blob: bool) -> Result<Option<Secret>> {
            let found = self.item(&ns.windows_target_name(account));
            self.log(format!("read {want_blob}"));
            if !want_blob {
                let pause = self.pause_after_presence_check.lock().unwrap().take();
                if let Some((entered, resume)) = pause {
                    entered.send(()).unwrap();
                    resume.recv().unwrap();
                }
            }
            Ok(found.map(Secret::from_bytes))
        }

        fn write(&self, ns: &Namespace, account: &str, secret: &Secret) -> Result<()> {
            self.log("write".to_string());
            self.items.lock().unwrap().insert(
                ns.windows_target_name(account),
                secret.expose_bytes().to_vec(),
            );
            Ok(())
        }

        fn remove(&self, ns: &Namespace, account: &str) -> Result<bool> {
            self.log("remove".to_string());
            Ok(self
                .items
                .lock()
                .unwrap()
                .remove(&ns.windows_target_name(account))
                .is_some())
        }
    }

    fn secret(s: &str) -> Secret {
        Secret::from_string(s)
    }

    fn shared(service: &str, prefix: &str) -> Namespace {
        Namespace::new(service)
            .unwrap()
            .windows_target(WindowsTarget::Slash {
                prefix: Some(prefix.to_string()),
            })
    }

    /// Audit finding: only add-if-absent took the lock, so a `set` that landed between its check
    /// and its write was overwritten and add-if-absent still returned `true`.
    #[test]
    fn a_set_cannot_land_between_add_if_absents_check_and_its_write() {
        let ns = Namespace::new("svc").unwrap();
        let fake = Fake::new();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        *fake.pause_after_presence_check.lock().unwrap() = Some((entered_tx, resume_rx));
        let set_finished = AtomicBool::new(false);

        let (added, set_finished_while_add_held_the_target) = std::thread::scope(|scope| {
            let adder = scope.spawn(|| add_if_absent(&fake, &ns, "acct", &secret("A")));
            // The adder has seen the credential absent and is holding the target.
            entered_rx.recv().unwrap();
            let setter = scope.spawn(|| {
                set(&fake, &ns, "acct", &secret("B")).unwrap();
                set_finished.store(true, Ordering::SeqCst);
            });
            std::thread::sleep(Duration::from_millis(250));
            let early = set_finished.load(Ordering::SeqCst);
            resume_tx.send(()).unwrap();
            let added = adder.join().unwrap().unwrap();
            setter.join().unwrap();
            (added, early)
        });

        assert!(
            !set_finished_while_add_held_the_target,
            "set wrote while add_if_absent held the target between its check and its write"
        );
        assert!(added, "the target was absent when add_if_absent looked");
        assert_eq!(
            fake.item("svc/acct").as_deref(),
            Some(&b"B"[..]),
            "set ran after add_if_absent and must not have been overwritten by it"
        );
    }

    /// Audit finding: services that share a Windows prefix write one native target but locked
    /// different names.
    #[test]
    fn services_sharing_a_prefix_take_the_same_lock() {
        let fake = Fake::new();
        let a = shared("svc-a", "Shared");
        let b = shared("svc-b", "SHARED");
        set(&fake, &a, "acct", &secret("x")).unwrap();
        set(&fake, &b, "acct", &secret("y")).unwrap();
        let locks: Vec<String> = fake
            .lines()
            .into_iter()
            .filter(|l| l.starts_with("lock "))
            .collect();
        assert_eq!(locks, ["lock shared/acct", "lock shared/acct"]);
    }

    #[test]
    fn different_targets_take_different_locks() {
        let fake = Fake::new();
        let ns = shared("svc", "p");
        set(&fake, &ns, "one", &secret("x")).unwrap();
        set(&fake, &ns, "two", &secret("x")).unwrap();
        let locks: Vec<String> = fake
            .lines()
            .into_iter()
            .filter(|l| l.starts_with("lock "))
            .collect();
        assert_eq!(locks, ["lock p/one", "lock p/two"]);
    }

    #[test]
    fn every_mutator_holds_the_lock_around_all_of_its_store_calls() {
        type Run = fn(&Fake, &Namespace);
        let cases: [(&str, Run); 5] = [
            ("set", |f, ns| {
                set(f, ns, "acct", &secret("v")).unwrap();
            }),
            ("add_if_absent", |f, ns| {
                add_if_absent(f, ns, "acct", &secret("v")).unwrap();
            }),
            ("delete", |f, ns| {
                delete(f, ns, "acct").unwrap();
            }),
            ("delete_if_equals", |f, ns| {
                delete_if_equals(f, ns, "acct", &secret("v")).unwrap();
            }),
            ("delete_if_equals on a present item", |f, ns| {
                f.items
                    .lock()
                    .unwrap()
                    .insert("svc/acct".to_string(), b"v".to_vec());
                delete_if_equals(f, ns, "acct", &secret("v")).unwrap();
            }),
        ];
        for (name, run) in cases {
            let ns = Namespace::new("svc").unwrap();
            let fake = Fake::new();
            run(&fake, &ns);
            let lines = fake.lines();
            assert_eq!(
                lines.first().map(String::as_str),
                Some("lock svc/acct"),
                "{name}"
            );
            assert_eq!(
                lines.last().map(String::as_str),
                Some("unlock svc/acct"),
                "{name}"
            );
            let inner = &lines[1..lines.len() - 1];
            assert!(!inner.is_empty(), "{name}: touched the store");
            assert!(
                inner
                    .iter()
                    .all(|l| l.starts_with("read") || l == "write" || l == "remove"),
                "{name}: {lines:?}"
            );
        }
    }

    #[test]
    fn add_if_absent_reports_false_and_writes_nothing_when_the_item_exists() {
        let ns = Namespace::new("svc").unwrap();
        let fake = Fake::new();
        set(&fake, &ns, "acct", &secret("first")).unwrap();
        assert!(!add_if_absent(&fake, &ns, "acct", &secret("second")).unwrap());
        assert_eq!(fake.item("svc/acct").as_deref(), Some(&b"first"[..]));
    }

    #[test]
    fn delete_if_equals_removes_only_a_matching_value() {
        let ns = Namespace::new("svc").unwrap();
        let fake = Fake::new();
        assert_eq!(
            delete_if_equals(&fake, &ns, "acct", &secret("v")).unwrap(),
            ConditionalDelete::Absent
        );
        set(&fake, &ns, "acct", &secret("newer")).unwrap();
        assert_eq!(
            delete_if_equals(&fake, &ns, "acct", &secret("older")).unwrap(),
            ConditionalDelete::Changed
        );
        assert_eq!(fake.item("svc/acct").as_deref(), Some(&b"newer"[..]));
        assert_eq!(
            delete_if_equals(&fake, &ns, "acct", &secret("newer")).unwrap(),
            ConditionalDelete::Deleted
        );
        assert_eq!(fake.item("svc/acct"), None);
    }

    /// Audit finding: a prefix with a NUL must not reach the lock or the store at all.
    #[test]
    fn an_invalid_target_is_refused_before_any_lock_or_store_call() {
        let fake = Fake::new();
        let ns = shared("svc", "victim\0ignored");
        assert!(matches!(
            set(&fake, &ns, "acct", &secret("v")),
            Err(Error::Invalid { .. })
        ));
        assert!(matches!(
            delete(&fake, &ns, "acct"),
            Err(Error::Invalid { .. })
        ));
        assert!(matches!(
            add_if_absent(&fake, &ns, "acct", &secret("v")),
            Err(Error::Invalid { .. })
        ));
        assert!(matches!(
            delete_if_equals(&fake, &ns, "acct", &secret("v")),
            Err(Error::Invalid { .. })
        ));
        assert!(fake.lines().is_empty());
    }
}
