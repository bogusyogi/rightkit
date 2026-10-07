//! macOS login-keychain backend over the legacy file-keychain API (the same generic-password
//! items `keyring` and `security-framework` read, so existing items stay readable).
//!
//! Presence uses `SecKeychainFindGenericPassword` requesting neither the password nor an item
//! reference, so no secret bytes are fetched and no ACL authorization is evaluated. Every call
//! sets the process-wide UI policy first; non-interactive calls fail with
//! `errSecInteractionNotAllowed` instead of showing a dialog.

use crate::backend::{Backend, Interaction};
use crate::error::{Error, Result};
use crate::namespace::Namespace;
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
const READ_ONLY: OSStatus = -25292;

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

/// The login keychain (default search list), or one explicit keychain file.
pub struct KeychainBackend {
    keychain: Option<Arc<Handle>>,
}

impl KeychainBackend {
    pub fn system() -> Self {
        Self { keychain: None }
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
        Ok(Self { keychain: Some(h) })
    }
    fn kc_ptr(&self) -> *mut c_void {
        self.keychain.as_ref().map_or(null_mut(), |h| h.0)
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
        &self,
        ns: &Namespace,
        account: &str,
        want_data: bool,
        want_item: bool,
    ) -> (OSStatus, Option<Secret>, *mut c_void) {
        let (s, a) = (ns.service(), account);
        let (mut len, mut data, mut item) = (0u32, null_mut::<c_void>(), null_mut::<c_void>());
        let st = unsafe {
            SecKeychainFindGenericPassword(
                self.kc_ptr(),
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

    fn add(&self, ns: &Namespace, account: &str, secret: &Secret) -> OSStatus {
        let (s, a, p) = (ns.service(), account, secret.expose_bytes());
        unsafe {
            SecKeychainAddGenericPassword(
                self.kc_ptr(),
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

impl Backend for KeychainBackend {
    fn name(&self) -> &'static str {
        if self.keychain.is_some() {
            "macos-keychain-file"
        } else {
            "macos-keychain"
        }
    }

    fn get(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<Option<Secret>> {
        Self::with_ui(mode, || match self.find(ns, account, true, false) {
            (0, s, _) => Ok(s),
            (NOT_FOUND, _, _) => Ok(None),
            (st, _, _) => Err(map("get", st)),
        })?
    }

    fn exists(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<bool> {
        Self::with_ui(mode, || match self.find(ns, account, false, false).0 {
            0 => Ok(true),
            NOT_FOUND => Ok(false),
            st => Err(map("presence", st)),
        })?
    }

    fn set(&self, ns: &Namespace, account: &str, secret: &Secret, mode: Interaction) -> Result<()> {
        Self::with_ui(mode, || {
            let st = self.add(ns, account, secret);
            if st == 0 {
                return Ok(());
            }
            if st != DUPLICATE {
                return Err(map("set", st));
            }
            let (fst, _, item) = self.find(ns, account, false, true);
            if fst != 0 {
                return Err(map("set", fst));
            }
            let h = Handle(item);
            let p = secret.expose_bytes();
            let mst = unsafe {
                SecKeychainItemModifyAttributesAndData(
                    h.0,
                    null(),
                    p.len() as u32,
                    p.as_ptr().cast(),
                )
            };
            if mst == 0 {
                return Ok(());
            }
            // An item whose ACL is bound to an older build cannot be modified silently. Only a
            // user-initiated save may replace it (delete + add), which can raise the OS prompt.
            if mode == Interaction::Interactive && (mst == AUTH_FAILED || mst == READ_ONLY) {
                let dst = unsafe { SecKeychainItemDelete(h.0) };
                if dst != 0 {
                    return Err(map("set", dst));
                }
                let ast = self.add(ns, account, secret);
                return if ast == 0 {
                    Ok(())
                } else {
                    Err(map("set", ast))
                };
            }
            Err(map("set", mst))
        })?
    }

    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> Result<bool> {
        Self::with_ui(mode, || match self.add(ns, account, secret) {
            0 => Ok(true),
            DUPLICATE => Ok(false),
            st => Err(map("add", st)),
        })?
    }

    fn delete(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<bool> {
        Self::with_ui(mode, || match self.find(ns, account, false, true) {
            (0, _, item) => {
                let h = Handle(item);
                match unsafe { SecKeychainItemDelete(h.0) } {
                    0 => Ok(true),
                    st => Err(map("delete", st)),
                }
            }
            (NOT_FOUND, _, _) => Ok(false),
            (st, _, _) => Err(map("delete", st)),
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
            keychain: Some(self.handle.clone()),
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
