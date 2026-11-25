//! End to end: the key manager, the key provider, device-approval transfer, wipe on revoke and the
//! deadman wipe, driven through the public API over real files in a fresh temp directory.
//!
//! The credential store is a real `rightkit_credentials::Store` over a file-backed `Backend` (one
//! file per credential, no OS keychain, no network), so every "reload" below opens a brand new
//! `Store` over the same directory and reads what is on disk.
//!
//! It proves: a key is created once and survives a reload; a record sealed under a key is opened
//! again through `ZoneKeys` by key id and by version, across a rotation and until the old version is
//! retired; a tampered record, another zone and a key the device never held do not open it; a key
//! moves to a new device only through the short-code transfer, and a wrong code installs nothing;
//! a revoke notice wipes only its target (data erased, keys gone from disk, no way to read the old
//! record afterwards) while a tampered notice, another device and the already-rotated owner are
//! left alone; a wipe whose erase fails stays pending on disk, keeps the keys, and `resume`
//! finishes it; and the deadman clock, self-wipe and resume behave the same way.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rightkit_credentials::{
    Backend, Config, Error as CredError, Interaction, Namespace, Result as CredResult, Secret,
    Store,
};
use rightkit_sync_keys::{
    issue_revoke_notice, revoke_device, DataKey, Deadman, DeviceSessions, Error, InstallOutcome,
    KeyId, KeyManager, KeyProvider, LocalCopy, NoticeOutcome, RevokeWipe, TransferContext,
    ZoneKeys, KEY_LEN,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

const APP: &str = "app.example.keys-e2e";
const ACCOUNT: &str = "account-e2e";
const ZONE: &str = "notes";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "rightkit-sync-keys-e2e-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create scratch");
        Scratch(root)
    }
    fn dir(&self, name: &str) -> PathBuf {
        let dir = self.0.join(name);
        std::fs::create_dir_all(&dir).expect("create dir");
        dir
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

/// A credential backend that keeps one file per credential under a directory.
struct FileBackend {
    dir: PathBuf,
}

impl FileBackend {
    fn path(&self, ns: &Namespace, account: &str) -> PathBuf {
        self.dir
            .join(format!("{}.{}", hex(ns.service()), hex(account)))
    }
}

fn io(op: &'static str, e: std::io::Error) -> CredError {
    CredError::Backend {
        op,
        code: e.raw_os_error().unwrap_or(-1) as i64,
    }
}

impl Backend for FileBackend {
    fn name(&self) -> &'static str {
        "file-e2e"
    }
    fn durable(&self) -> bool {
        true
    }
    fn get(&self, ns: &Namespace, account: &str, _: Interaction) -> CredResult<Option<Secret>> {
        match std::fs::read(self.path(ns, account)) {
            Ok(bytes) => Ok(Some(Secret::from_bytes(bytes))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io("get", e)),
        }
    }
    fn exists(&self, ns: &Namespace, account: &str, _: Interaction) -> CredResult<bool> {
        Ok(self.path(ns, account).exists())
    }
    fn set(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        _: Interaction,
    ) -> CredResult<()> {
        let path = self.path(ns, account);
        let mut tmp_name = path.file_name().expect("name").to_os_string();
        tmp_name.push(".tmp");
        let tmp = path.with_file_name(tmp_name);
        std::fs::write(&tmp, secret.expose_bytes()).map_err(|e| io("set", e))?;
        std::fs::rename(&tmp, &path).map_err(|e| io("set", e))
    }
    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        _: Interaction,
    ) -> CredResult<bool> {
        use std::io::Write;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.path(ns, account))
        {
            Ok(mut f) => {
                f.write_all(secret.expose_bytes())
                    .map_err(|e| io("add", e))?;
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(io("add", e)),
        }
    }
    fn delete(&self, ns: &Namespace, account: &str, _: Interaction) -> CredResult<bool> {
        match std::fs::remove_file(self.path(ns, account)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(io("delete", e)),
        }
    }
}

/// A manager over a brand new `Store` on `dir`: calling it again is a reload from disk.
fn manager_for(dir: &Path, account: &str) -> KeyManager {
    let ns = Namespace::new(APP).expect("namespace");
    let backend = Arc::new(FileBackend {
        dir: dir.to_path_buf(),
    });
    let store = Store::open(Config::new(ns).backend(backend));
    KeyManager::new(store, account).expect("manager")
}

fn manager(dir: &Path) -> KeyManager {
    manager_for(dir, ACCOUNT)
}

fn zone_keys(dir: &Path) -> ZoneKeys {
    ZoneKeys::new(manager(dir), APP, ZONE).expect("zone keys")
}

static NONCE: AtomicU64 = AtomicU64::new(1);

/// `key id (17 bytes) | nonce (24) | ciphertext`, the key id bound as associated data.
fn seal(provider: &dyn KeyProvider, plaintext: &[u8]) -> Vec<u8> {
    let key = provider.current_key().expect("current key");
    let id = key.key_id().as_str().to_string();
    let mut nonce = [0u8; 24];
    nonce[..8].copy_from_slice(&NONCE.fetch_add(1, Ordering::SeqCst).to_be_bytes());
    nonce[8..12].copy_from_slice(&std::process::id().to_be_bytes());
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_bytes()).expect("cipher");
    let ct = cipher
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: plaintext,
                aad: id.as_bytes(),
            },
        )
        .expect("seal");
    let mut out = id.into_bytes();
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

/// `None` when the provider does not hold the key or the record does not authenticate.
fn open(provider: &dyn KeyProvider, blob: &[u8]) -> Option<Vec<u8>> {
    let id = KeyId::parse(std::str::from_utf8(blob.get(..17)?).ok()?).ok()?;
    let key = provider.key_by_id(&id).expect("lookup")?;
    let mut nonce = [0u8; 24];
    nonce.copy_from_slice(blob.get(17..41)?);
    XChaCha20Poly1305::new_from_slice(key.as_bytes())
        .ok()?
        .decrypt(
            &XNonce::from(nonce),
            Payload {
                msg: blob.get(41..)?,
                aad: id.as_str().as_bytes(),
            },
        )
        .ok()
}

/// A real directory a wipe must erase. `blocked` makes the erase fail before it removes anything
/// (a copy that cannot be erased right now).
struct DirCopy {
    path: PathBuf,
    blocked: Arc<AtomicBool>,
}

impl DirCopy {
    fn boxed(path: &Path, blocked: &Arc<AtomicBool>) -> Vec<Box<dyn LocalCopy>> {
        vec![Box::new(DirCopy {
            path: path.to_path_buf(),
            blocked: blocked.clone(),
        })]
    }
}

impl LocalCopy for DirCopy {
    fn name(&self) -> &str {
        "synced-data"
    }
    fn erase(&self) -> std::io::Result<()> {
        if self.blocked.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("copy is in use"));
        }
        match std::fs::remove_dir_all(&self.path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

fn synced_copy(scratch: &Scratch, name: &str) -> PathBuf {
    let dir = scratch.dir(name);
    std::fs::write(dir.join("records.db"), b"synced plaintext copy").expect("write copy");
    dir
}

fn not_enrolled(dir: &Path) {
    let m = manager(dir);
    assert!(!m.is_enrolled().expect("enrolled check"));
    assert!(m.current().expect("current").is_none());
    assert_eq!(m.require_current().unwrap_err(), Error::NotEnrolled);
    assert_eq!(
        zone_keys(dir).current_key().unwrap_err(),
        Error::NotEnrolled
    );
}

#[test]
fn key_is_created_once_survives_reload_and_rotates_with_old_records_readable() {
    let scratch = Scratch::new();
    let dir = scratch.dir("device");

    assert!(!manager(&dir).is_enrolled().expect("enrolled"));
    let (first, created) = manager(&dir).create_if_absent().expect("create");
    assert!(created);
    assert_eq!(first.version(), 1);
    assert_eq!(first.expose_bytes().len(), KEY_LEN);
    let (again, created_again) = manager(&dir).create_if_absent().expect("create again");
    assert!(
        !created_again,
        "a reload must adopt the key, not mint another"
    );
    assert_eq!(again.key_id(), first.key_id());
    assert_eq!(again.expose_bytes(), first.expose_bytes());
    assert!(format!("{first:?}").contains("redacted"));

    // Seal under v1, reload from disk, open.
    let provider = zone_keys(&dir);
    let scope_v1 = provider.account_scope().expect("scope");
    let record_v1 = seal(&provider, b"written under version one");
    assert_eq!(
        open(&zone_keys(&dir), &record_v1).as_deref(),
        Some(&b"written under version one"[..])
    );

    // A tampered record, another zone, another account and an unknown id do not open it.
    let mut tampered = record_v1.clone();
    *tampered.last_mut().unwrap() ^= 1;
    assert!(open(&zone_keys(&dir), &tampered).is_none());
    let other_zone = ZoneKeys::new(manager(&dir), APP, "other-zone").expect("zone");
    let wrong = other_zone
        .key_by_id(first.key_id())
        .expect("lookup")
        .expect("held");
    assert_ne!(
        wrong.as_bytes(),
        provider.current_key().expect("key").as_bytes(),
        "zones must not share a key"
    );
    assert!(open(&other_zone, &record_v1).is_none());
    assert!(!manager_for(&dir, "someone-else")
        .is_enrolled()
        .expect("enrolled"));
    let stranger = DataKey::generate(1).expect("generate");
    assert!(provider
        .key_by_id(stranger.key_id())
        .expect("lookup")
        .is_none());
    assert!(KeyId::parse("not-a-key-id").is_err());

    // Rotate: new id, new scope, old record still opens, both lookups agree.
    let second = manager(&dir).rotate().expect("rotate");
    assert_eq!(second.version(), 2);
    assert_ne!(second.key_id(), first.key_id());
    let provider = zone_keys(&dir);
    assert_ne!(provider.account_scope().expect("scope"), scope_v1);
    let record_v2 = seal(&provider, b"written under version two");
    assert_eq!(
        open(&provider, &record_v1).as_deref(),
        Some(&b"written under version one"[..])
    );
    assert_eq!(
        open(&provider, &record_v2).as_deref(),
        Some(&b"written under version two"[..])
    );
    assert_eq!(
        provider
            .key_by_version(1)
            .expect("v1")
            .expect("held")
            .key_id(),
        first.key_id()
    );
    assert!(provider.key_by_version(9).expect("v9").is_none());
    assert_eq!(
        manager(&dir).require_current().expect("current").version(),
        2
    );

    // The current version cannot be retired; an old one can, and then its record is gone for good,
    // including after a reload.
    assert!(matches!(
        manager(&dir).retire(2),
        Err(Error::Invalid { .. })
    ));
    assert!(manager(&dir).retire(1).expect("retire"));
    let provider = zone_keys(&dir);
    assert!(open(&provider, &record_v1).is_none());
    assert!(provider.key_by_version(1).expect("v1").is_none());
    assert_eq!(
        open(&provider, &record_v2).as_deref(),
        Some(&b"written under version two"[..])
    );

    // Corrupt stored key: reported, never replaced by a new one.
    let v2_file = std::fs::read_dir(&dir)
        .expect("list")
        .map(|e| e.expect("entry").path())
        .find(|p| {
            std::fs::read(p)
                .map(|b| b == hex_of(second.expose_bytes()).as_bytes())
                .unwrap_or(false)
        })
        .expect("the v2 key file");
    std::fs::write(&v2_file, b"not hex").expect("corrupt");
    assert_eq!(manager(&dir).current().unwrap_err(), Error::Corrupt);
    assert_eq!(
        manager(&dir).create_if_absent().unwrap_err(),
        Error::Corrupt
    );

    // Sign-out erases everything on disk.
    std::fs::write(&v2_file, hex_of(second.expose_bytes())).expect("restore");
    manager(&dir).forget_all().expect("forget");
    not_enrolled(&dir);
    assert!(open(&zone_keys(&dir), &record_v2).is_none());
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn install_enforces_versions_and_replace_erases_every_old_key() {
    let scratch = Scratch::new();
    let (a, b) = (scratch.dir("a"), scratch.dir("b"));
    let (key, _) = manager(&a).create_if_absent().expect("create");

    assert_eq!(
        manager(&b).install(&key, false).expect("install"),
        InstallOutcome::Advanced
    );
    assert_eq!(
        manager(&b).install(&key, false).expect("again"),
        InstallOutcome::AlreadyCurrent
    );

    // A different key at the same version is a conflict, not an overwrite.
    let rival = DataKey::generate(1).expect("rival");
    assert_eq!(
        manager(&b).install(&rival, false).unwrap_err(),
        Error::VersionConflict { version: 1 }
    );
    assert_eq!(
        manager(&b).require_current().expect("current").key_id(),
        key.key_id()
    );

    // Reset path: replace erases every earlier version.
    manager(&b).rotate().expect("rotate");
    assert_eq!(
        manager(&b).install(&rival, true).expect("replace"),
        InstallOutcome::Replaced
    );
    let m = manager(&b);
    assert_eq!(
        m.require_current().expect("current").key_id(),
        rival.key_id()
    );
    assert!(m.get_version(2).expect("v2").is_none());
    assert!(m.find_by_id(key.key_id()).expect("find").is_none());
}

#[test]
fn key_reaches_a_new_device_only_through_the_short_code_transfer() {
    let scratch = Scratch::new();
    let (owner_dir, new_dir) = (scratch.dir("owner"), scratch.dir("new"));
    let (owner_key, _) = manager(&owner_dir).create_if_absent().expect("create");
    let record = seal(&zone_keys(&owner_dir), b"owner record");
    let ctx = || TransferContext::new(APP, ACCOUNT).expect("context");

    let owner = DeviceSessions::new();
    let newcomer = DeviceSessions::new();

    // First attempt: the wrong code is typed on the new device. Nothing is installed.
    let begin = newcomer.request_begin(ctx(), "Work laptop").expect("begin");
    let (offer, label) = owner
        .approve_begin(ctx(), "new-id", &begin)
        .expect("approve begin");
    assert_eq!(label.as_deref(), Some("Work laptop"));
    let (reveal, new_code) = newcomer.request_offer(&offer).expect("offer");
    owner.approve_reveal("new-id", &reveal).expect("reveal");
    let (shown_label, shown_device, owner_code) = owner.approve_show().expect("show");
    assert_eq!(shown_label.as_deref(), Some("Work laptop"));
    assert_eq!(shown_device, "new-id");
    assert!(
        owner.approve_reveal("someone-else", &reveal).is_err(),
        "a session answers only the device it was opened for"
    );
    owner
        .approve_commit(&new_code, &owner_dir_manager(&owner_dir))
        .expect("commit");
    assert!(owner.approve_take_transfer("someone-else").is_none());
    let transfer = owner.approve_take_transfer("new-id").expect("transfer");
    assert!(owner.approve_take_transfer("new-id").is_none(), "one use");

    let mut bad = owner_code.clone().into_bytes();
    bad[0] = if bad[0] == b'0' { b'1' } else { b'0' };
    let bad = String::from_utf8(bad).expect("digits");
    assert!(newcomer
        .request_accept(&bad, &transfer, || Ok(manager(&new_dir)))
        .is_err());
    not_enrolled(&new_dir);

    // Second attempt, right codes: installed, same key, and it reads the owner's record.
    let begin = newcomer.request_begin(ctx(), "Work laptop").expect("begin");
    let (offer, _) = owner
        .approve_begin(ctx(), "new-id", &begin)
        .expect("approve begin");
    let (reveal, new_code) = newcomer.request_offer(&offer).expect("offer");
    owner.approve_reveal("new-id", &reveal).expect("reveal");
    let (_, _, owner_code) = owner.approve_show().expect("show");
    owner
        .approve_commit(&new_code, &owner_dir_manager(&owner_dir))
        .expect("commit");
    let transfer = owner.approve_take_transfer("new-id").expect("transfer");
    newcomer
        .request_accept(&owner_code, &transfer, || Ok(manager(&new_dir)))
        .expect("accept");

    let installed = manager(&new_dir).require_current().expect("installed");
    assert_eq!(installed.key_id(), owner_key.key_id());
    assert_eq!(
        open(&zone_keys(&new_dir), &record).as_deref(),
        Some(&b"owner record"[..])
    );
}

fn owner_dir_manager(dir: &Path) -> KeyManager {
    manager(dir)
}

#[test]
fn revoke_notice_wipes_only_its_target_and_the_revoked_device_loses_everything() {
    let scratch = Scratch::new();
    let (a, b, c) = (scratch.dir("a"), scratch.dir("b"), scratch.dir("c"));
    let (key_v1, _) = manager(&a).create_if_absent().expect("create");
    manager(&b).install(&key_v1, false).expect("enrol b");
    manager(&c).install(&key_v1, false).expect("enrol c");
    let record_v1 = seal(&zone_keys(&a), b"before the revoke");
    let b_copy = synced_copy(&scratch, "b-data");
    let c_copy = synced_copy(&scratch, "c-data");
    let free = Arc::new(AtomicBool::new(false));
    let wipe_b =
        || RevokeWipe::new(manager(&b), "dev-b", DirCopy::boxed(&b_copy, &free)).expect("wipe b");
    let wipe_c =
        || RevokeWipe::new(manager(&c), "dev-c", DirCopy::boxed(&c_copy, &free)).expect("wipe c");
    let wipe_a = || {
        RevokeWipe::new(
            manager(&a),
            "dev-a",
            DirCopy::boxed(&scratch.dir("a-data"), &free),
        )
        .expect("wipe a")
    };

    assert!(matches!(
        issue_revoke_notice(&manager(&a), "dev-a", "dev-a", 1),
        Err(Error::Invalid { .. })
    ));

    // The owner revokes dev-b: the notice is handed out, then the key rotates.
    let (mut published, mut notified) = (Vec::new(), Vec::new());
    let report = revoke_device(
        &manager(&a),
        "dev-a",
        "dev-b",
        1_700_000_000,
        |n| {
            published = n.to_vec();
            true
        },
        |n| {
            notified = n.to_vec();
            false
        },
    )
    .expect("revoke");
    assert!(report.published && !report.notified);
    assert_eq!(published, notified);
    let key_v2 = manager(&a).require_current().expect("rotated");
    assert_eq!(key_v2.version(), 2);
    assert_ne!(key_v2.key_id(), key_v1.key_id());

    // New data is unreadable to the revoked device even before it learns of the revoke.
    let record_v2 = seal(&zone_keys(&a), b"after the revoke");
    assert!(open(&zone_keys(&b), &record_v2).is_none());
    assert_eq!(
        open(&zone_keys(&b), &record_v1).as_deref(),
        Some(&b"before the revoke"[..])
    );

    // The notice means nothing to other devices.
    assert_eq!(
        wipe_c().handle_notice(&published).expect("c"),
        NoticeOutcome::NotForThisDevice
    );
    assert_eq!(
        wipe_a().handle_notice(&published).expect("a"),
        NoticeOutcome::Ignored,
        "the owner already rotated past the key the notice is sealed under"
    );
    assert!(manager(&c).is_enrolled().expect("c enrolled"));
    assert!(manager(&a).is_enrolled().expect("a enrolled"));
    assert!(c_copy.join("records.db").exists());

    // A damaged or forged notice touches nothing on the target.
    let mut tampered = published.clone();
    *tampered.last_mut().unwrap() ^= 1;
    assert_eq!(
        wipe_b().handle_notice(&tampered).unwrap_err(),
        Error::Authentication
    );
    assert!(matches!(
        wipe_b().handle_notice(&published[..10]),
        Err(Error::Protocol(_))
    ));
    assert!(manager(&b).is_enrolled().expect("b enrolled"));
    assert!(b_copy.join("records.db").exists());
    assert!(!wipe_b().is_pending().expect("pending"));

    // The real notice: data erased, keys gone on disk, nothing left to read the old record with.
    assert_eq!(
        wipe_b().handle_notice(&published).expect("wipe"),
        NoticeOutcome::Wiped
    );
    assert!(!b_copy.exists());
    assert!(!wipe_b().is_pending().expect("pending"));
    not_enrolled(&b);
    assert!(open(&zone_keys(&b), &record_v1).is_none());
    assert_eq!(
        wipe_b().handle_notice(&published).expect("replay"),
        NoticeOutcome::Ignored,
        "a replay finds no key and does nothing"
    );

    // The owner keeps reading both generations.
    assert!(open(&zone_keys(&a), &record_v1).is_some());
    assert!(open(&zone_keys(&a), &record_v2).is_some());
}

#[test]
fn a_wipe_whose_erase_fails_stays_pending_on_disk_and_resume_finishes_it() {
    let scratch = Scratch::new();
    let (owner, target) = (scratch.dir("owner"), scratch.dir("target"));
    let (key, _) = manager(&owner).create_if_absent().expect("create");
    manager(&target).install(&key, false).expect("enrol");
    let copy = synced_copy(&scratch, "target-data");
    let notice =
        issue_revoke_notice(&manager(&owner), "dev-owner", "dev-target", 5).expect("notice");

    let blocked = Arc::new(AtomicBool::new(true));
    let wipe = RevokeWipe::new(
        manager(&target),
        "dev-target",
        DirCopy::boxed(&copy, &blocked),
    )
    .expect("wipe");
    assert_eq!(
        wipe.handle_notice(&notice).unwrap_err(),
        Error::WipeFailed {
            copy: "synced-data".into()
        }
    );
    // Keys stay until the data is gone, and the pending mark is on disk for the next start.
    assert!(copy.join("records.db").exists());
    assert!(manager(&target).is_enrolled().expect("enrolled"));
    let restarted = RevokeWipe::new(
        manager(&target),
        "dev-target",
        DirCopy::boxed(&copy, &blocked),
    )
    .expect("restart");
    assert!(restarted.is_pending().expect("pending"));
    assert_eq!(
        restarted.resume().unwrap_err(),
        Error::WipeFailed {
            copy: "synced-data".into()
        }
    );
    assert!(restarted.is_pending().expect("still pending"));

    blocked.store(false, Ordering::SeqCst);
    assert!(restarted.resume().expect("resume"));
    assert!(!copy.exists());
    assert!(!restarted.is_pending().expect("done"));
    not_enrolled(&target);
    assert!(!restarted.resume().expect("nothing left"));
}

#[test]
fn deadman_clock_and_self_wipe_destroy_data_and_keys_and_survive_a_crash() {
    const DAY: i64 = 86_400_000;
    let scratch = Scratch::new();
    let keys = scratch.dir("keys");
    let state = scratch.dir("state");
    let marker = scratch.dir("outside").join("self-wipe.pending");
    let (key, _) = manager(&keys).create_if_absent().expect("create");
    let record = seal(&zone_keys(&keys), b"deadman record");
    let copy = synced_copy(&scratch, "deadman-data");
    let blocked = Arc::new(AtomicBool::new(false));
    let copies = DirCopy::boxed(&copy, &blocked);
    let open_manager = || -> Result<KeyManager, String> { Ok(manager(&keys)) };
    let t0 = 1_700_000_000_000i64;

    // The clock starts when first asked; a reload of the clock sees the same mark.
    let deadman = Deadman::new(&state, &marker, 3);
    assert!(!deadman.due(t0));
    assert!(!Deadman::new(&state, &marker, 3).due(t0 + 3 * DAY));
    assert!(Deadman::new(&state, &marker, 3).due(t0 + 3 * DAY + 1));
    deadman.record_heard(t0 + 2 * DAY);
    assert!(!Deadman::new(&state, &marker, 3).due(t0 + 5 * DAY));
    assert!(Deadman::new(&state, &marker, 3).due(t0 + 5 * DAY + 1));
    assert!(!deadman.is_pending());
    assert!(!deadman.resume(&copies, open_manager), "nothing pending");
    assert!(manager(&keys).is_enrolled().expect("enrolled"));

    // A self-wipe that cannot erase fails, keeps the keys and leaves its marker for the next start.
    blocked.store(true, Ordering::SeqCst);
    assert!(deadman.self_wipe(&copies, open_manager).is_err());
    assert!(deadman.is_pending());
    assert!(marker.exists());
    assert!(copy.join("records.db").exists());
    assert_eq!(
        manager(&keys).require_current().expect("kept").key_id(),
        key.key_id()
    );
    assert!(
        Deadman::new(&state, &marker, 3).is_pending(),
        "a restart sees it"
    );

    // The next start finishes it.
    blocked.store(false, Ordering::SeqCst);
    assert!(Deadman::new(&state, &marker, 3).resume(&copies, open_manager));
    assert!(!marker.exists());
    assert!(!Deadman::new(&state, &marker, 3).is_pending());
    assert!(!copy.exists());
    not_enrolled(&keys);
    assert!(open(&zone_keys(&keys), &record).is_none());

    // A clean self-wipe on an enrolled device does the whole job in one call.
    manager(&keys).create_if_absent().expect("re-enrol");
    std::fs::create_dir_all(&copy).expect("recreate");
    deadman.self_wipe(&copies, open_manager).expect("self wipe");
    assert!(!deadman.is_pending());
    assert!(!copy.exists());
    not_enrolled(&keys);
}
