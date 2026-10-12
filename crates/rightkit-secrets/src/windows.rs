//! Windows Credential Manager backend (generic credentials, per-user, persisted locally).
//! Credential Manager never prompts. It has no attribute-only query, so presence reads the
//! credential through `CredReadW` and drops it without copying the blob.
//!
//! Credential Manager has no atomic "write if absent" or "delete if equal", so every mutation
//! (`set`, `add_if_absent`, `delete`, `delete_if_equals`) runs under a named mutex keyed on the
//! normalised native target name AND the current user's SID, in the `Global\` kernel namespace
//! with a DACL that grants only that SID (see `locked.rs`); reads take no lock. Credential Manager
//! entries persisted with `CRED_PERSIST_LOCAL_MACHINE` are visible to every logon session of the
//! same user, so a session-local (`Local\`) mutex would not exclude a writer in another session. The target name is the
//! validated one (`Namespace::windows_target_checked`): no NUL or control character, within the
//! native length limit.

use crate::backend::{Backend, ConditionalDelete, Interaction};
use crate::error::{Error, Result};
use crate::keyring_compat;
use crate::locked::{self, CredentialOps};
use crate::namespace::{check_target, windows_credential_identity, BlobCodec, Namespace};
use crate::secret::Secret;
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_NOT_FOUND, HANDLE, WAIT_ABANDONED, WAIT_FAILED,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
};
use windows_sys::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
    CRED_TYPE_GENERIC,
};
use windows_sys::Win32::Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenUser, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcess, OpenProcessToken, ReleaseMutex, WaitForSingleObject,
};

/// How long a mutation waits for another writer of the same target before giving up.
const LOCK_WAIT_MS: u32 = 10_000;

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

/// The string form of the current user's SID (`S-1-5-21-...`), read once from the process token.
fn user_sid() -> Result<&'static str> {
    static SID: OnceLock<String> = OnceLock::new();
    if let Some(sid) = SID.get() {
        return Ok(sid.as_str());
    }
    let sid = query_user_sid()?;
    Ok(SID.get_or_init(|| sid).as_str())
}

fn query_user_sid() -> Result<String> {
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(code("lock sid token", GetLastError()));
        }
        let mut needed = 0u32;
        // Sizing call: fails with ERROR_INSUFFICIENT_BUFFER and reports the size.
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
        if needed == 0 {
            let e = GetLastError();
            CloseHandle(token);
            return Err(code("lock sid size", e));
        }
        // u64 elements keep the buffer aligned for TOKEN_USER.
        let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
        let ok = GetTokenInformation(
            token,
            TokenUser,
            buf.as_mut_ptr().cast(),
            needed,
            &mut needed,
        );
        let e = GetLastError();
        CloseHandle(token);
        if ok == 0 {
            return Err(code("lock sid read", e));
        }
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut text: *mut u16 = std::ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 || text.is_null() {
            return Err(code("lock sid text", GetLastError()));
        }
        let mut len = 0usize;
        while *text.add(len) != 0 {
            len += 1;
        }
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
        LocalFree(text.cast());
        Ok(sid)
    }
}

/// The named mutex for one normalised native target under the current user:
/// `Global\rightkit-secrets-t-<hash of user SID and target>`. `Global\` makes it visible to every
/// logon session of this user, which is the scope of a `CRED_PERSIST_LOCAL_MACHINE` entry; the
/// SID in the name keeps one user's locks apart from another's (and from a squatter's: the mutex
/// is created with a DACL granting only this SID, so another user cannot open or hold it).
fn lock_name(normalized_target: &str) -> Result<Vec<u16>> {
    let sid = user_sid()?;
    let suffix = hash_suffix(&format!(
        "rightkit-secrets/target/v2\0{sid}\0{normalized_target}"
    ))?;
    Ok(wide(&format!("Global\\rightkit-secrets-t-{suffix}")))
}

/// The mutex name 0.1.0 to 0.1.3 took for one service and account:
/// `Local\rightkit-secrets-<hash of service, NUL, account>`. A process built on one of those
/// versions takes only this name, so this version takes it as well (see [`lock_both`]).
fn legacy_lock_name(service: &str, account: &str) -> Result<Vec<u16>> {
    let suffix = hash_suffix(&format!("{service}\0{account}"))?;
    Ok(wide(&format!("Local\\rightkit-secrets-{suffix}")))
}

/// The first 16 bytes of the SHA-256 of `input`, as lowercase hex.
fn hash_suffix(input: &str) -> Result<String> {
    let mut digest = [0u8; 32];
    let status = unsafe {
        BCryptHash(
            BCRYPT_SHA256_ALG_HANDLE,
            std::ptr::null(),
            0,
            input.as_ptr(),
            input.len() as u32,
            digest.as_mut_ptr(),
            digest.len() as u32,
        )
    };
    if status < 0 {
        return Err(Error::Backend {
            op: "lock hash",
            code: status as i64,
        });
    }
    Ok(digest[..16].iter().map(|b| format!("{b:02x}")).collect())
}

/// Held while one thread mutates one target. Named mutexes are owned by a thread, so the guard
/// is released on the thread that took it (the worker thread of one `Store` call).
pub(crate) struct TargetLock {
    handle: HANDLE,
    acquired: bool,
}

impl TargetLock {
    fn acquire(normalized_target: &str, wait_ms: u32) -> Result<Self> {
        let name = lock_name(normalized_target)?;
        unsafe {
            // Owner-only DACL: generic-all for this user's SID, nobody else.
            let sddl = wide(&format!("D:(A;;GA;;;{})", user_sid()?));
            let mut descriptor: *mut std::ffi::c_void = std::ptr::null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1, // SDDL_REVISION_1
                &mut descriptor,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(code("lock acl", GetLastError()));
            }
            let attributes = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            };
            let handle = CreateMutexW(&attributes, 0, name.as_ptr());
            let create_error = GetLastError();
            LocalFree(descriptor);
            if handle.is_null() {
                // Includes access denied: the name is held by another user, so fail closed.
                return Err(code("lock create", create_error));
            }
            Self::wait(handle, wait_ms)
        }
    }

    /// The mutex of 0.1.0 to 0.1.3 for this service and account, created as those versions
    /// create it (default security, `Local\`), so both name and object are the ones they use.
    fn acquire_legacy(service: &str, account: &str, wait_ms: u32) -> Result<Self> {
        let name = legacy_lock_name(service, account)?;
        unsafe {
            let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            if handle.is_null() {
                return Err(code("legacy lock create", GetLastError()));
            }
            Self::wait(handle, wait_ms)
        }
    }

    /// Waits for an open mutex handle, which it owns from here on.
    unsafe fn wait(handle: HANDLE, wait_ms: u32) -> Result<Self> {
        // `guard` closes the handle on every path below.
        let mut guard = Self {
            handle,
            acquired: false,
        };
        match WaitForSingleObject(handle, wait_ms) {
            WAIT_OBJECT_0 | WAIT_ABANDONED => {
                guard.acquired = true;
                Ok(guard)
            }
            WAIT_TIMEOUT => Err(Error::Timeout {
                op: "lock",
                secs: u64::from(wait_ms / 1000),
            }),
            WAIT_FAILED => Err(code("lock wait", GetLastError())),
            other => Err(code("lock wait", other)),
        }
    }
}

/// Both locks of one mutation, always taken in this order: the target lock of this version,
/// then the lock 0.1.0 to 0.1.3 take for the same service and account. An older process takes
/// only the second, so while old and new builds of an app run side by side they still exclude
/// each other (two of them creating one database key would otherwise each get a different key).
/// The order is fixed and an older process holds one lock at most, so no cycle can form.
/// Failing to take either is an error, never an unlocked write.
fn lock_both(
    normalized_target: &str,
    service: &str,
    account: &str,
    wait_ms: u32,
) -> Result<(TargetLock, TargetLock)> {
    let target = TargetLock::acquire(normalized_target, wait_ms)?;
    let legacy = TargetLock::acquire_legacy(service, account, wait_ms)?;
    Ok((target, legacy))
}

impl Drop for TargetLock {
    fn drop(&mut self) {
        unsafe {
            if self.acquired {
                ReleaseMutex(self.handle);
            }
            CloseHandle(self.handle);
        }
    }
}

/// The unlocked Credential Manager primitives. Callers hold the target lock (`locked.rs`).
struct Win32;

impl CredentialOps for Win32 {
    type Guard = (TargetLock, TargetLock);

    fn lock(
        &self,
        normalized_target: &str,
        ns: &Namespace,
        account: &str,
    ) -> Result<(TargetLock, TargetLock)> {
        lock_both(normalized_target, ns.service(), account, LOCK_WAIT_MS)
    }

    fn read(&self, ns: &Namespace, account: &str, want_blob: bool) -> Result<Option<Secret>> {
        let name = wide(&ns.windows_target_checked(account)?);
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
            let out: Result<Secret> = if want_blob {
                let c = &*p;
                let raw = if c.CredentialBlob.is_null() || c.CredentialBlobSize == 0 {
                    Vec::new()
                } else {
                    std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize)
                        .to_vec()
                };
                let raw = zeroize::Zeroizing::new(raw);
                match ns.blob_codec() {
                    BlobCodec::KeyringUtf16 => keyring_compat::decode(&raw).map(Secret::from_bytes),
                    BlobCodec::Raw | BlobCodec::LegacyUtf8 => Ok(Secret::from_bytes(raw.to_vec())),
                }
            } else {
                Ok(Secret::from_bytes(Vec::new()))
            };
            CredFree(p as _);
            out.map(Some)
        }
    }

    fn write(&self, ns: &Namespace, account: &str, secret: &Secret) -> Result<()> {
        let name = wide(&ns.windows_target_checked(account)?);
        let user = wide(account);
        let blob = zeroize::Zeroizing::new(match ns.blob_codec() {
            BlobCodec::Raw => secret.expose_bytes().to_vec(),
            BlobCodec::KeyringUtf16 => keyring_compat::encode(secret.expose_bytes())?,
            BlobCodec::LegacyUtf8 => {
                return Err(Error::Invalid {
                    what: "windows target",
                    reason: "KeyringCompatLegacyUtf8 is a read-only migration view; write \
                             through KeyringCompat"
                        .into(),
                })
            }
        });
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

    fn remove(&self, ns: &Namespace, account: &str) -> Result<bool> {
        let name = wide(&ns.windows_target_checked(account)?);
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

impl Backend for CredentialManagerBackend {
    fn name(&self) -> &'static str {
        "windows-credential-manager"
    }
    fn exclusion_identity(&self) -> Option<String> {
        // Every instance addresses the one per-user Credential Manager.
        Some("windows-credential-manager".to_string())
    }
    fn credential_identity(&self, ns: &Namespace, account: &str) -> Result<String> {
        // The same normalised target the per-target lock is named by.
        windows_credential_identity(ns, account)
    }
    fn durable(&self) -> bool {
        true
    }
    fn get(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<Option<Secret>> {
        check_target(ns, account)?;
        Win32.read(ns, account, true)
    }
    fn exists(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<bool> {
        check_target(ns, account)?;
        Ok(Win32.read(ns, account, false)?.is_some())
    }
    fn set(&self, ns: &Namespace, account: &str, secret: &Secret, _: Interaction) -> Result<()> {
        check_target(ns, account)?;
        locked::set(&Win32, ns, account, secret)
    }
    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        _: Interaction,
    ) -> Result<bool> {
        check_target(ns, account)?;
        locked::add_if_absent(&Win32, ns, account, secret)
    }
    fn delete(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<bool> {
        check_target(ns, account)?;
        locked::delete(&Win32, ns, account)
    }
    fn delete_if_equals(
        &self,
        ns: &Namespace,
        account: &str,
        expected: &Secret,
        _: Interaction,
    ) -> Result<ConditionalDelete> {
        check_target(ns, account)?;
        locked::delete_if_equals(&Win32, ns, account, expected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::{normalize_target_name, WindowsTarget};
    use std::sync::mpsc;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique(tag: &str) -> String {
        format!(
            "rk-secrets-unit-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    #[test]
    fn the_lock_is_named_by_the_normalised_target_not_by_the_service() {
        let a = Namespace::new("svc-a")
            .unwrap()
            .windows_target(WindowsTarget::Slash {
                prefix: Some("Shared".into()),
            });
        let b = Namespace::new("svc-b")
            .unwrap()
            .windows_target(WindowsTarget::Slash {
                prefix: Some("SHARED".into()),
            });
        let name_a = lock_name(&normalize_target_name(
            &a.windows_target_checked("acct").unwrap(),
        ))
        .unwrap();
        let name_b = lock_name(&normalize_target_name(
            &b.windows_target_checked("acct").unwrap(),
        ))
        .unwrap();
        assert_eq!(name_a, name_b);
        let other = lock_name(&normalize_target_name(
            &a.windows_target_checked("other").unwrap(),
        ))
        .unwrap();
        assert_ne!(name_a, other);
    }

    #[test]
    fn the_lock_is_global_and_bound_to_the_user() {
        let name = String::from_utf16(&lock_name("svc/acct").unwrap()).unwrap();
        assert!(name.starts_with("Global\\rightkit-secrets-t-"), "{name}");
        assert!(user_sid().unwrap().starts_with("S-1-"));
        assert_eq!(
            name,
            String::from_utf16(&lock_name("svc/acct").unwrap()).unwrap()
        );
    }

    /// Comparative review, 2026-10-12: the name must be the one 0.1.3 computes, or an older
    /// process is not excluded. The expected value is SHA-256 of `service NUL account`, first
    /// 16 bytes, computed outside this crate.
    #[test]
    fn the_legacy_lock_has_the_name_older_versions_take() {
        // The name is built for `CreateMutexW`, so it ends with the terminating NUL.
        let name = legacy_lock_name("dev.rightkit.secrets.legacy", "db").unwrap();
        let (terminator, text) = name.split_last().unwrap();
        assert_eq!(*terminator, 0);
        assert_eq!(
            String::from_utf16(text).unwrap(),
            "Local\\rightkit-secrets-4a09df4ea76be3781179b6dbb18cb6b1"
        );
    }

    /// A process on 0.1.3 holds only the legacy mutex. While it does, a mutation here waits.
    #[test]
    fn a_mutation_waits_for_a_holder_of_the_legacy_lock() {
        let service = unique("legacy");
        let target = normalize_target_name(&format!("{service}/acct"));
        // Taken exactly as 0.1.3 takes it: by name, default security, nothing else held.
        let name = legacy_lock_name(&service, "acct").unwrap();
        let old = unsafe {
            let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            assert!(!handle.is_null());
            TargetLock::wait(handle, 1_000).unwrap()
        };
        let (tx, rx) = mpsc::channel();
        let (thread_target, thread_service) = (target.clone(), service.clone());
        let contender = std::thread::spawn(move || {
            tx.send(lock_both(&thread_target, &thread_service, "acct", 200).map(|_| ()))
                .unwrap();
        });
        let outcome = rx.recv().unwrap();
        contender.join().unwrap();
        assert!(
            matches!(outcome, Err(Error::Timeout { op: "lock", .. })),
            "{outcome:?}"
        );
        drop(old);
        // Free again once released, and the refused attempt left the target lock free too.
        std::thread::spawn(move || lock_both(&target, &service, "acct", 1_000).map(|_| ()))
            .join()
            .unwrap()
            .unwrap();
    }

    /// Audit finding: `set` and `delete` took no lock, so they ran beside `add_if_absent`.
    #[test]
    fn a_second_thread_cannot_take_a_target_while_another_holds_it() {
        let target = normalize_target_name(&format!("{}/acct", unique("held")));
        let held = TargetLock::acquire(&target, 1_000).unwrap();
        let (tx, rx) = mpsc::channel();
        let name = target.clone();
        let contender = std::thread::spawn(move || {
            tx.send(TargetLock::acquire(&name, 200).map(|_| ()))
                .unwrap();
        });
        let outcome = rx.recv().unwrap();
        contender.join().unwrap();
        assert!(
            matches!(outcome, Err(Error::Timeout { op: "lock", .. })),
            "{outcome:?}"
        );
        drop(held);
        // Free again once released.
        let name = target.clone();
        std::thread::spawn(move || TargetLock::acquire(&name, 1_000).map(|_| ()))
            .join()
            .unwrap()
            .unwrap();
    }
}
