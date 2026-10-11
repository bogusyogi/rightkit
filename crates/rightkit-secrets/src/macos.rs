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
//! ## Where a write can land
//! Apple's creation calls do not fail closed for an explicit keychain. Read from Apple's
//! published source (not reproduced at run time): `SecKeychainAddGenericPassword` and `SecItemAdd`
//! with `kSecUseKeychain` both end in `SecKeychainItemCreateFromContent`, which catches EVERY
//! failure to use the requested keychain (the reference is invalid, or the keychain is deleted
//! or not available) and retries through the default keychain. No public creation API takes an
//! explicit keychain without that retry, and a check made after the write cannot undo a secret
//! already stored in the wrong keychain (a crash, or a failed delete, leaves it there). So the
//! guarantee is made BEFORE the write:
//!
//! * The user-domain default target: the retry's destination IS the requested one (the default
//!   keychain), so a write can only land where it was asked to.
//! * Any other explicit keychain ([`KeychainBackend::open_file`]): a write is refused with
//!   `Error::Unavailable` unless the keychain is the current default. Reads and deletes still
//!   work on it.
//! * Throwaway test keychains ([`IsolatedKeychain`], [`KeychainBackend::open_isolated_file`]):
//!   writes are allowed. They hold fake data in the temp directory; the residual risk, that the
//!   file is deleted in the instant between the pre-write checks and the add, would put a
//!   fixture secret in the default keychain.
//!
//! Before every write the destination is also checked to be a valid keychain reference whose
//! file exists, and unlocked (a locked one is `Error::InteractionRequired` when the call is
//! non-interactive). The native add is not asked for the item reference and nothing is
//! relocated or deleted afterwards.
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
    fn SecKeychainGetStatus(kc: *const c_void, status: *mut u32) -> OSStatus;
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
/// `kSecUnlockStateStatus`: set in the `SecKeychainGetStatus` result while the keychain is unlocked.
const STATUS_UNLOCKED: u32 = 1;
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
    /// One explicit keychain file. `fixture` is true only for a throwaway test keychain, the one
    /// kind of explicit keychain that may be written to (see the module docs).
    File { handle: Arc<Handle>, fixture: bool },
    /// Test seam: a resolver standing in for the user-domain default lookup. Fixture keychains
    /// only.
    #[cfg(test)]
    Resolved(Arc<dyn Fn() -> Result<Arc<Handle>> + Send + Sync>),
}

impl Target {
    /// True when a write may only go ahead if the keychain is the current default, because the
    /// native add's fallback would otherwise put the secret in a different keychain.
    fn write_needs_default_match(&self) -> bool {
        matches!(self, Target::File { fixture: false, .. })
    }
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

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// File of the current user-domain default keychain, or `None` if it cannot be resolved.
fn user_default_path() -> Option<PathBuf> {
    let kc = user_default_keychain().ok()?;
    keychain_path(kc.0)
}

/// The OS calls a write needs, behind a trait so the decision whether to make the add can be
/// tested without a keychain that disappears at a chosen moment.
trait Native {
    /// `SecKeychainGetStatus` bits, or the failing status for an invalid keychain reference.
    fn status(&self, kc: &Handle) -> std::result::Result<u32, OSStatus>;
    /// Path of the keychain file.
    fn path(&self, kc: &Handle) -> Option<PathBuf>;
    /// The native add into `kc`. Nothing is read back or relocated afterwards.
    fn add(&self, kc: &Handle, ns: &Namespace, account: &str, secret: &Secret) -> OSStatus;
}

struct Os;

impl Native for Os {
    fn status(&self, kc: &Handle) -> std::result::Result<u32, OSStatus> {
        let mut bits = 0u32;
        match unsafe { SecKeychainGetStatus(kc.0, &mut bits) } {
            0 => Ok(bits),
            st => Err(st),
        }
    }
    fn path(&self, kc: &Handle) -> Option<PathBuf> {
        keychain_path(kc.0)
    }
    fn add(&self, kc: &Handle, ns: &Namespace, account: &str, secret: &Secret) -> OSStatus {
        let (s, a, p) = (ns.service(), account, secret.expose_bytes());
        unsafe {
            SecKeychainAddGenericPassword(
                kc.0,
                s.len() as u32,
                s.as_ptr().cast(),
                a.len() as u32,
                a.as_ptr().cast(),
                p.len() as u32,
                p.as_ptr().cast(),
                null_mut(),
            )
        }
    }
}

/// Check the destination, then add. Every check happens BEFORE the add, because the add itself
/// falls back to the default keychain when the requested one is unusable (module docs).
/// `must_be_default` is the default-keychain probe for an explicit keychain that is not a test
/// fixture: such a write is refused unless that keychain is the default.
fn add_checked<N: Native>(
    native: &N,
    dest: &Handle,
    ns: &Namespace,
    account: &str,
    secret: &Secret,
    mode: Interaction,
    must_be_default: Option<&dyn Fn() -> Option<PathBuf>>,
) -> Result<OSStatus> {
    let bits = native.status(dest).map_err(|st| {
        Error::Unavailable(format!("destination keychain is not valid (code {st})"))
    })?;
    let path = native
        .path(dest)
        .filter(|p| p.exists())
        .ok_or_else(|| Error::Unavailable("destination keychain file is not available".into()))?;
    if bits & STATUS_UNLOCKED == 0 && mode == Interaction::NonInteractive {
        return Err(Error::InteractionRequired);
    }
    if let Some(default_path) = must_be_default {
        match default_path() {
            Some(default) if same_file(&path, &default) => {}
            _ => {
                return Err(Error::Unavailable(
                    "refusing to write: a write to a keychain other than the user default cannot be guaranteed to stay in it"
                        .into(),
                ))
            }
        }
    }
    Ok(native.add(dest, ns, account, secret))
}

impl KeychainBackend {
    /// The user-domain default keychain. Operations never reach other keychains on the search
    /// list.
    pub fn system() -> Self {
        Self {
            target: Target::UserDefault,
        }
    }
    /// Open an existing keychain file; ONLY that file is searched. Reads and deletes work on it.
    /// A write is refused with `Error::Unavailable` unless this keychain is the current default
    /// (see the module docs: the OS add would otherwise fall back to the default keychain if this
    /// file went missing). For a throwaway test keychain use [`Self::open_isolated_file`].
    pub fn open_file(path: &Path, password: Option<&str>) -> Result<Self> {
        Self::open_file_with(path, password, false)
    }

    /// Open a throwaway test keychain file (one made by [`IsolatedKeychain`]) for reading AND
    /// writing. Only a file under the OS temp directory is accepted. Test fixtures only: if the
    /// file were deleted in the instant between the pre-write checks and the add, the OS would
    /// store the fixture secret in the default keychain.
    pub fn open_isolated_file(path: &Path, password: Option<&str>) -> Result<Self> {
        let temp = std::env::temp_dir()
            .canonicalize()
            .map_err(|e| Error::Io(e.to_string()))?;
        let canonical = path.canonicalize().map_err(|_| Error::Invalid {
            what: "path",
            reason: "cannot be resolved".into(),
        })?;
        if !canonical.starts_with(&temp) {
            return Err(Error::Invalid {
                what: "path",
                reason: "an isolated keychain must be under the temp directory".into(),
            });
        }
        Self::open_file_with(path, password, true)
    }

    fn open_file_with(path: &Path, password: Option<&str>, fixture: bool) -> Result<Self> {
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
            target: Target::File { handle: h, fixture },
        })
    }
    /// The keychain this call operates on; always a concrete keychain, never null.
    fn keychain(&self) -> Result<Arc<Handle>> {
        match &self.target {
            Target::UserDefault => user_default_keychain(),
            Target::File { handle, .. } => Ok(handle.clone()),
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

    /// Add a generic password to `kc` after the pre-write checks (see [`add_checked`]).
    fn add(
        &self,
        kc: &Handle,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> Result<OSStatus> {
        let default_probe = user_default_path;
        let must_be_default: Option<&dyn Fn() -> Option<PathBuf>> =
            if self.target.write_needs_default_match() {
                Some(&default_probe)
            } else {
                None
            };
        add_checked(&Os, kc, ns, account, secret, mode, must_be_default)
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
    backend: &'a KeychainBackend,
    kc: &'a Handle,
    ns: &'a Namespace,
    account: &'a str,
    secret: &'a Secret,
    mode: Interaction,
}

impl<'a> KcOps<'a> {
    fn new(
        backend: &'a KeychainBackend,
        kc: &'a Handle,
        ns: &'a Namespace,
        account: &'a str,
        secret: &'a Secret,
        mode: Interaction,
    ) -> Self {
        Self {
            backend,
            kc,
            ns,
            account,
            secret,
            mode,
        }
    }
}

impl ItemOps for KcOps<'_> {
    type Item = Handle;
    fn add(&self) -> Result<OSStatus> {
        self.backend
            .add(self.kc, self.ns, self.account, self.secret, self.mode)
    }
    fn find_item(&self) -> (OSStatus, Option<Handle>) {
        let (st, _, item) = KeychainBackend::find(self.kc, self.ns, self.account, false, true);
        (st, (!item.is_null()).then(|| Handle(item)))
    }
    fn modify(&self, item: &Handle) -> OSStatus {
        let p = self.secret.expose_bytes();
        unsafe {
            SecKeychainItemModifyAttributesAndData(
                item.0,
                null(),
                p.len() as u32,
                p.as_ptr().cast(),
            )
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
            set_flow(&KcOps::new(self, &kc, ns, account, secret, mode))
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
            match self.add(&kc, ns, account, secret, mode)? {
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
            target: Target::File {
                handle: self.handle.clone(),
                fixture: true,
            },
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
            (
                READ_ONLY,
                Error::Backend {
                    op: "set",
                    code: READ_ONLY as i64,
                },
            ),
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
            assert!(
                matches!(b.delete(&ns, acct, ni), Err(Error::Invalid { .. })),
                "{acct:?}"
            );
            assert!(matches!(b.get(&ns, acct, ni), Err(Error::Invalid { .. })));
            assert!(matches!(
                b.exists(&ns, acct, ni),
                Err(Error::Invalid { .. })
            ));
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
                matches!(
                    default_from_status(st, null_mut()),
                    Err(Error::Unavailable(_))
                ),
                "status {st}"
            );
        }
        // success with a null keychain reference
        assert!(matches!(
            default_from_status(0, null_mut()),
            Err(Error::Unavailable(_))
        ));
    }

    // ---- second-read finding 2: a write is gated BEFORE the native add, never repaired after ----

    /// Scripted OS answers; counts how many native adds were attempted.
    struct FakeNative {
        status: std::result::Result<u32, OSStatus>,
        path: Option<PathBuf>,
        adds: std::cell::Cell<u32>,
    }

    impl FakeNative {
        fn ready(path: PathBuf) -> Self {
            Self {
                status: Ok(STATUS_UNLOCKED),
                path: Some(path),
                adds: std::cell::Cell::new(0),
            }
        }
    }

    impl Native for FakeNative {
        fn status(&self, _: &Handle) -> std::result::Result<u32, OSStatus> {
            self.status
        }
        fn path(&self, _: &Handle) -> Option<PathBuf> {
            self.path.clone()
        }
        fn add(&self, _: &Handle, _: &Namespace, _: &str, _: &Secret) -> OSStatus {
            self.adds.set(self.adds.get() + 1);
            0
        }
    }

    fn existing_path() -> PathBuf {
        std::env::temp_dir()
    }

    fn missing_path() -> PathBuf {
        std::env::temp_dir().join("rightkit-secrets-no-such-keychain.keychain-db")
    }

    fn try_add(
        native: &FakeNative,
        mode: Interaction,
        must_be_default: Option<&dyn Fn() -> Option<PathBuf>>,
    ) -> Result<OSStatus> {
        let ns = Namespace::new("rightkit.test.gate").unwrap();
        add_checked(
            native,
            &Handle(null_mut()),
            &ns,
            "acct",
            &secret("v"),
            mode,
            must_be_default,
        )
    }

    #[test]
    fn no_add_is_attempted_for_an_invalid_keychain_reference() {
        let mut native = FakeNative::ready(existing_path());
        native.status = Err(NO_SUCH_KEYCHAIN);
        assert!(matches!(
            try_add(&native, Interaction::Interactive, None),
            Err(Error::Unavailable(_))
        ));
        assert_eq!(native.adds.get(), 0);
    }

    #[test]
    fn no_add_is_attempted_when_the_keychain_file_is_gone_or_unknown() {
        for path in [Some(missing_path()), None] {
            let mut native = FakeNative::ready(existing_path());
            native.path = path;
            assert!(matches!(
                try_add(&native, Interaction::Interactive, None),
                Err(Error::Unavailable(_))
            ));
            assert_eq!(native.adds.get(), 0);
        }
    }

    #[test]
    fn a_locked_keychain_blocks_a_non_interactive_add_before_the_native_call() {
        let mut native = FakeNative::ready(existing_path());
        native.status = Ok(0);
        assert!(matches!(
            try_add(&native, Interaction::NonInteractive, None),
            Err(Error::InteractionRequired)
        ));
        assert_eq!(native.adds.get(), 0);
        // An explicit user action may let the OS prompt to unlock.
        assert_eq!(try_add(&native, Interaction::Interactive, None), Ok(0));
        assert_eq!(native.adds.get(), 1);
    }

    #[test]
    fn a_ready_destination_gets_exactly_one_native_add() {
        let native = FakeNative::ready(existing_path());
        assert_eq!(try_add(&native, Interaction::NonInteractive, None), Ok(0));
        assert_eq!(native.adds.get(), 1);
    }

    #[test]
    fn an_explicit_keychain_that_is_not_the_default_is_never_written() {
        let other = existing_path();
        for probe in [
            Some(missing_path()),
            None, // the default cannot be resolved
        ] {
            let native = FakeNative::ready(other.clone());
            let default_is = move || probe.clone();
            assert!(matches!(
                try_add(&native, Interaction::NonInteractive, Some(&default_is)),
                Err(Error::Unavailable(_))
            ));
            assert_eq!(native.adds.get(), 0);
        }
        // The same keychain as the default: the add's fallback target is the requested one.
        let native = FakeNative::ready(other.clone());
        let default_is = || Some(other.clone());
        assert_eq!(
            try_add(&native, Interaction::NonInteractive, Some(&default_is)),
            Ok(0)
        );
        assert_eq!(native.adds.get(), 1);
    }

    #[test]
    fn open_file_reads_and_deletes_but_refuses_to_write_to_a_non_default_keychain() {
        let kc = IsolatedKeychain::create().unwrap();
        let plain = KeychainBackend::open_file(kc.path(), Some(kc.password())).unwrap();
        let ns = Namespace::new("rightkit.test.refuse-explicit").unwrap();
        let ni = Interaction::NonInteractive;
        assert!(matches!(
            plain.set(&ns, "acct", &secret("v"), ni),
            Err(Error::Unavailable(_))
        ));
        assert!(matches!(
            plain.add_if_absent(&ns, "acct", &secret("v"), ni),
            Err(Error::Unavailable(_))
        ));
        assert_eq!(
            read(&kc.backend(), &ns, "acct"),
            None,
            "nothing was written"
        );

        kc.backend()
            .set(&ns, "acct", &secret("fixture"), ni)
            .unwrap();
        assert_eq!(read(&plain, &ns, "acct").as_deref(), Some("fixture"));
        assert!(plain.exists(&ns, "acct", ni).unwrap());
        assert!(plain.delete(&ns, "acct", ni).unwrap());
    }

    #[test]
    fn open_isolated_file_writes_to_a_temp_keychain_and_refuses_anything_else() {
        let kc = IsolatedKeychain::create().unwrap();
        let isolated = KeychainBackend::open_isolated_file(kc.path(), Some(kc.password())).unwrap();
        let ns = Namespace::new("rightkit.test.isolated-open").unwrap();
        let ni = Interaction::NonInteractive;
        isolated.set(&ns, "acct", &secret("v"), ni).unwrap();
        assert_eq!(read(&isolated, &ns, "acct").as_deref(), Some("v"));
        // Outside the temp directory: refused before the file is opened.
        assert!(matches!(
            KeychainBackend::open_isolated_file(Path::new("/usr/bin/true"), None),
            Err(Error::Invalid { .. })
        ));
    }

    /// The post-write relocation path (ask for the item, compare its keychain, delete it) is gone:
    /// it could not undo a secret already stored in the wrong keychain.
    #[test]
    fn production_code_has_no_post_write_relocation() {
        let text =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/macos.rs"))
                .unwrap();
        let marker = format!("{}\nmod {} {{", "#[cfg(test)]", "tests");
        let production = text.split(marker.as_str()).next().unwrap();
        for needle in [
            format!("{}{}", "SecKeychainItem", "CopyKeychain"),
            format!("{}{}", "item_is", "_in"),
            format!("{}{}", "could not be", " removed"),
        ] {
            assert!(
                !production.contains(&needle),
                "production code still has {needle}"
            );
        }
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
