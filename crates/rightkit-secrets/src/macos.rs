//! macOS keychain backend over the legacy file-keychain API (the same generic-password items
//! `keyring` and `security-framework` read, so existing items stay readable).
//!
//! The system backend targets the user-domain DEFAULT keychain explicitly on every call
//! (`SecKeychainCopyDomainDefault`, resolved per call so a changed default is followed), never
//! the null keychain: for the `Find` calls null means the whole user search list, so a duplicate
//! service and account item in another keychain could be read, modified or deleted. If the
//! default keychain cannot be resolved the call fails with `Error::Unavailable`; it never falls
//! back to the search list.
//!
//! Presence uses `SecKeychainFindGenericPassword` requesting neither the password nor an item
//! reference, so no secret bytes are fetched and no ACL authorization is evaluated. Every call
//! sets the process-wide UI policy first; non-interactive calls fail with
//! `errSecInteractionNotAllowed` instead of showing a dialog.

use crate::backend::{Backend, Interaction};
use crate::error::{Error, Result};
use crate::namespace::{check_target, Namespace};
use crate::secret::Secret;
use std::ffi::{c_char, c_void, CString};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex};

type OSStatus = i32;

#[link(name = "Security", kind = "framework")]
extern "C" {
    fn SecKeychainSetUserInteractionAllowed(allowed: u8) -> OSStatus;
    fn SecKeychainGetUserInteractionAllowed(allowed: *mut u8) -> OSStatus;
    fn SecKeychainCreate(
        path: *const c_char,
        pw_len: u32,
        pw: *const c_void,
        prompt: u8,
        access: *const c_void,
        out: *mut *mut c_void,
    ) -> OSStatus;
    fn SecKeychainOpen(path: *const c_char, out: *mut *mut c_void) -> OSStatus;
    fn SecKeychainCopyDomainDefault(domain: i32, out: *mut *mut c_void) -> OSStatus;
    fn SecKeychainDelete(kc: *mut c_void) -> OSStatus;
    fn SecKeychainLock(kc: *mut c_void) -> OSStatus;
    fn SecKeychainUnlock(kc: *mut c_void, pw_len: u32, pw: *const c_void, use_pw: u8) -> OSStatus;
    fn SecKeychainAddGenericPassword(
        kc: *mut c_void,
        sl: u32,
        s: *const c_char,
        al: u32,
        a: *const c_char,
        pl: u32,
        p: *const c_void,
        item: *mut *mut c_void,
    ) -> OSStatus;
    fn SecKeychainFindGenericPassword(
        kc: *const c_void,
        sl: u32,
        s: *const c_char,
        al: u32,
        a: *const c_char,
        pl: *mut u32,
        p: *mut *mut c_void,
        item: *mut *mut c_void,
    ) -> OSStatus;
    fn SecKeychainItemModifyAttributesAndData(
        item: *mut c_void,
        attrs: *const c_void,
        len: u32,
        data: *const c_void,
    ) -> OSStatus;
    fn SecKeychainItemDelete(item: *mut c_void) -> OSStatus;
    fn SecKeychainItemCopyKeychain(item: *mut c_void, kc: *mut *mut c_void) -> OSStatus;
    fn SecKeychainGetPath(kc: *const c_void, len: *mut u32, path: *mut c_char) -> OSStatus;
    fn SecKeychainItemFreeContent(attrs: *const c_void, data: *mut c_void) -> OSStatus;
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *const c_void);
}

const NOT_FOUND: OSStatus = -25300;
const DUPLICATE: OSStatus = -25299;
const AUTH_FAILED: OSStatus = -25293;
const INTERACTION_NOT_ALLOWED: OSStatus = -25308;
const USER_CANCELED: OSStatus = -128;
const NO_SUCH_KEYCHAIN: OSStatus = -25294;
const NO_DEFAULT_KEYCHAIN: OSStatus = -25307;
#[cfg(test)]
const READ_ONLY: OSStatus = -25292;
/// `kSecPreferencesDomainUser`: the per-user domain whose default keychain is the login keychain
/// unless the user changed it. The previous `keyring` backend resolved exactly this domain.
const PREFERENCES_DOMAIN_USER: i32 = 0;

fn map(op: &'static str, status: OSStatus) -> Error {
    match status {
        INTERACTION_NOT_ALLOWED => Error::InteractionRequired,
        USER_CANCELED | AUTH_FAILED => Error::Denied,
        NO_SUCH_KEYCHAIN | NO_DEFAULT_KEYCHAIN => {
            Error::Unavailable(format!("no usable keychain (code {status})"))
        }
        s => Error::Backend { op, code: s as i64 },
    }
}

struct Handle(*mut c_void);
// SAFETY: a SecKeychainRef is a thread-safe CoreFoundation object; we only release it on drop.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

/// `SecKeychainSetUserInteractionAllowed` is process-wide, so calls are serialized and each one
/// sets the policy it needs immediately before the OS call.
static UI_LOCK: Mutex<()> = Mutex::new(());

/// Which keychain a backend operates on. There is deliberately no "null" (search list) variant.
enum Target {
    /// The user-domain default keychain, resolved on every operation.
    UserDefault,
    /// One explicit keychain file.
    File(Arc<Handle>),
    /// Test seam: a resolver standing in for the user-domain default lookup.
    #[cfg(test)]
    Resolved(Arc<dyn Fn() -> Result<Arc<Handle>> + Send + Sync>),
}

/// The user-domain default keychain (the login keychain unless the user changed it), or one
/// explicit keychain file.
pub struct KeychainBackend {
    target: Target,
}

fn default_from_status(st: OSStatus, kc: *mut c_void) -> Result<Arc<Handle>> {
    // Every failure to resolve the default keychain (a missing-keychain status, any other status,
    // or success with a null reference) is the documented `Unavailable`; the caller never gets a
    // typed `Denied`/`InteractionRequired`/`Backend` error for it, and never a search-list
    // fallback. A reference returned alongside a failure status is not released (not ours to
    // free when the call reports failure).
    if st != 0 || kc.is_null() {
        return Err(Error::Unavailable(format!(
            "cannot resolve the default keychain (code {st})"
        )));
    }
    Ok(Arc::new(Handle(kc)))
}

fn user_default_keychain() -> Result<Arc<Handle>> {
    let mut kc: *mut c_void = null_mut();
    let st = unsafe { SecKeychainCopyDomainDefault(PREFERENCES_DOMAIN_USER, &mut kc) };
    default_from_status(st, kc)
}

fn keychain_path(kc: *const c_void) -> Option<PathBuf> {
    let mut buf = vec![0u8; 4096];
    let mut len = buf.len() as u32;
    let st = unsafe { SecKeychainGetPath(kc, &mut len, buf.as_mut_ptr().cast()) };
    if st != 0 || len as usize > buf.len() {
        return None;
    }
    buf.truncate(len as usize);
    String::from_utf8(buf).ok().map(PathBuf::from)
}

/// True only when `item` is positively known to belong to `kc` (same keychain file). Any
/// failure to establish that is `false`.
fn item_is_in(item: &Handle, kc: &Handle) -> bool {
    let mut owner: *mut c_void = null_mut();
    let st = unsafe { SecKeychainItemCopyKeychain(item.0, &mut owner) };
    if st != 0 || owner.is_null() {
        return false;
    }
    let owner = Handle(owner);
    match (keychain_path(owner.0), keychain_path(kc.0)) {
        (Some(a), Some(b)) => match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => a == b,
        },
        _ => false,
    }
}

impl KeychainBackend {
    /// The user-domain default keychain. Operations never reach other keychains on the search
    /// list.
    pub fn system() -> Self {
        Self {
            target: Target::UserDefault,
        }
    }
    /// Open an existing keychain file; ONLY that file is searched or written.
    pub fn open_file(path: &Path, password: Option<&str>) -> Result<Self> {
        let c = CString::new(path.to_string_lossy().as_bytes()).map_err(|_| Error::Invalid {
            what: "path",
            reason: "NUL".into(),
        })?;
        let mut kc: *mut c_void = null_mut();
        let st = unsafe { SecKeychainOpen(c.as_ptr(), &mut kc) };
        if st != 0 || kc.is_null() {
            return Err(map("open_keychain", st));
        }
        let h = Arc::new(Handle(kc));
        if let Some(pw) = password {
            let st = unsafe { SecKeychainUnlock(kc, pw.len() as u32, pw.as_ptr().cast(), 1) };
            if st != 0 {
                return Err(map("unlock_keychain", st));
            }
        }
        Ok(Self {
            target: Target::File(h),
        })
    }
    /// The keychain this call operates on; always a concrete keychain, never null.
    fn keychain(&self) -> Result<Arc<Handle>> {
        match &self.target {
            Target::UserDefault => user_default_keychain(),
            Target::File(h) => Ok(h.clone()),
            #[cfg(test)]
            Target::Resolved(f) => f(),
        }
    }

    fn with_ui<T>(mode: Interaction, f: impl FnOnce() -> T) -> Result<T> {
        let _g = UI_LOCK
            .lock()
            .map_err(|_| Error::Unavailable("keychain UI lock poisoned".into()))?;
        let st = unsafe {
            SecKeychainSetUserInteractionAllowed((mode == Interaction::Interactive) as u8)
        };
        if st != 0 {
            return Err(Error::Unavailable(format!(
                "cannot set Keychain interaction policy (code {st})"
            )));
        }
        Ok(f())
    }

    fn find(
        kc: &Handle,
        ns: &Namespace,
        account: &str,
        want_data: bool,
        want_item: bool,
    ) -> (OSStatus, Option<Secret>, *mut c_void) {
        let (s, a) = (ns.service(), account);
        let (mut len, mut data, mut item) = (0u32, null_mut::<c_void>(), null_mut::<c_void>());
        let st = unsafe {
            SecKeychainFindGenericPassword(
                kc.0,
                s.len() as u32,
                s.as_ptr().cast(),
                a.len() as u32,
                a.as_ptr().cast(),
                if want_data { &mut len } else { null_mut() },
                if want_data { &mut data } else { null_mut() },
                if want_item { &mut item } else { null_mut() },
            )
        };
        let secret = if st == 0 && want_data {
            let v = if data.is_null() {
                Vec::new()
            } else {
                unsafe { std::slice::from_raw_parts(data as *const u8, len as usize) }.to_vec()
            };
            unsafe { SecKeychainItemFreeContent(null(), data) };
            Some(Secret::from_bytes(v))
        } else {
            None
        };
        (st, secret, item)
    }

    /// Add a generic password and verify that the created item really lives in `kc`.
    ///
    /// The native add can fall back to the DEFAULT keychain when an explicit keychain has become
    /// unavailable (source-derived, not reproduced; see the crate README). So the item reference
    /// is requested and its owning keychain compared with `kc`; an item that is elsewhere (or
    /// whose location cannot be established) is deleted again and reported as an error. The
    /// secret exists in the wrong keychain only for the instant between add and delete.
    fn add(kc: &Handle, ns: &Namespace, account: &str, secret: &Secret) -> Result<OSStatus> {
        let (s, a, p) = (ns.service(), account, secret.expose_bytes());
        let mut item: *mut c_void = null_mut();
        let st = unsafe {
            SecKeychainAddGenericPassword(
                kc.0,
                s.len() as u32,
                s.as_ptr().cast(),
                a.len() as u32,
                a.as_ptr().cast(),
                p.len() as u32,
                p.as_ptr().cast(),
                &mut item,
            )
        };
        let item = Handle(item); // released on drop when non-null
        if st != 0 {
            return Ok(st);
        }
        if !item.0.is_null() && item_is_in(&item, kc) {
            return Ok(0);
        }
        if item.0.is_null() {
            return Err(Error::Unavailable(
                "added item cannot be located to verify its keychain".into(),
            ));
        }
        match unsafe { SecKeychainItemDelete(item.0) } {
            0 => Err(Error::Unavailable(
                "item was created outside the requested keychain and has been removed".into(),
            )),
            dst => Err(Error::Unavailable(format!(
                "item was created outside the requested keychain and could not be removed (code {dst})"
            ))),
        }
    }
}

/// The OS item operations `set` needs, so the replacement decision can be tested without
/// real failures from the OS.
trait ItemOps {
    type Item;
    fn add(&self) -> Result<OSStatus>;
    fn find_item(&self) -> (OSStatus, Option<Self::Item>);
    fn modify(&self, item: &Self::Item) -> OSStatus;
}

fn set_flow<O: ItemOps>(ops: &O) -> Result<()> {
    let st = ops.add()?;
    if st == 0 {
        return Ok(());
    }
    if st != DUPLICATE {
        return Err(map("set", st));
    }
    let (fst, item) = ops.find_item();
    let item = match (fst, item) {
        (0, Some(i)) => i,
        (0, None) => return Err(map("set", 0)),
        (f, _) => return Err(map("set", f)),
    };
    let mst = ops.modify(&item);
    if mst == 0 {
        return Ok(());
    }
    // An update that fails (an item whose ACL is bound to an older build, a read-only keychain)
    // reports its own failure. The existing item is NEVER deleted as a fallback: a delete
    // followed by a failed add would destroy the stored credential.
    Err(map("set", mst))
}

struct KcOps<'a> {
    kc: &'a Handle,
    ns: &'a Namespace,
    account: &'a str,
    secret: &'a Secret,
}

impl<'a> KcOps<'a> {
    fn new(kc: &'a Handle, ns: &'a Namespace, account: &'a str, secret: &'a Secret) -> Self {
        Self {
            kc,
            ns,
            account,
            secret,
        }
    }
}

impl ItemOps for KcOps<'_> {
    type Item = Handle;
    fn add(&self) -> Result<OSStatus> {
        KeychainBackend::add(self.kc, self.ns, self.account, self.secret)
    }
    fn find_item(&self) -> (OSStatus, Option<Handle>) {
        let (st, _, item) = KeychainBackend::find(self.kc, self.ns, self.account, false, true);
        (st, (!item.is_null()).then(|| Handle(item)))
    }
    fn modify(&self, item: &Handle) -> OSStatus {
        let p = self.secret.expose_bytes();
        unsafe {
            SecKeychainItemModifyAttributesAndData(item.0, null(), p.len() as u32, p.as_ptr().cast())
        }
    }
}

impl Backend for KeychainBackend {
    fn name(&self) -> &'static str {
        match self.target {
            Target::UserDefault => "macos-keychain",
            _ => "macos-keychain-file",
        }
    }

    fn get(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<Option<Secret>> {
        check_target(ns, account)?;
        Self::with_ui(mode, || {
            let kc = self.keychain()?;
            match Self::find(&kc, ns, account, true, false) {
                (0, s, _) => Ok(s),
                (NOT_FOUND, _, _) => Ok(None),
                (st, _, _) => Err(map("get", st)),
            }
        })?
    }

    fn exists(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<bool> {
        check_target(ns, account)?;
        Self::with_ui(mode, || {
            let kc = self.keychain()?;
            match Self::find(&kc, ns, account, false, false).0 {
                0 => Ok(true),
                NOT_FOUND => Ok(false),
                st => Err(map("presence", st)),
            }
        })?
    }

    fn set(&self, ns: &Namespace, account: &str, secret: &Secret, mode: Interaction) -> Result<()> {
        check_target(ns, account)?;
        Self::with_ui(mode, || {
            let kc = self.keychain()?;
            set_flow(&KcOps::new(&kc, ns, account, secret))
        })?
    }

    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> Result<bool> {
        check_target(ns, account)?;
        Self::with_ui(mode, || {
            let kc = self.keychain()?;
            match Self::add(&kc, ns, account, secret)? {
                0 => Ok(true),
                DUPLICATE => Ok(false),
                st => Err(map("add", st)),
            }
        })?
    }

    fn delete(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<bool> {
        check_target(ns, account)?;
        Self::with_ui(mode, || {
            let kc = self.keychain()?;
            match Self::find(&kc, ns, account, false, true) {
                (0, _, item) => {
                    let h = Handle(item);
                    match unsafe { SecKeychainItemDelete(h.0) } {
                        0 => Ok(true),
                        st => Err(map("delete", st)),
                    }
                }
                (NOT_FOUND, _, _) => Ok(false),
                (st, _, _) => Err(map("delete", st)),
            }
        })?
    }
}

/// Current process-wide Keychain UI policy (`true` = dialogs allowed). For tests/diagnostics.
pub fn user_interaction_allowed() -> Result<bool> {
    let mut v = 1u8;
    let st = unsafe { SecKeychainGetUserInteractionAllowed(&mut v) };
    if st != 0 {
        Err(map("get_interaction", st))
    } else {
        Ok(v != 0)
    }
}

/// A throw-away keychain file used by end-to-end tests so they never see the user's real items.
/// It is never added to the search list; only its own handle is passed to the OS. Deleted on drop.
pub struct IsolatedKeychain {
    path: PathBuf,
    dir: PathBuf,
    password: String,
    handle: Arc<Handle>,
}

impl IsolatedKeychain {
    pub fn create() -> Result<Self> {
        let mut rnd = [0u8; 12];
        getrandom::fill(&mut rnd).map_err(|_| Error::Unavailable("OS CSPRNG failed".into()))?;
        let hex: String = rnd.iter().map(|b| format!("{b:02x}")).collect();
        let dir = std::env::temp_dir().join(format!("rightkit-secrets-test-{hex}"));
        std::fs::create_dir_all(&dir).map_err(|e| Error::Io(e.to_string()))?;
        let path = dir.join("isolated.keychain-db");
        let password = format!("pw-{hex}");
        let c = CString::new(path.to_string_lossy().as_bytes()).unwrap();
        let mut kc: *mut c_void = null_mut();
        let st = unsafe {
            SecKeychainCreate(
                c.as_ptr(),
                password.len() as u32,
                password.as_ptr().cast(),
                0,
                null(),
                &mut kc,
            )
        };
        if st != 0 || kc.is_null() {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(map("create_keychain", st));
        }
        Ok(Self {
            path,
            dir,
            password,
            handle: Arc::new(Handle(kc)),
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// The test keychain's password (needed by a child process that opens the same file).
    pub fn password(&self) -> &str {
        &self.password
    }
    pub fn backend(&self) -> KeychainBackend {
        KeychainBackend {
            target: Target::File(self.handle.clone()),
        }
    }
    pub fn lock(&self) -> Result<()> {
        match unsafe { SecKeychainLock(self.handle.0) } {
            0 => Ok(()),
            s => Err(map("lock", s)),
        }
    }
    pub fn unlock(&self) -> Result<()> {
        match unsafe {
            SecKeychainUnlock(
                self.handle.0,
                self.password.len() as u32,
                self.password.as_ptr().cast(),
                1,
            )
        } {
            0 => Ok(()),
            s => Err(map("unlock", s)),
        }
    }
}

impl Drop for IsolatedKeychain {
    fn drop(&mut self) {
        unsafe { SecKeychainDelete(self.handle.0) };
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keychain_path(h: &Handle) -> PathBuf {
        let mut buf = vec![0u8; 4096];
        let mut len = buf.len() as u32;
        let st = unsafe { SecKeychainGetPath(h.0, &mut len, buf.as_mut_ptr().cast()) };
        assert_eq!(st, 0, "SecKeychainGetPath");
        buf.truncate(len as usize);
        PathBuf::from(String::from_utf8(buf).unwrap())
    }

    /// A backend whose "user-domain default keychain" is `kc`, standing in for the real lookup.
    /// Every keychain a test uses is a throwaway file under the temp dir; the real default is
    /// never resolved here.
    fn default_is(kc: &IsolatedKeychain) -> KeychainBackend {
        let h = kc.handle.clone();
        let tmp = std::env::temp_dir().canonicalize().unwrap();
        let path = keychain_path(&h).canonicalize().unwrap();
        assert!(
            path.starts_with(&tmp),
            "test keychain outside temp dir: {path:?}"
        );
        assert!(!path.to_string_lossy().contains("login.keychain"));
        KeychainBackend {
            target: Target::Resolved(Arc::new(move || Ok(h.clone()))),
        }
    }

    fn secret(s: &str) -> Secret {
        Secret::from_string(s)
    }

    fn read(b: &KeychainBackend, ns: &Namespace, acct: &str) -> Option<String> {
        b.get(ns, acct, Interaction::NonInteractive)
            .unwrap()
            .map(|s| s.expose_str().unwrap().to_owned())
    }

    #[test]
    fn find_update_delete_hit_only_the_target_keychain() {
        let (a, b) = (
            IsolatedKeychain::create().unwrap(),
            IsolatedKeychain::create().unwrap(),
        );
        let ns = Namespace::new("rightkit.test.two-keychains").unwrap();
        let ni = Interaction::NonInteractive;
        // The same service and account holds a different secret in each keychain.
        a.backend()
            .set(&ns, "acct", &secret("secret-A"), ni)
            .unwrap();
        b.backend()
            .set(&ns, "acct", &secret("secret-B"), ni)
            .unwrap();
        let target = default_is(&b);

        // find
        assert_eq!(read(&target, &ns, "acct").as_deref(), Some("secret-B"));
        assert!(target.exists(&ns, "acct", ni).unwrap());

        // update: modifies B's item, leaves A's untouched
        target.set(&ns, "acct", &secret("secret-B2"), ni).unwrap();
        assert_eq!(read(&target, &ns, "acct").as_deref(), Some("secret-B2"));
        assert_eq!(read(&a.backend(), &ns, "acct").as_deref(), Some("secret-A"));

        // add_if_absent sees B's item as a duplicate and does not touch A
        assert!(!target.add_if_absent(&ns, "acct", &secret("x"), ni).unwrap());
        assert_eq!(read(&a.backend(), &ns, "acct").as_deref(), Some("secret-A"));

        // delete: removes B's item only
        assert!(target.delete(&ns, "acct", ni).unwrap());
        assert_eq!(read(&a.backend(), &ns, "acct").as_deref(), Some("secret-A"));

        // Not found in the target is NotFound even though another keychain has a match.
        assert_eq!(read(&target, &ns, "acct"), None);
        assert!(!target.exists(&ns, "acct", ni).unwrap());
        assert!(!target.delete(&ns, "acct", ni).unwrap());
        assert_eq!(read(&a.backend(), &ns, "acct").as_deref(), Some("secret-A"));
    }

    #[test]
    fn update_when_only_the_other_keychain_has_the_item_adds_to_the_target() {
        let (a, b) = (
            IsolatedKeychain::create().unwrap(),
            IsolatedKeychain::create().unwrap(),
        );
        let ns = Namespace::new("rightkit.test.other-only").unwrap();
        let ni = Interaction::NonInteractive;
        a.backend()
            .set(&ns, "acct", &secret("secret-A"), ni)
            .unwrap();
        let target = default_is(&b);
        target.set(&ns, "acct", &secret("secret-B"), ni).unwrap();
        assert_eq!(read(&target, &ns, "acct").as_deref(), Some("secret-B"));
        assert_eq!(read(&a.backend(), &ns, "acct").as_deref(), Some("secret-A"));
        assert!(target.delete(&ns, "acct", ni).unwrap());
        assert_eq!(read(&a.backend(), &ns, "acct").as_deref(), Some("secret-A"));
    }

    #[test]
    fn unresolvable_default_fails_closed_and_never_falls_back() {
        let a = IsolatedKeychain::create().unwrap();
        let ns = Namespace::new("rightkit.test.no-default").unwrap();
        let ni = Interaction::NonInteractive;
        a.backend()
            .set(&ns, "acct", &secret("secret-A"), ni)
            .unwrap();
        let none = KeychainBackend {
            target: Target::Resolved(Arc::new(|| {
                Err(map("default_keychain", NO_DEFAULT_KEYCHAIN))
            })),
        };
        assert!(matches!(
            none.get(&ns, "acct", ni),
            Err(Error::Unavailable(_))
        ));
        assert!(matches!(
            none.exists(&ns, "acct", ni),
            Err(Error::Unavailable(_))
        ));
        assert!(matches!(
            none.set(&ns, "acct", &secret("x"), ni),
            Err(Error::Unavailable(_))
        ));
        assert!(matches!(
            none.delete(&ns, "acct", ni),
            Err(Error::Unavailable(_))
        ));
        assert_eq!(read(&a.backend(), &ns, "acct").as_deref(), Some("secret-A"));
    }

    #[test]
    fn system_backend_resolves_per_call_and_has_no_null_keychain_target() {
        // `Target` has no search-list/null variant; the system backend is `UserDefault`.
        assert!(matches!(
            KeychainBackend::system().target,
            Target::UserDefault
        ));
        assert_eq!(KeychainBackend::system().name(), "macos-keychain");
    }

    // ---- audit finding 1: a failed update must never delete the existing item ----

    struct FakeOps {
        add_results: std::cell::RefCell<Vec<OSStatus>>,
        modify: OSStatus,
    }
    impl ItemOps for FakeOps {
        type Item = ();
        fn add(&self) -> Result<OSStatus> {
            let mut r = self.add_results.borrow_mut();
            Ok(if r.is_empty() { 0 } else { r.remove(0) })
        }
        fn find_item(&self) -> (OSStatus, Option<()>) {
            (0, Some(()))
        }
        fn modify(&self, _: &()) -> OSStatus {
            self.modify
        }
    }

    #[test]
    fn failed_interactive_update_returns_the_update_failure_and_never_deletes() {
        for (mst, expect) in [
            (READ_ONLY, Error::Backend { op: "set", code: READ_ONLY as i64 }),
            (AUTH_FAILED, Error::Denied),
        ] {
            // add -> duplicate; modify fails; a (hypothetical) delete would succeed and the
            // follow-up add would fail: the old code lost the credential here.
            let ops = FakeOps {
                add_results: std::cell::RefCell::new(vec![DUPLICATE, -25295]),
                modify: mst,
            };
            let r = set_flow(&ops);
            assert_eq!(r, Err(expect), "update failure {mst}");
        }
    }

    // ---- audit finding 2: invalid service/account is rejected at the backend boundary ----

    #[test]
    fn backend_rejects_empty_account_before_any_os_call() {
        let a = IsolatedKeychain::create().unwrap();
        let ns = Namespace::new("rightkit.test.wildcard").unwrap();
        let ni = Interaction::NonInteractive;
        let b = a.backend();
        b.set(&ns, "real", &secret("keep-me"), ni).unwrap();
        let long = "x".repeat(251);
        let bad = ["", "bad\naccount", long.as_str()];
        for acct in bad {
            assert!(matches!(b.delete(&ns, acct, ni), Err(Error::Invalid { .. })), "{acct:?}");
            assert!(matches!(b.get(&ns, acct, ni), Err(Error::Invalid { .. })));
            assert!(matches!(b.exists(&ns, acct, ni), Err(Error::Invalid { .. })));
            assert!(matches!(
                b.set(&ns, acct, &secret("x"), ni),
                Err(Error::Invalid { .. })
            ));
            assert!(matches!(
                b.add_if_absent(&ns, acct, &secret("x"), ni),
                Err(Error::Invalid { .. })
            ));
        }
        assert_eq!(read(&b, &ns, "real").as_deref(), Some("keep-me"));
    }

    // ---- audit finding 6: every default-keychain resolution failure is Unavailable ----

    #[test]
    fn every_default_resolution_failure_is_unavailable() {
        for st in [
            NO_SUCH_KEYCHAIN,
            NO_DEFAULT_KEYCHAIN,
            AUTH_FAILED,
            INTERACTION_NOT_ALLOWED,
            USER_CANCELED,
            -1,
            123456,
        ] {
            assert!(
                matches!(default_from_status(st, null_mut()), Err(Error::Unavailable(_))),
                "status {st}"
            );
        }
        // success with a null keychain reference
        assert!(matches!(
            default_from_status(0, null_mut()),
            Err(Error::Unavailable(_))
        ));
    }

    // ---- audit finding 4: the post-add check recognises which keychain an item lives in ----

    #[test]
    fn added_item_is_verified_against_its_own_keychain_only() {
        let (a, b) = (
            IsolatedKeychain::create().unwrap(),
            IsolatedKeychain::create().unwrap(),
        );
        let ns = Namespace::new("rightkit.test.verify-add").unwrap();
        // add() itself performs the verification and succeeds for the right keychain.
        assert_eq!(
            KeychainBackend::add(&a.handle, &ns, "acct", &secret("v")),
            Ok(0)
        );
        let (st, _, item) = KeychainBackend::find(&a.handle, &ns, "acct", false, true);
        assert_eq!(st, 0);
        let item = Handle(item);
        assert!(item_is_in(&item, &a.handle));
        // The same item is not "in" the other temporary keychain: this is the check that
        // catches a native add that landed in a different keychain.
        assert!(!item_is_in(&item, &b.handle));
        // Nothing was written to B.
        assert_eq!(read(&b.backend(), &ns, "acct"), None);
    }

    /// The macOS e2e suite may not construct the real default-keychain backend, so none of it
    /// reads, writes or deletes in the login keychain. Needles are assembled at run time so this
    /// file does not match itself; the unit tests above only construct `system()` (no OS call)
    /// and use temp keychains elsewhere (`default_is` asserts each path is under the temp dir).
    #[test]
    fn e2e_suite_never_constructs_the_login_keychain_backend() {
        let needles = [
            format!("{}::{}(", "KeychainBackend", "system"),
            format!("{}(", "system_backend"),
        ];
        let text =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e.rs"))
                .unwrap();
        for n in &needles {
            assert!(!text.contains(n.as_str()), "tests/e2e.rs uses {n}");
        }
    }
}
