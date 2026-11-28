//! macOS: read an item's access control and create an item that only this process can read
//! (rightkit-credentials 0.5.11, for the `rightkit-qa` snapshot store, GATE-CONTRACT #1552).
//!
//! The ordinary write path (`SecKeychainAddGenericPassword`) takes no access object, so a new item
//! gets the default access of the creating process and an existing item is updated in place with
//! whatever access it already has. This module adds the two things that were missing, for the
//! legacy file keychain this crate already uses:
//!
//! * a READ-ONLY proof ([`item_proof`]): the item's access object is copied
//!   (`SecKeychainItemCopyAccess`), its ACL entries are read (`SecAccessCopyACLList`,
//!   `SecACLCopyAuthorizations`, `SecACLCopyContents`) and the item is admitted only when every
//!   entry that authorizes reading the secret lists exactly one trusted application and that
//!   application is satisfied by this process's executable. Nothing is changed and no secret is
//!   read.
//! * an ISOLATED write ([`set_isolated`]): an absent item is created with an explicit access object
//!   that trusts only the current process (`SecTrustedApplicationCreateFromPath(NULL)`,
//!   `SecAccessCreate`, `SecKeychainItemCreateFromContent`) and is proved again after creation; a
//!   present item is updated in place only when it is already proven; any other present item is
//!   refused and left untouched. The write is made on the same pinned user-default keychain as
//!   every other mutation, after the same destination checks.
//!
//! Fail closed: an access object that cannot be read, an entry that cannot be decoded, an
//! unrecognised authorization on a readable entry, an "any application" entry, more or fewer than
//! one trusted application, a trusted application this process does not satisfy, a partition list
//! that names anything but this team: none is admitted.
//!
//! Written from the documented Security.framework calls; not compiled or run on a Mac yet.

use super::OSStatus;
use super::{map, Handle, KeychainBackend, Native, Os, DUPLICATE, NOT_FOUND, STATUS_UNLOCKED};
use crate::backend::Interaction;
use crate::error::{Error, Result};
use crate::namespace::Namespace;
use crate::secret::Secret;
use std::ffi::{c_char, c_void, CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::ptr::{null, null_mut};

type CFIndex = isize;

/// `kCFStringEncodingUTF8`.
const UTF8: u32 = 0x0800_0100;
/// `kSecGenericPasswordItemClass` ('genp').
const GENERIC_PASSWORD: u32 = 0x6765_6E70;
/// `kSecServiceItemAttr` ('svce') and `kSecAccountItemAttr` ('acct').
const ATTR_SERVICE: u32 = 0x7376_6365;
const ATTR_ACCOUNT: u32 = 0x6163_6374;
/// `kSecKeychainPromptRequirePassphase`: the entry asks for the keychain password.
const PROMPT_REQUIRE_PASSPHRASE: u16 = 0x0001;

const TAG_ANY: &str = "ACLAuthorizationAny";
const TAG_DECRYPT: &str = "ACLAuthorizationDecrypt";
const TAG_PARTITION: &str = "ACLAuthorizationPartitionID";
const TAG_CHANGE_ACL: &str = "ACLAuthorizationChangeACL";
const TAG_CHANGE_OWNER: &str = "ACLAuthorizationChangeOwner";
const TAG_DELETE: &str = "ACLAuthorizationDelete";

/// What the proof found. Anything but [`AccessProof::SelfOnly`] means "do not rely on this item".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessProof {
    /// No item with that service and account exists in the keychain.
    Absent,
    /// Every entry that authorizes reading the secret trusts only this process (and the partition
    /// list, when the item has one, names only this team).
    SelfOnly,
    /// The item exists and its access was read, and it is not self-only. The text is a fixed
    /// reason that names no value.
    Foreign(&'static str),
}

impl AccessProof {
    /// True only for [`AccessProof::SelfOnly`].
    pub fn is_self_only(&self) -> bool {
        matches!(self, AccessProof::SelfOnly)
    }
}

#[repr(C)]
struct Attr {
    tag: u32,
    length: u32,
    data: *mut c_void,
}

#[repr(C)]
struct AttrList {
    count: u32,
    attr: *mut Attr,
}

#[link(name = "Security", kind = "framework")]
extern "C" {
    fn SecKeychainItemCopyAccess(item: *mut c_void, access: *mut *mut c_void) -> OSStatus;
    fn SecAccessCopyACLList(access: *mut c_void, list: *mut *const c_void) -> OSStatus;
    fn SecACLCopyAuthorizations(acl: *mut c_void) -> *const c_void;
    fn SecACLCopyContents(
        acl: *mut c_void,
        applications: *mut *const c_void,
        description: *mut *const c_void,
        prompt_selector: *mut u16,
    ) -> OSStatus;
    fn SecTrustedApplicationCreateFromPath(path: *const c_char, app: *mut *mut c_void) -> OSStatus;
    fn SecTrustedApplicationValidateWithPath(app: *mut c_void, path: *const c_char) -> OSStatus;
    fn SecAccessCreate(
        descriptor: *const c_void,
        trusted_list: *const c_void,
        access: *mut *mut c_void,
    ) -> OSStatus;
    fn SecKeychainItemCreateFromContent(
        class: u32,
        attr_list: *const AttrList,
        length: u32,
        data: *const c_void,
        keychain: *mut c_void,
        initial_access: *mut c_void,
        item: *mut *mut c_void,
    ) -> OSStatus;
    fn SecKeychainItemModifyAttributesAndData(
        item: *mut c_void,
        attrs: *const c_void,
        len: u32,
        data: *const c_void,
    ) -> OSStatus;
    fn SecKeychainItemDelete(item: *mut c_void) -> OSStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    #[link_name = "kCFTypeArrayCallBacks"]
    static K_CF_TYPE_ARRAY_CALL_BACKS: u8;
    fn CFRelease(cf: *const c_void);
    fn CFArrayCreate(
        allocator: *const c_void,
        values: *const *const c_void,
        count: CFIndex,
        callbacks: *const c_void,
    ) -> *const c_void;
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

/// Releases a Core Foundation object (or a Security reference) on drop.
struct Cf(*const c_void);

impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from a Create or Copy call and is released exactly once.
            unsafe { CFRelease(self.0) };
        }
    }
}

fn unreadable(what: &str) -> Error {
    Error::Unavailable(format!(
        "the keychain item's access cannot be read ({what})"
    ))
}

/// A CFString as text; `None` when it is null or not representable as UTF-8.
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
            CStr::from_ptr(buffer.as_ptr())
                .to_string_lossy()
                .into_owned(),
        )
    }
}

/// The running executable as a C path (the file the OS launched this process from).
fn self_path() -> Result<CString> {
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|_| Error::Unavailable("the running executable cannot be identified".into()))?;
    CString::new(exe.as_os_str().as_bytes())
        .map_err(|_| Error::Unavailable("the running executable's path is not valid".into()))
}

/// A team identifier is ten upper-case letters or digits (the shape `rightkit-qa` validates).
fn valid_team(team_id: &str) -> bool {
    team_id.len() == 10
        && team_id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// The partition-list description is a property list, usually hex-encoded; read either form as bytes.
fn description_bytes(text: &str) -> Vec<u8> {
    let hex = text.trim().as_bytes();
    if !hex.is_empty() && hex.len() % 2 == 0 && hex.iter().all(u8::is_ascii_hexdigit) {
        let nibble = |b: u8| -> u8 {
            match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                _ => b - b'A' + 10,
            }
        };
        return hex
            .chunks_exact(2)
            .map(|pair| match pair {
                [high, low] => (nibble(*high) << 4) | nibble(*low),
                _ => 0,
            })
            .collect();
    }
    text.as_bytes().to_vec()
}

/// True only when `bytes` carries at least one `teamid:<team_id>` token and no other partition
/// token (`teamid:` of another team, `cdhash:`, `apple-tool:`, `apple:`). A token is found at every
/// byte offset, so the length byte a binary property list puts before a string cannot hide it.
fn partition_is_only(bytes: &[u8], team_id: &str) -> bool {
    const OTHER: [&[u8]; 3] = [b"cdhash:", b"apple-tool:", b"apple:"];
    const TEAM: &[u8] = b"teamid:";
    let team = team_id.as_bytes();
    let mut own = 0usize;
    for at in 0..bytes.len() {
        let Some(rest) = bytes.get(at..) else {
            return false;
        };
        if OTHER.iter().any(|p| rest.starts_with(p)) {
            return false;
        }
        if let Some(after) = rest.strip_prefix(TEAM) {
            if after.get(..team.len()) == Some(team) {
                own += 1;
            } else {
                return false;
            }
        }
    }
    own > 0
}

/// Whether the trusted application list `apps` (a CFArray of SecTrustedApplicationRef) holds exactly
/// one application and this process's executable satisfies it. False for a null list ("any
/// application"), an empty one, or more than one entry.
fn exactly_self(apps: *const c_void, exe: &CStr) -> bool {
    if apps.is_null() {
        return false;
    }
    // SAFETY: `apps` is a live CFArray of SecTrustedApplicationRef owned by the caller for the call;
    // the index is within the count; `exe` is a NUL-terminated path.
    unsafe {
        if CFArrayGetCount(apps) != 1 {
            return false;
        }
        let app = CFArrayGetValueAtIndex(apps, 0) as *mut c_void;
        !app.is_null() && SecTrustedApplicationValidateWithPath(app, exe.as_ptr()) == 0
    }
}

/// Every listed application (possibly none) is satisfied by this process.
fn all_self(apps: *const c_void, exe: &CStr) -> bool {
    if apps.is_null() {
        return false;
    }
    // SAFETY: as in `exactly_self`.
    unsafe {
        (0..CFArrayGetCount(apps)).all(|index| {
            let app = CFArrayGetValueAtIndex(apps, index) as *mut c_void;
            !app.is_null() && SecTrustedApplicationValidateWithPath(app, exe.as_ptr()) == 0
        })
    }
}

/// The proof for one item reference. Reads only.
fn item_proof(item: *mut c_void, team_id: &str, exe: &CStr) -> Result<AccessProof> {
    if !valid_team(team_id) {
        return Err(Error::Invalid {
            what: "team id",
            reason: "must be ten upper-case letters or digits".into(),
        });
    }
    // SAFETY: every pointer passed is valid for the call; each object returned by a Copy or Create
    // call is owned by a `Cf` that releases it; arrays are read only within their count.
    unsafe {
        let mut access: *mut c_void = null_mut();
        let status = SecKeychainItemCopyAccess(item, &mut access);
        if status != 0 || access.is_null() {
            return Err(map("item_access", status));
        }
        let access = Cf(access as *const c_void);
        let mut list: *const c_void = null();
        let status = SecAccessCopyACLList(access.0 as *mut c_void, &mut list);
        if status != 0 || list.is_null() {
            return Err(map("item_acl_list", status));
        }
        let list = Cf(list);

        let mut decrypt_covered = false;
        for index in 0..CFArrayGetCount(list.0) {
            let acl = CFArrayGetValueAtIndex(list.0, index) as *mut c_void;
            if acl.is_null() {
                return Err(unreadable("entry"));
            }
            let authorizations = Cf(SecACLCopyAuthorizations(acl));
            if authorizations.0.is_null() {
                return Err(unreadable("authorizations"));
            }
            let mut tags: Vec<String> = Vec::new();
            for tag_index in 0..CFArrayGetCount(authorizations.0) {
                match cf_string(CFArrayGetValueAtIndex(authorizations.0, tag_index)) {
                    Some(tag) => tags.push(tag),
                    None => return Err(unreadable("authorization tag")),
                }
            }
            let mut applications: *const c_void = null();
            let mut description: *const c_void = null();
            let mut selector: u16 = 0;
            let status =
                SecACLCopyContents(acl, &mut applications, &mut description, &mut selector);
            let applications = Cf(applications);
            let description = Cf(description);
            if status != 0 {
                return Err(map("item_acl_contents", status));
            }

            if tags.iter().any(|t| t == TAG_PARTITION) {
                let Some(text) = cf_string(description.0) else {
                    return Err(unreadable("partition list"));
                };
                if !partition_is_only(&description_bytes(&text), team_id) {
                    return Ok(AccessProof::Foreign(
                        "the partition list names more than this team",
                    ));
                }
            }

            // Entries that authorize anything but these four can reveal the secret (or are not
            // recognised, which is the same): they must trust exactly this process.
            let reads = tags.iter().any(|t| {
                !matches!(
                    t.as_str(),
                    TAG_PARTITION | TAG_CHANGE_ACL | TAG_CHANGE_OWNER | TAG_DELETE
                )
            });
            if reads {
                if !exactly_self(applications.0, exe) {
                    return Ok(AccessProof::Foreign(
                        "an entry that can read the secret trusts more or other than this app",
                    ));
                }
                if tags.iter().any(|t| t == TAG_ANY || t == TAG_DECRYPT) {
                    decrypt_covered = true;
                }
            } else if tags
                .iter()
                .any(|t| t == TAG_CHANGE_ACL || t == TAG_CHANGE_OWNER)
            {
                // Whoever may change the access can grant themselves the read: no other
                // application, and "any application" only behind the keychain password.
                let trusted = if applications.0.is_null() {
                    selector & PROMPT_REQUIRE_PASSPHRASE != 0
                } else {
                    all_self(applications.0, exe)
                };
                if !trusted {
                    return Ok(AccessProof::Foreign(
                        "another application may change the item's access",
                    ));
                }
            }
        }
        // No partition entry at all: only the trusted-application check applies.
        if !decrypt_covered {
            return Ok(AccessProof::Foreign(
                "no entry authorizes this app to read the secret",
            ));
        }
        Ok(AccessProof::SelfOnly)
    }
}

/// Finds the item in `kc` only; `Ok(None)` when it does not exist.
fn find(kc: &Handle, ns: &Namespace, account: &str) -> Result<Option<Handle>> {
    match KeychainBackend::find(kc, ns, account, false, true) {
        (0, _, item) if !item.is_null() => Ok(Some(Handle(item))),
        (0, _, _) => Err(map("find", 0)),
        (NOT_FOUND, _, item) => {
            drop(Handle(item));
            Ok(None)
        }
        (status, _, item) => {
            drop(Handle(item));
            Err(map("find", status))
        }
    }
}

/// The read-only proof for `account` in `kc`.
pub(super) fn proof(
    kc: &Handle,
    ns: &Namespace,
    account: &str,
    team_id: &str,
) -> Result<AccessProof> {
    let exe = self_path()?;
    match find(kc, ns, account)? {
        None => Ok(AccessProof::Absent),
        Some(item) => item_proof(item.0, team_id, &exe),
    }
}

/// The same destination checks `add_checked` makes before any add: the keychain reference is valid,
/// its file exists, and it is unlocked unless the call may prompt. An explicit keychain is refused.
fn check_destination(backend: &KeychainBackend, kc: &Handle, mode: Interaction) -> Result<()> {
    if backend.target.write_is_refused() {
        return Err(Error::Unavailable(
            "refusing to write: a write to an explicit keychain cannot be guaranteed to stay in it"
                .into(),
        ));
    }
    let bits = Os.status(kc).map_err(|st| {
        Error::Unavailable(format!("destination keychain is not valid (code {st})"))
    })?;
    Os.path(kc)
        .filter(|p| p.exists())
        .ok_or_else(|| Error::Unavailable("destination keychain file is not available".into()))?;
    if bits & STATUS_UNLOCKED == 0 && mode == Interaction::NonInteractive {
        return Err(Error::InteractionRequired);
    }
    Ok(())
}

/// A SecAccess that trusts only this process: the trusted-application list is exactly one entry,
/// `SecTrustedApplicationCreateFromPath(NULL)` (the calling application).
fn self_only_access() -> Result<Cf> {
    // SAFETY: every pointer passed is valid for the call; each created object is owned by a `Cf`;
    // the array copies (retains) the application reference, which `app` still owns and releases.
    unsafe {
        let mut app: *mut c_void = null_mut();
        let status = SecTrustedApplicationCreateFromPath(null(), &mut app);
        if status != 0 || app.is_null() {
            return Err(map("trusted_app", status));
        }
        let app = Cf(app as *const c_void);
        let values = [app.0];
        let trusted = Cf(CFArrayCreate(
            null(),
            values.as_ptr(),
            1,
            std::ptr::addr_of!(K_CF_TYPE_ARRAY_CALL_BACKS).cast::<c_void>(),
        ));
        if trusted.0.is_null() {
            return Err(Error::Unavailable("the trusted list cannot be made".into()));
        }
        let descriptor = Cf(CFStringCreateWithCString(
            null(),
            c"RightKit QA snapshot".as_ptr(),
            UTF8,
        ));
        if descriptor.0.is_null() {
            return Err(Error::Unavailable("the access label cannot be made".into()));
        }
        let mut access: *mut c_void = null_mut();
        let status = SecAccessCreate(descriptor.0, trusted.0, &mut access);
        if status != 0 || access.is_null() {
            return Err(map("access_create", status));
        }
        Ok(Cf(access as *const c_void))
    }
}

/// Creates the item with an explicit self-only access in `kc` (never the search list), holding
/// `data`. The isolated write passes a placeholder here, never the secret: the secret is written
/// only after the new item's access is proven.
fn create_with_access(
    kc: &Handle,
    access: &Cf,
    ns: &Namespace,
    account: &str,
    data: &[u8],
) -> Result<()> {
    let (service, account) = ns.keychain_item(account);
    let (Ok(service_len), Ok(account_len), Ok(data_len)) = (
        u32::try_from(service.len()),
        u32::try_from(account.len()),
        u32::try_from(data.len()),
    ) else {
        return Err(Error::Invalid {
            what: "item",
            reason: "too long".into(),
        });
    };
    let mut attrs = [
        Attr {
            tag: ATTR_SERVICE,
            length: service_len,
            data: service.as_ptr() as *mut c_void,
        },
        Attr {
            tag: ATTR_ACCOUNT,
            length: account_len,
            data: account.as_ptr() as *mut c_void,
        },
    ];
    let list = AttrList {
        count: 2,
        attr: attrs.as_mut_ptr(),
    };
    // SAFETY: `list` and the attribute strings outlive the call and are only read by it (the
    // mutable pointers are the C signature's, not written through); `data` is a live buffer of
    // `data_len` bytes; `kc` and `access` are live references; no item reference is requested.
    let status = unsafe {
        SecKeychainItemCreateFromContent(
            GENERIC_PASSWORD,
            &list,
            data_len,
            data.as_ptr().cast(),
            kc.0,
            access.0 as *mut c_void,
            null_mut(),
        )
    };
    match status {
        0 => Ok(()),
        DUPLICATE => Err(Error::Unavailable(
            "the slot appeared while it was being created; refusing to replace it".into(),
        )),
        s => Err(map("set_isolated", s)),
    }
}

/// What a new slot holds between its creation and the proof of its access: not a secret.
const PLACEHOLDER: &[u8] = &[0];

/// Replaces the data of `item`, whose access the caller has just proven self-only. The access
/// object is not touched (`NULL` attributes, data only).
fn write_data(item: &Handle, bytes: &[u8]) -> Result<()> {
    let len = u32::try_from(bytes.len()).map_err(|_| Error::Invalid {
        what: "secret",
        reason: "too long".into(),
    })?;
    // SAFETY: `item` is a live item reference; `bytes` is a live buffer of `len` bytes, only read.
    let status = unsafe {
        SecKeychainItemModifyAttributesAndData(item.0, null(), len, bytes.as_ptr().cast())
    };
    if status == 0 {
        Ok(())
    } else {
        Err(map("set_isolated", status))
    }
}

fn refuse_foreign(reason: &'static str) -> Error {
    Error::Unavailable(format!(
        "refusing to write a slot whose access control is not proven self-only ({reason})"
    ))
}

/// The isolated write. Absent: create with a self-only access, then prove it (a failed proof
/// deletes the item just made and refuses). Present and proven self-only: update in place (its
/// access stays what was proven; nothing is deleted, so a failure cannot destroy the stored value).
/// Present and not proven: refuse, change and delete nothing.
pub(super) fn set_isolated(
    backend: &KeychainBackend,
    kc: &Handle,
    ns: &Namespace,
    account: &str,
    secret: &Secret,
    team_id: &str,
    mode: Interaction,
) -> Result<()> {
    if secret.is_empty() {
        return Err(Error::Invalid {
            what: "secret",
            reason: "empty (use delete)".into(),
        });
    }
    if !valid_team(team_id) {
        return Err(Error::Invalid {
            what: "team id",
            reason: "must be ten upper-case letters or digits".into(),
        });
    }
    check_destination(backend, kc, mode)?;
    let exe = self_path()?;
    if let Some(existing) = find(kc, ns, account)? {
        return match item_proof(existing.0, team_id, &exe)? {
            AccessProof::SelfOnly => write_data(&existing, secret.expose_bytes()),
            AccessProof::Foreign(reason) => Err(refuse_foreign(reason)),
            AccessProof::Absent => Err(refuse_foreign("the item vanished")),
        };
    }
    let access = self_only_access()?;
    // A one-byte placeholder, never the secret: the secret reaches the keychain only after the
    // new item's access is read back and proven, so a failed proof leaves no secret behind.
    create_with_access(kc, &access, ns, account, PLACEHOLDER)?;
    // Read the item back: it must be the one just made, with the access just given.
    let verdict = match find(kc, ns, account)? {
        Some(item) => match item_proof(item.0, team_id, &exe) {
            Ok(AccessProof::SelfOnly) => return write_data(&item, secret.expose_bytes()),
            Ok(AccessProof::Foreign(reason)) => (Some(item), refuse_foreign(reason)),
            Ok(AccessProof::Absent) => (Some(item), refuse_foreign("the item vanished")),
            Err(err) => (Some(item), err),
        },
        None => (None, refuse_foreign("the item vanished")),
    };
    let (item, err) = verdict;
    if let Some(item) = item {
        // SAFETY: `item` is a live item reference; the failed read-back leaves nothing to keep.
        let status = unsafe { SecKeychainItemDelete(item.0) };
        if status != 0 {
            return Err(Error::Unavailable(format!(
                "the new slot is not proven self-only and could not be removed (code {status})"
            )));
        }
    }
    Err(err)
}
