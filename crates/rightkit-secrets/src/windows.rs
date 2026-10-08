//! Windows Credential Manager backend (generic credentials, per-user, persisted locally).
//! Credential Manager never prompts. It has no attribute-only query, so presence reads the
//! credential through `CredReadW` and drops it without copying the blob.

use crate::backend::{Backend, Interaction};
use crate::error::{Error, Result};
use crate::namespace::Namespace;
use crate::secret::Secret;
use std::sync::Mutex;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_NOT_FOUND, HANDLE, WAIT_ABANDONED, WAIT_FAILED, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
    CRED_TYPE_GENERIC,
};
use windows_sys::Win32::Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE};
use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn code(op: &'static str, err: u32) -> Error {
    Error::Backend {
        op,
        code: err as i64,
    }
}

pub struct CredentialManagerBackend;

/// Retained process-local serialization; `AccountLock` also covers other processes.
static ADD_LOCK: Mutex<()> = Mutex::new(());

struct AccountLock {
    handle: HANDLE,
    acquired: bool,
}

impl AccountLock {
    fn acquire(ns: &Namespace, account: &str) -> Result<Self> {
        let input = format!("{}\0{}", ns.service(), account);
        let mut digest = [0u8; 32];
        unsafe {
            let status = BCryptHash(
                BCRYPT_SHA256_ALG_HANDLE,
                std::ptr::null(),
                0,
                input.as_ptr(),
                input.len() as u32,
                digest.as_mut_ptr(),
                digest.len() as u32,
            );
            if status < 0 {
                return Err(Error::Backend {
                    op: "lock hash",
                    code: status as i64,
                });
            }
            let suffix: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
            let name = wide(&format!("Local\\rightkit-secrets-{suffix}"));
            let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            if handle.is_null() {
                return Err(code("lock create", GetLastError()));
            }
            let mut guard = Self {
                handle,
                acquired: false,
            };
            match WaitForSingleObject(handle, 10_000) {
                WAIT_OBJECT_0 | WAIT_ABANDONED => {
                    guard.acquired = true;
                    Ok(guard)
                }
                WAIT_TIMEOUT => Err(Error::Timeout {
                    op: "lock",
                    secs: 10,
                }),
                WAIT_FAILED => Err(code("lock wait", GetLastError())),
                other => Err(code("lock wait", other)),
            }
        }
    }
}

impl Drop for AccountLock {
    fn drop(&mut self) {
        unsafe {
            if self.acquired {
                ReleaseMutex(self.handle);
            }
            CloseHandle(self.handle);
        }
    }
}

impl CredentialManagerBackend {
    fn read(&self, ns: &Namespace, account: &str, want_blob: bool) -> Result<Option<Secret>> {
        let name = wide(&ns.windows_target_name(account));
        unsafe {
            let mut p: *mut CREDENTIALW = std::ptr::null_mut();
            if CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &mut p) == 0 {
                let e = GetLastError();
                return if e == ERROR_NOT_FOUND {
                    Ok(None)
                } else {
                    Err(code("read", e))
                };
            }
            if p.is_null() {
                return Err(Error::Backend {
                    op: "read",
                    code: 0,
                });
            }
            let out = if want_blob {
                let c = &*p;
                let bytes = if c.CredentialBlob.is_null() || c.CredentialBlobSize == 0 {
                    Vec::new()
                } else {
                    std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize)
                        .to_vec()
                };
                Secret::from_bytes(bytes)
            } else {
                Secret::from_bytes(Vec::new())
            };
            CredFree(p as _);
            Ok(Some(out))
        }
    }
}

impl Backend for CredentialManagerBackend {
    fn name(&self) -> &'static str {
        "windows-credential-manager"
    }
    fn get(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<Option<Secret>> {
        self.read(ns, account, true)
    }
    fn exists(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<bool> {
        Ok(self.read(ns, account, false)?.is_some())
    }
    fn set(&self, ns: &Namespace, account: &str, secret: &Secret, _: Interaction) -> Result<()> {
        let name = wide(&ns.windows_target_name(account));
        let user = wide(account);
        let blob = secret.expose_bytes();
        unsafe {
            let mut cred: CREDENTIALW = std::mem::zeroed();
            cred.Type = CRED_TYPE_GENERIC;
            cred.TargetName = name.as_ptr() as *mut u16;
            cred.UserName = user.as_ptr() as *mut u16;
            cred.CredentialBlobSize = blob.len() as u32;
            cred.CredentialBlob = blob.as_ptr() as *mut u8;
            cred.Persist = CRED_PERSIST_LOCAL_MACHINE;
            if CredWriteW(&cred, 0) == 0 {
                return Err(code("set", GetLastError()));
            }
        }
        Ok(())
    }
    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> Result<bool> {
        let _g = ADD_LOCK
            .lock()
            .map_err(|_| Error::Unavailable("lock poisoned".into()))?;
        let _account = AccountLock::acquire(ns, account)?;
        if self.exists(ns, account, mode)? {
            return Ok(false);
        }
        self.set(ns, account, secret, mode)?;
        Ok(true)
    }
    fn delete(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<bool> {
        let name = wide(&ns.windows_target_name(account));
        unsafe {
            if CredDeleteW(name.as_ptr(), CRED_TYPE_GENERIC, 0) == 0 {
                let e = GetLastError();
                return if e == ERROR_NOT_FOUND {
                    Ok(false)
                } else {
                    Err(code("delete", e))
                };
            }
        }
        Ok(true)
    }
}
