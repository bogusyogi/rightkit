//! macOS: the keychain partition list of the protected item (see `protected_guard.rs`).
//!
//! Security gives every item a partition list when it is created, from the code signature of the
//! creating process: `teamid:<team>` for Developer ID code, `cdhash:<hash>` for ad-hoc code,
//! `apple-tool:` for the `security` command. Another process cannot change the list without the
//! keychain password, so an item that carries exactly the host app's team was created by app-signed
//! code. Both the read and the check are pinned to the user's login keychain by path: an item in
//! any other keychain (for example one an attacker made the default) is never read or trusted, and
//! the engine fails closed unless the default keychain is the login keychain.
//! This is defence in depth, not the boundary: a process of the same user can make a keychain of its
//! own with a matching partition list and move it onto the login keychain's path, so the stored blob
//! is also sealed (`seal.rs`).
//! Written from the documented Security.framework calls; not compiled or run on a Mac yet.

use std::ffi::{CString, OsStr, c_char, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::Mutex;

type OSStatus = i32;
type CFIndex = isize;

/// `kSecPreferencesDomainUser`; the same domain `rightkit-credentials` writes to.
const DOMAIN_USER: i32 = 0;
const UTF8: u32 = 0x0800_0100;
/// `errSecItemNotFound`.
const NOT_FOUND: OSStatus = -25300;
/// `SecKeychainSetUserInteractionAllowed` is process-wide; one read at a time changes and restores it.
static INTERACTION: Mutex<()> = Mutex::new(());

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecKeychainCopyDomainDefault(domain: i32, out: *mut *mut c_void) -> OSStatus;
    fn SecKeychainOpen(path: *const c_char, out: *mut *mut c_void) -> OSStatus;
    fn SecKeychainGetPath(kc: *const c_void, len: *mut u32, path: *mut c_char) -> OSStatus;
    fn SecKeychainItemCopyKeychain(item: *mut c_void, kc: *mut *mut c_void) -> OSStatus;
    fn SecKeychainItemFreeContent(attrs: *const c_void, data: *mut c_void) -> OSStatus;
    fn SecKeychainSetUserInteractionAllowed(allowed: u8) -> OSStatus;
    fn SecKeychainGetUserInteractionAllowed(allowed: *mut u8) -> OSStatus;
    fn SecKeychainFindGenericPassword(
        kc: *const c_void,
        service_len: u32,
        service: *const c_char,
        account_len: u32,
        account: *const c_char,
        password_len: *mut u32,
        password: *mut *mut c_void,
        item: *mut *mut c_void,
    ) -> OSStatus;
    fn SecKeychainItemCopyAccess(item: *mut c_void, access: *mut *mut c_void) -> OSStatus;
    fn SecAccessCopyMatchingACLList(access: *mut c_void, tag: *const c_void) -> *const c_void;
    fn SecACLCopyContents(
        acl: *mut c_void,
        applications: *mut *const c_void,
        description: *mut *const c_void,
        prompt_selector: *mut u16,
    ) -> OSStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
    fn CFArrayGetCount(array: *const c_void) -> CFIndex;
    fn CFArrayGetValueAtIndex(array: *const c_void, index: CFIndex) -> *const c_void;
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        text: *const c_char,
        encoding: u32,
    ) -> *const c_void;
    fn CFStringGetLength(text: *const c_void) -> CFIndex;
    fn CFStringGetMaximumSizeForEncoding(length: CFIndex, encoding: u32) -> CFIndex;
    fn CFStringGetCString(
        text: *const c_void,
        buffer: *mut c_char,
        size: CFIndex,
        encoding: u32,
    ) -> u8;
}

/// Releases a Core Foundation object on drop.
struct Cf(*const c_void);

impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from a Create or Copy call and is released exactly once.
            unsafe { CFRelease(self.0) };
        }
    }
}

fn cf_string(text: *const c_void) -> Option<String> {
    if text.is_null() {
        return None;
    }
    // SAFETY: `text` is a live CFString owned by the caller; the buffer is sized by
    // CFStringGetMaximumSizeForEncoding plus the terminator and CFStringGetCString NUL-terminates.
    unsafe {
        let size = CFStringGetMaximumSizeForEncoding(CFStringGetLength(text), UTF8) + 1;
        let mut buffer = vec![0 as c_char; usize::try_from(size).ok()?];
        if CFStringGetCString(text, buffer.as_mut_ptr(), size, UTF8) == 0 {
            return None;
        }
        Some(
            std::ffi::CStr::from_ptr(buffer.as_ptr())
                .to_string_lossy()
                .into_owned(),
        )
    }
}

/// The login keychain file of the current user, from the account database (not `$HOME`, which any
/// parent process can set).
fn login_path() -> Result<PathBuf, String> {
    // SAFETY: `getpwuid_r` fills `pwd` with strings that live in `buffer`, which outlives the copy.
    unsafe {
        let mut buffer = vec![0 as c_char; 4096];
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut found: *mut libc::passwd = null_mut();
        let rc = libc::getpwuid_r(
            libc::geteuid(),
            &mut pwd,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        );
        if rc != 0 || found.is_null() || pwd.pw_dir.is_null() {
            return Err("the home folder is unknown".into());
        }
        let home = std::ffi::CStr::from_ptr(pwd.pw_dir).to_bytes();
        let keychains = PathBuf::from(OsStr::from_bytes(home)).join("Library/Keychains");
        // Older accounts keep the legacy name; the current one is `login.keychain-db`.
        let current = keychains.join("login.keychain-db");
        let legacy = keychains.join("login.keychain");
        Ok(if !current.exists() && legacy.exists() {
            legacy
        } else {
            current
        })
    }
}

/// The file a keychain reference points at, resolved.
fn keychain_file(keychain: *const c_void) -> Result<PathBuf, String> {
    let mut buffer = vec![0 as c_char; 1025];
    let mut len: u32 = 1024;
    // SAFETY: the buffer holds `len` bytes; `len` is set to the length written.
    let status = unsafe { SecKeychainGetPath(keychain, &mut len, buffer.as_mut_ptr()) };
    if status != 0 {
        return Err(format!("a keychain's file cannot be read ({status})"));
    }
    let bytes: Vec<u8> = buffer[..len as usize].iter().map(|&c| c as u8).collect();
    std::fs::canonicalize(OsStr::from_bytes(&bytes))
        .map_err(|_| "a keychain's file cannot be resolved".to_string())
}

/// The user's login keychain, opened by its path. Fails closed unless the user's default keychain
/// is that same file, because the engine writes its item through the default (`rightkit-credentials`)
/// and an attacker's keychain made the default must not be where the host app writes, or reads.
fn login_keychain() -> Result<(Cf, PathBuf), String> {
    let login = std::fs::canonicalize(login_path()?)
        .map_err(|_| "the login keychain file is missing".to_string())?;
    let path = CString::new(login.as_os_str().as_bytes()).map_err(|_| "the keychain path")?;
    // SAFETY: the path is a NUL-terminated string; the references returned are owned by `Cf`.
    unsafe {
        let mut opened: *mut c_void = null_mut();
        let status = SecKeychainOpen(path.as_ptr(), &mut opened);
        if status != 0 || opened.is_null() {
            return Err(format!("the login keychain cannot be opened ({status})"));
        }
        let opened = Cf(opened);
        let mut default: *mut c_void = null_mut();
        let status = SecKeychainCopyDomainDefault(DOMAIN_USER, &mut default);
        if status != 0 || default.is_null() {
            return Err(format!("the default keychain cannot be opened ({status})"));
        }
        let default = Cf(default);
        if keychain_file(opened.0)? != login || keychain_file(default.0)? != login {
            return Err("the default keychain is not the login keychain".into());
        }
        Ok((opened, login))
    }
}

/// Finds the item in the login keychain only (a non-null keychain restricts the search to it), and
/// refuses one whose own keychain is not the login file. `None`: no such item.
fn find_item(
    keychain: &Cf,
    login: &Path,
    service: &str,
    account: &str,
    want_data: bool,
) -> Result<Option<(Cf, Vec<u8>)>, String> {
    let service = CString::new(service).map_err(|_| "the item name is not valid")?;
    let account = CString::new(account).map_err(|_| "the item name is not valid")?;
    // SAFETY: every pointer is valid for the call; the item is owned by a `Cf`; the password bytes
    // are copied and then freed with SecKeychainItemFreeContent.
    unsafe {
        let mut item: *mut c_void = null_mut();
        let mut length: u32 = 0;
        let mut data: *mut c_void = null_mut();
        let status = SecKeychainFindGenericPassword(
            keychain.0,
            service.as_bytes().len() as u32,
            service.as_ptr(),
            account.as_bytes().len() as u32,
            account.as_ptr(),
            if want_data { &mut length } else { null_mut() },
            if want_data { &mut data } else { null_mut() },
            &mut item,
        );
        if status == NOT_FOUND {
            return Ok(None);
        }
        let bytes = if want_data && !data.is_null() {
            let copy = std::slice::from_raw_parts(data as *const u8, length as usize).to_vec();
            SecKeychainItemFreeContent(null(), data);
            copy
        } else {
            Vec::new()
        };
        if status != 0 || item.is_null() {
            return Err(format!("the keychain item cannot be read ({status})"));
        }
        let item = Cf(item);
        let mut home: *mut c_void = null_mut();
        let status = SecKeychainItemCopyKeychain(item.0 as *mut c_void, &mut home);
        if status != 0 || home.is_null() {
            return Err(format!(
                "the keychain item's keychain is unknown ({status})"
            ));
        }
        let home = Cf(home);
        if keychain_file(home.0)?.as_path() != login {
            return Err("the keychain item is not in the login keychain".into());
        }
        Ok(Some((item, bytes)))
    }
}

/// The partition descriptions of the item's partition-ID ACL entries, as text.
fn partition_descriptions(item: &Cf) -> Result<Vec<String>, String> {
    let tag = CString::new("ACLAuthorizationPartitionID").map_err(|_| "tag")?;
    // SAFETY: every pointer passed is valid for the call; each object returned by a Copy or Create
    // call is owned by a `Cf` that releases it; the arrays are read only within their length.
    unsafe {
        let mut access: *mut c_void = null_mut();
        let status = SecKeychainItemCopyAccess(item.0 as *mut c_void, &mut access);
        if status != 0 || access.is_null() {
            return Err(format!(
                "the keychain item's access cannot be read ({status})"
            ));
        }
        let access = Cf(access);
        let tag = Cf(CFStringCreateWithCString(null(), tag.as_ptr(), UTF8));
        if tag.0.is_null() {
            return Err("the partition tag cannot be made".into());
        }
        let list = Cf(SecAccessCopyMatchingACLList(access.0 as *mut c_void, tag.0));
        if list.0.is_null() {
            return Err("the keychain item has no partition list".into());
        }
        let mut out = Vec::new();
        for index in 0..CFArrayGetCount(list.0) {
            let acl = CFArrayGetValueAtIndex(list.0, index) as *mut c_void;
            let mut applications: *const c_void = null();
            let mut description: *const c_void = null();
            let mut selector: u16 = 0;
            let status =
                SecACLCopyContents(acl, &mut applications, &mut description, &mut selector);
            let _applications = Cf(applications);
            let description = Cf(description);
            if status != 0 {
                return Err(format!("a partition entry cannot be read ({status})"));
            }
            if let Some(text) = cf_string(description.0) {
                out.push(text);
            }
        }
        Ok(out)
    }
}

/// The description is the partition list as a property list, usually hex-encoded; read either form
/// as bytes (a binary list keeps its strings as plain ASCII).
fn description_bytes(text: &str) -> Vec<u8> {
    let hex = text.trim();
    if !hex.is_empty()
        && let Some(bytes) = rightkit_fs::digest::unhex(hex)
    {
        return bytes;
    }
    text.as_bytes().to_vec()
}

/// Every partition token in `bytes`: `teamid:<team>`, `cdhash:<hex>`, `apple-tool:`, `apple:`.
fn partition_tokens(bytes: &[u8]) -> Vec<String> {
    const PREFIXES: [&str; 4] = ["teamid:", "cdhash:", "apple-tool:", "apple:"];
    let mut tokens = Vec::new();
    let mut run = String::new();
    let mut flush = |run: &mut String| {
        let mut rest = run.as_str();
        loop {
            let found = PREFIXES
                .iter()
                .filter_map(|p| rest.find(p).map(|at| (at, *p)))
                .min_by_key(|(at, _)| *at);
            let Some((at, prefix)) = found else { break };
            let after = &rest[at + prefix.len()..];
            let tail = if prefix.starts_with("team") || prefix.starts_with("cdhash") {
                after
                    .find(|c: char| !c.is_ascii_alphanumeric())
                    .unwrap_or(after.len())
            } else {
                0
            };
            tokens.push(format!("{prefix}{}", &after[..tail]));
            rest = &after[tail..];
        }
        run.clear();
    };
    for &b in bytes {
        if b.is_ascii_alphanumeric() || matches!(b, b':' | b'-' | b'_' | b'.') {
            run.push(b as char);
        } else {
            flush(&mut run);
        }
    }
    flush(&mut run);
    tokens
}

/// Every build accepts exactly the host app's team, dev installs included (security record S1 / README
/// "Where the designs met" item 5).
fn judge(tokens: &[String]) -> Result<(), String> {
    if tokens.is_empty() {
        return Err("the keychain item carries no partition list".into());
    }
    let Some(team) = crate::signing_team() else {
        return Err(
            "no code-signing team is configured, so the keychain item is not trusted".into(),
        );
    };
    let own = format!("teamid:{team}");
    if tokens.iter().all(|t| *t == own) {
        return Ok(());
    }
    Err("the keychain item was not created by the app's own signed code (its partition list differs), so it is not trusted".into())
}

/// Every partition entry of `item` judged as above.
fn verify(item: &Cf) -> Result<(), String> {
    let mut tokens = Vec::new();
    for description in partition_descriptions(item)? {
        tokens.extend(partition_tokens(&description_bytes(&description)));
    }
    judge(&tokens)
}

/// Checks the item in the login keychain (`None`: it is not there, which fails). Used after the
/// engine writes its item.
pub fn check_item(service: &str, account: &str) -> Result<(), String> {
    let (keychain, login) = login_keychain()?;
    let (item, _) = find_item(&keychain, &login, service, account, false)?
        .ok_or("the keychain item cannot be found")?;
    verify(&item)
}

/// Reads the item from the login keychain only, never from whichever keychain is the default or
/// first on the search list: the partition list is checked before the value is read and again
/// after, and an item in any other keychain is refused. `Ok(None)`: no item in the login keychain.
pub fn read_item(service: &str, account: &str) -> Result<Option<String>, String> {
    let (keychain, login) = login_keychain()?;
    let Some((before, _)) = find_item(&keychain, &login, service, account, false)? else {
        return Ok(None);
    };
    verify(&before)?;
    // Reading never prompts: a locked keychain is a fail closed, as in `rightkit-credentials`.
    let _interaction = INTERACTION.lock().unwrap_or_else(|e| e.into_inner());
    let mut allowed: u8 = 1;
    // SAFETY: plain out-parameter calls.
    unsafe {
        SecKeychainGetUserInteractionAllowed(&mut allowed);
        SecKeychainSetUserInteractionAllowed(0);
    }
    let read = find_item(&keychain, &login, service, account, true);
    // SAFETY: restores the setting read above.
    unsafe { SecKeychainSetUserInteractionAllowed(allowed) };
    let (after, bytes) = read?.ok_or("the keychain item vanished while it was read")?;
    verify(&after)?;
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "the stored settings are damaged".into())
}
