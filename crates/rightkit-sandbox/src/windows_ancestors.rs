//! Physical-ancestor leases. Included only by the Windows AppContainer backend.
//!
//! Two kinds of named kernel object say "in use", and they are named differently on purpose.
//!
//! * The ancestor lease mutex is `Global\rightkit-ancestor-lease-<hash of path and AppContainer
//!   SID>`, with no user in the name. The AppContainer SID follows from the grant set alone, so two
//!   users with the same grants share one SID and one entry on a shared folder: the lease has to be
//!   visible to both. Every authenticated user may open it; one that cannot be opened for lack of
//!   access still counts as live.
//! * The profile lock and in-use mark are `Global\rightkit-profile-<kind>-<user SID>-<profile>`.
//!   A profile registration belongs to one user, so its mark does too.
//!
//! Both are in `Global` so that another logon session (fast user switching, Remote Desktop) sees
//! them. The ancestor lease is also created under its old session-local name,
//! `Local\rightkit-ancestor-lease-<hash>`, and the sweep counts either name as live, so a process
//! on rightkit-sandbox 0.3.3 or earlier and a process on this version see each other inside one
//! session.
use super::*;
use std::os::windows::ffi::OsStringExt;

pub(super) const PROFILE_PREFIX: &str = "rightkit.sandbox.";
const ANCESTOR_ACCESS: u32 = 0x0010_0081; // SYNCHRONIZE | READ_ATTRIBUTES | LIST_DIRECTORY
const ACCESS_DENIED_ACE_TYPE: u8 = 1;
const INHERITED_ACE: u8 = 0x10;

#[link(name = "kernel32")]
unsafe extern "system" {
    pub(super) fn GetFinalPathNameByHandleW(
        handle: *mut c_void,
        path: *mut u16,
        len: u32,
        flags: u32,
    ) -> u32;
    fn CreateMutexW(attributes: *const c_void, initial_owner: i32, name: *const u16)
        -> *mut c_void;
    fn CreateMutexExW(
        attributes: *const c_void,
        name: *const u16,
        flags: u32,
        access: u32,
    ) -> *mut c_void;
    fn OpenMutexW(access: u32, inherit: i32, name: *const u16) -> *mut c_void;
    fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
    fn ReleaseMutex(handle: *mut c_void) -> i32;
}

#[link(name = "bcrypt")]
unsafe extern "system" {
    fn BCryptHash(
        algorithm: *mut c_void,
        secret: *const u8,
        secret_len: u32,
        input: *const u8,
        input_len: u32,
        output: *mut u8,
        output_len: u32,
    ) -> i32;
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn RtlNtStatusToDosError(status: i32) -> u32;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn InitializeAcl(acl: *mut c_void, length: u32, revision: u32) -> i32;
    fn AddAce(
        acl: *mut c_void,
        revision: u32,
        index: u32,
        bytes: *const c_void,
        length: u32,
    ) -> i32;
    fn GetLengthSid(sid: *mut c_void) -> u32;
    fn RegOpenKeyExW(
        key: *mut c_void,
        name: *const u16,
        options: u32,
        access: u32,
        out: *mut *mut c_void,
    ) -> i32;
    fn RegEnumKeyExW(
        key: *mut c_void,
        index: u32,
        name: *mut u16,
        len: *mut u32,
        reserved: *mut u32,
        class: *mut u16,
        class_len: *mut u32,
        time: *mut c_void,
    ) -> i32;
    fn RegQueryValueExW(
        key: *mut c_void,
        name: *const u16,
        reserved: *mut u32,
        kind: *mut u32,
        data: *mut u8,
        len: *mut u32,
    ) -> i32;
    fn RegCloseKey(key: *mut c_void) -> i32;
    fn InitializeSecurityDescriptor(descriptor: *mut c_void, revision: u32) -> i32;
    fn SetSecurityDescriptorDacl(
        descriptor: *mut c_void,
        present: i32,
        dacl: *mut c_void,
        defaulted: i32,
    ) -> i32;
    fn GetSecurityDescriptorControl(
        descriptor: *mut c_void,
        control: *mut u16,
        revision: *mut u32,
    ) -> i32;
    fn SetSecurityDescriptorControl(descriptor: *mut c_void, mask: u16, bits: u16) -> i32;
    fn SetFileSecurityW(path: *const u16, information: u32, descriptor: *mut c_void) -> i32;
}

fn ancestor_error(path: &Path, error: io::Error) -> SandboxError {
    SandboxError::AncestorAccess {
        path: path.to_path_buf(),
        win32_error: error.raw_os_error().unwrap_or(13) as u32, // ERROR_INVALID_DATA
    }
}

fn physical_path(path: &Path) -> io::Result<PathBuf> {
    let name = wide(path);
    // No OPEN_REPARSE_POINT: follow junctions & symlinks before walking parents.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            0,
            7,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        let mut buffer = vec![0u16; 512];
        loop {
            let len = unsafe {
                GetFinalPathNameByHandleW(handle, buffer.as_mut_ptr(), buffer.len() as u32, 0)
            };
            if len == 0 {
                return Err(io::Error::last_os_error());
            }
            if (len as usize) < buffer.len() {
                return Ok(PathBuf::from(std::ffi::OsString::from_wide(
                    &buffer[..len as usize],
                )));
            }
            buffer.resize(len as usize + 1, 0);
        }
    })();
    unsafe { CloseHandle(handle) };
    result
}

fn ancestor_chain(path: &Path) -> Result<Vec<PathBuf>, SandboxError> {
    let physical = physical_path(path).map_err(|e| ancestor_error(path, e))?;
    // Volume roots (`\?\D:\`) are skipped: a user cannot edit their DACL, and AppContainer
    // stat of a root does not need a lease (the failures were on admin-owned subfolders).
    Ok(physical
        .ancestors()
        .skip(1)
        .filter(|p| !p.as_os_str().is_empty() && p.parent().is_some())
        .map(Path::to_path_buf)
        .collect())
}

struct RegistryKey(*mut c_void);
impl Drop for RegistryKey {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}

fn open_key(parent: *mut c_void, name: &str) -> io::Result<Option<RegistryKey>> {
    let mut key = std::ptr::null_mut();
    let status = unsafe { RegOpenKeyExW(parent, wide(name).as_ptr(), 0, 0x20019, &mut key) };
    match status {
        0 => Ok(Some(RegistryKey(key))),
        2 => Ok(None),
        n => Err(io::Error::from_raw_os_error(n)),
    }
}

struct DerivedSid(*mut c_void);
impl Drop for DerivedSid {
    fn drop(&mut self) {
        unsafe { FreeSid(self.0) };
    }
}

fn derive_sid(name: &str) -> io::Result<DerivedSid> {
    let mut sid = std::ptr::null_mut();
    let hr = unsafe { DeriveAppContainerSidFromAppContainerName(wide(name).as_ptr(), &mut sid) };
    if hr < 0 {
        return Err(io::Error::from_raw_os_error(hr & 0xffff));
    }
    Ok(DerivedSid(sid))
}

fn sid_string(sid: *mut c_void) -> io::Result<String> {
    let mut text = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let value = unsafe { read_wide(text) };
    unsafe { LocalFree(text.cast()) };
    Ok(value)
}

fn validate_owner_prefix(prefix: &str) -> Result<(), SandboxError> {
    let valid = (2..=64).contains(&prefix.len())
        && prefix.strip_suffix('.').is_some_and(|name| {
            let segments: Vec<_> = name.split('.').collect();
            segments.len() >= 2
                && segments.iter().all(|segment| {
                    !segment.is_empty()
                        && segment
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                })
        });
    if !valid {
        return Err(SandboxError::InvalidPolicy(
            "owner prefix must be 2..=64 bytes of at least two nonempty lowercase ASCII \
             letter/digit/hyphen segments separated by '.', ending with '.'"
                .into(),
        ));
    }
    Ok(())
}

/// This user's registered profiles whose name this crate could have made under `prefix` and
/// whose mapping is the SID that name derives to: (lower-case name, SID text).
pub(super) fn owned_profiles(prefix: &str) -> io::Result<Vec<(String, String)>> {
    let mut owned = Vec::new();
    for (profile, mapped_sid) in registered_profiles()? {
        if let Some(derived) = owned_mapping(&profile, &mapped_sid, prefix)? {
            owned.push((profile.to_ascii_lowercase(), derived));
        }
    }
    Ok(owned)
}

fn owned_profile_sids(prefix: &str) -> io::Result<Vec<String>> {
    Ok(owned_profiles(prefix)?
        .into_iter()
        .map(|(_, sid)| sid)
        .collect())
}

/// The name of a profile's `holder` or `guard` object. It is in the `Global` namespace, so every
/// logon session of the machine sees it (fast user switching, Remote Desktop), and it carries the
/// user's SID, so two users with the same grant set do not share one. An ordinary user may create
/// mutexes there; the create-global privilege is for sections and symbolic links.
pub(super) fn profile_object_text(kind: &str, profile: &str) -> io::Result<String> {
    Ok(format!(
        r"Global\rightkit-profile-{kind}-{}-{}",
        current_user_sid()?,
        profile.to_ascii_lowercase()
    ))
}

fn profile_object_name(kind: &str, profile: &str) -> io::Result<Vec<u16>> {
    Ok(wide(profile_object_text(kind, profile)?))
}

/// Marks a profile as in use for as long as it lives. It is a named kernel object, so it is
/// gone when its last holder exits, however that process ends.
pub(super) struct ProfileHolder(usize);

impl Drop for ProfileHolder {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0 as *mut c_void) };
    }
}

impl ProfileHolder {
    pub(super) fn create(profile: &str) -> io::Result<Self> {
        note_mutation("profile_holder");
        let name = profile_object_name("holder", profile)?;
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(handle as usize))
        }
    }
}

/// Whether any process holds a [`ProfileHolder`] for `profile`.
pub(super) fn profile_holder_exists(profile: &str) -> io::Result<bool> {
    let name = profile_object_name("holder", profile)?;
    let handle = unsafe { OpenMutexW(0x0010_0000, 0, name.as_ptr()) }; // SYNCHRONIZE
    if !handle.is_null() {
        unsafe { CloseHandle(handle) };
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(2) => Ok(false), // ERROR_FILE_NOT_FOUND alone proves no holder.
        Some(5) => Ok(true),  // ERROR_ACCESS_DENIED still proves existence.
        _ => Err(error),
    }
}

/// Exclusive, short-lived ownership of one profile's create-or-delete step, across processes.
/// A spawn holds it while it marks the profile in use and creates or adopts it; the maintenance
/// call holds it while it checks for a holder and deletes. Must be dropped on the thread that
/// acquired it.
pub(super) struct ProfileGuard(usize);

impl Drop for ProfileGuard {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.0 as *mut c_void);
            CloseHandle(self.0 as *mut c_void);
        }
    }
}

impl ProfileGuard {
    /// `None` when another process kept it for longer than `wait_ms`.
    pub(super) fn acquire(profile: &str, wait_ms: u32) -> io::Result<Option<Self>> {
        note_mutation("profile_guard");
        let name = profile_object_name("guard", profile)?;
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // 0 is WAIT_OBJECT_0; 0x80 is WAIT_ABANDONED (the last owner died holding it, and the
        // mutex is ours now); 0x102 is WAIT_TIMEOUT.
        match unsafe { WaitForSingleObject(handle, wait_ms) } {
            0 | 0x80 => Ok(Some(Self(handle as usize))),
            0x102 => {
                unsafe { CloseHandle(handle) };
                Ok(None)
            }
            _ => {
                let error = io::Error::last_os_error();
                unsafe { CloseHandle(handle) };
                Err(error)
            }
        }
    }
}

fn owned_mapping(profile: &str, mapped_sid: &str, prefix: &str) -> io::Result<Option<String>> {
    let profile = profile.to_ascii_lowercase();
    let Some(suffix) = profile.strip_prefix(prefix) else {
        return Ok(None);
    };
    if suffix.len() != 16 || !suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(None);
    }
    let sid = derive_sid(&profile)?;
    let derived = sid_string(sid.0)?;
    Ok(derived.eq_ignore_ascii_case(mapped_sid).then_some(derived))
}

/// SIDs are hashes, so a textual S-1-15-2 prefix cannot establish ownership.
/// Enumerate persisted profiles, validate our exact name format, then derive & compare.
pub(super) fn registered_profiles() -> io::Result<Vec<(String, String)>> {
    let hkcu = (-2147483647isize) as *mut c_void;
    let Some(mappings) = open_key(
        hkcu,
        r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppContainer\Mappings",
    )?
    else {
        return Ok(Vec::new());
    };
    let mut owned = Vec::new();
    let mut index = 0;
    loop {
        let mut name = [0u16; 256];
        let mut len = name.len() as u32;
        let status = unsafe {
            RegEnumKeyExW(
                mappings.0,
                index,
                name.as_mut_ptr(),
                &mut len,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if status == 259 {
            break;
        } // ERROR_NO_MORE_ITEMS
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status));
        }
        index += 1;
        let mapped_sid = String::from_utf16_lossy(&name[..len as usize]);
        let Some(key) = open_key(mappings.0, &mapped_sid)? else {
            continue;
        };
        let mut moniker = [0u16; 256];
        let mut bytes = (moniker.len() * 2) as u32;
        let mut kind = 0;
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                wide("Moniker").as_ptr(),
                std::ptr::null_mut(),
                &mut kind,
                moniker.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if status == 2 {
            continue;
        }
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status));
        }
        if kind != 1 || !bytes.is_multiple_of(2) {
            continue;
        } // REG_SZ
        let units = &moniker[..bytes as usize / 2];
        let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
        let profile = String::from_utf16_lossy(&units[..end]);
        owned.push((profile, mapped_sid));
    }
    Ok(owned)
}

struct DaclSnapshot {
    descriptor: *mut c_void,
    dacl: *mut c_void,
}
impl Drop for DaclSnapshot {
    fn drop(&mut self) {
        unsafe { LocalFree(self.descriptor) };
    }
}
impl DaclSnapshot {
    fn read(path: &Path) -> io::Result<Self> {
        let mut descriptor = std::ptr::null_mut();
        let mut dacl = std::ptr::null_mut();
        let status = unsafe {
            GetNamedSecurityInfoW(
                wide(path).as_mut_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(Self { descriptor, dacl })
    }

    fn revision(&self) -> u8 {
        if self.dacl.is_null() {
            ACL_REVISION
        } else {
            unsafe { (*(self.dacl as *const Acl)).acl_revision }
        }
    }

    fn aces(&self) -> io::Result<Vec<Vec<u8>>> {
        if self.dacl.is_null() {
            return Ok(Vec::new());
        }
        let acl = unsafe { &*(self.dacl as *const Acl) };
        let mut aces = Vec::new();
        for index in 0..u32::from(acl.ace_count) {
            let mut ace = std::ptr::null_mut();
            if unsafe { GetAce(self.dacl, index, &mut ace) } == 0 {
                return Err(io::Error::last_os_error());
            }
            let header = unsafe { &*(ace as *const AceHeader) };
            let offset = (ace as usize).wrapping_sub(self.dacl as usize);
            let size = usize::from(header.ace_size);
            if size < 4 || offset + size > usize::from(acl.acl_size) {
                return Err(io::Error::from_raw_os_error(13));
            }
            aces.push(unsafe { std::slice::from_raw_parts(ace.cast::<u8>(), size) }.to_vec());
        }
        Ok(aces)
    }
}

/// Decode only basic allow/deny ACEs, checking inline SID bounds before EqualSid.
fn basic_ace(ace: &[u8]) -> Option<(u8, u8, u32, *mut c_void)> {
    if ace.len() < 16 || ![ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE].contains(&ace[0]) {
        return None;
    }
    let sid_len = 8 + usize::from(ace[9]) * 4;
    if ace[8] != 1 || 8 + sid_len > ace.len() {
        return None;
    }
    Some((
        ace[0],
        ace[1],
        u32::from_le_bytes(ace[4..8].try_into().unwrap()),
        ace[8..].as_ptr() as *mut c_void,
    ))
}

fn mapped_mask(mut mask: u32) -> u32 {
    if mask & 0x8000_0000 != 0 {
        mask |= FILE_GENERIC_READ;
    }
    if mask & 0x4000_0000 != 0 {
        mask |= FILE_GENERIC_WRITE;
    }
    if mask & 0x2000_0000 != 0 {
        mask |= FILE_GENERIC_EXECUTE;
    }
    if mask & 0x1000_0000 != 0 {
        mask |= 0x001f_01ff;
    }
    mask & 0x0fff_ffff
}

/// Ordered DACL evaluation for the lowbox SID pass (the caller already has access).
/// Accumulate split allows; applicable denies are never mistaken for permission.
fn allows_ancestor(snapshot: &DaclSnapshot, sid: &LocalSid) -> io::Result<bool> {
    if snapshot.dacl.is_null() {
        return Ok(true);
    } // NULL DACL allows all
    let packages = LocalSid::parse(ALL_APP_PACKAGES_SID)?;
    let everyone = LocalSid::parse("S-1-1-0")?;
    let mut remaining = ANCESTOR_ACCESS;
    for ace in snapshot.aces()? {
        let Some((kind, flags, mask, trustee)) = basic_ace(&ace) else {
            // An object/callback ACE needs token-aware evaluation. Fail closed rather
            // than infer permission from a later simple allow.
            if ace[0] != ACCESS_ALLOWED_ACE_TYPE && ace[0] != ACCESS_DENIED_ACE_TYPE {
                return Ok(false);
            }
            continue;
        };
        if flags & INHERIT_ONLY_ACE != 0 {
            continue;
        }
        let lowbox = [sid.0, packages.0]
            .iter()
            .any(|&group| unsafe { EqualSid(trustee, group) } != 0);
        let world_deny =
            kind == ACCESS_DENIED_ACE_TYPE && unsafe { EqualSid(trustee, everyone.0) } != 0;
        if !lowbox && !world_deny {
            continue;
        }
        let relevant = mapped_mask(mask) & remaining;
        if kind == ACCESS_DENIED_ACE_TYPE && relevant != 0 {
            return Ok(false);
        }
        if kind == ACCESS_ALLOWED_ACE_TYPE {
            remaining &= !relevant;
        }
        if remaining == 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

fn allow_ace(sid: &LocalSid) -> Vec<u8> {
    let sid_len = unsafe { GetLengthSid(sid.0) } as usize;
    let mut ace = vec![0; 8 + sid_len];
    ace[0] = ACCESS_ALLOWED_ACE_TYPE;
    let size = (ace.len() as u16).to_le_bytes();
    ace[2..4].copy_from_slice(&size);
    ace[4..8].copy_from_slice(&ANCESTOR_ACCESS.to_le_bytes());
    ace[8..].copy_from_slice(unsafe { std::slice::from_raw_parts(sid.0.cast::<u8>(), sid_len) });
    ace
}

type DaclEditor<'a> = dyn Fn(&Path, &[Vec<u8>], u8) -> io::Result<()> + 'a;

/// Preserve all original ACE bytes/order; never REVOKE_ACCESS (which removes other grants).
fn write_dacl(path: &Path, aces: &[Vec<u8>], revision: u8) -> io::Result<()> {
    let size = 8 + aces.iter().map(Vec::len).sum::<usize>();
    if size > u16::MAX as usize {
        return Err(io::Error::from_raw_os_error(1344));
    }
    let mut storage = vec![0u32; size.div_ceil(4)];
    let acl = storage.as_mut_ptr().cast();
    if unsafe { InitializeAcl(acl, size as u32, u32::from(revision)) } == 0 {
        return Err(io::Error::last_os_error());
    }
    for ace in aces {
        if unsafe {
            AddAce(
                acl,
                u32::from(revision),
                u32::MAX,
                ace.as_ptr().cast(),
                ace.len() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    // SetNamedSecurityInfo/SetSecurityInfo re-propagate inheritable ACEs through the whole
    // subtree (e.g. an entire user profile below C:\Users\<name>) on every write. Ancestor
    // ACEs are non-inheritable & inherited ACEs are written back verbatim, so write the DACL
    // in place with SetFileSecurityW, keeping the protected/auto-inherited control bits.
    const SE_DACL_AUTO_INHERITED: u16 = 0x0400;
    const SE_DACL_PROTECTED: u16 = 0x1000;
    let current = DaclSnapshot::read(path)?;
    let (mut control, mut sd_revision) = (0u16, 0u32);
    if unsafe { GetSecurityDescriptorControl(current.descriptor, &mut control, &mut sd_revision) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    // AUTO_INHERIT_REQ makes the kernel keep AUTO_INHERITED for this object only; it never
    // walks descendants (that propagation lives in the advapi Set*SecurityInfo layer).
    const SE_DACL_AUTO_INHERIT_REQ: u16 = 0x0100;
    let keep = SE_DACL_AUTO_INHERITED | SE_DACL_PROTECTED;
    let request = if control & SE_DACL_AUTO_INHERITED != 0 {
        SE_DACL_AUTO_INHERIT_REQ
    } else {
        0
    };
    // Absolute SECURITY_DESCRIPTOR is 40 bytes on 64-bit; over-allocate, 8-byte aligned.
    let mut descriptor = [0u64; 8];
    let sd = descriptor.as_mut_ptr().cast();
    let ok = unsafe {
        InitializeSecurityDescriptor(sd, 1) != 0
            && SetSecurityDescriptorDacl(sd, 1, acl, 0) != 0
            && SetSecurityDescriptorControl(
                sd,
                keep | SE_DACL_AUTO_INHERIT_REQ,
                (control & keep) | request,
            ) != 0
            && SetFileSecurityW(wide(path).as_ptr(), DACL_SECURITY_INFORMATION, sd) != 0
    };
    if ok {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

struct AncestorEntry {
    count: usize,
    path: PathBuf,
    sid_text: String,
    ace: Vec<u8>,
    _mutex: AncestorMutex,
}

/// The lease's name in the machine-wide namespace: every logon session and every user sees it.
fn ancestor_mutex_name(path: &Path, sid: &str) -> io::Result<String> {
    Ok(format!(
        r"Global\rightkit-ancestor-lease-{}",
        ancestor_mutex_hex(path, sid)?
    ))
}

/// The name rightkit-sandbox 0.3.3 and earlier use. Kept so old and new processes in one logon
/// session see each other's leases.
fn legacy_ancestor_mutex_name(path: &Path, sid: &str) -> io::Result<String> {
    Ok(format!(
        r"Local\rightkit-ancestor-lease-{}",
        ancestor_mutex_hex(path, sid)?
    ))
}

/// Paths supplied here come from physical_path; normalize case & separators once.
fn ancestor_mutex_hex(path: &Path, sid: &str) -> io::Result<String> {
    let input = format!(
        "{}|{}",
        lexical_path_key(path).to_lowercase(),
        sid.to_ascii_uppercase()
    );
    let input_len = u32::try_from(input.len()).map_err(|_| io::Error::from_raw_os_error(87))?;
    let mut digest = [0u8; 32];
    let status = unsafe {
        BCryptHash(
            0x41usize as *mut c_void, // BCRYPT_SHA256_ALG_HANDLE (pseudo-handle)
            std::ptr::null(),
            0,
            input.as_ptr(),
            input_len,
            digest.as_mut_ptr(),
            digest.len() as u32,
        )
    };
    if status < 0 {
        return Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(status) } as i32,
        ));
    }
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// An entry is stale only when its lease mutex was proved absent. A proof that failed keeps it.
fn proved_stale(path: &Path, sid: &str, proof: io::Result<bool>) -> bool {
    match proof {
        Ok(exists) => !exists,
        Err(error) => {
            eprintln!(
                "rightkit-sandbox: ancestor ACE kept for {} ({sid}): liveness unread: {error}",
                path.display()
            );
            false
        }
    }
}

const SYNCHRONIZE: u32 = 0x0010_0000;

// Handle ownership is independent of threads; no waits or mutex ownership are used.
struct MutexHandle(usize);
impl Drop for MutexHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0 as *mut c_void) };
    }
}

#[repr(C)]
struct MutexSecurityAttributes {
    length: u32,
    descriptor: *mut c_void,
    inherit: i32,
}

/// Create or open the machine-wide lease. The creator has full access and every authenticated
/// user may open it for `SYNCHRONIZE`, which is all a second holder asks for: a user who shares
/// the folder and the grant set then holds the same object for as long as its own child runs.
fn create_shared_mutex(name: &str) -> io::Result<MutexHandle> {
    let sddl = wide(format!(
        "D:(A;;GA;;;{})(A;;0x100000;;;AU)",
        current_user_sid()?
    ));
    let mut descriptor = std::ptr::null_mut();
    let mut size = 0u32;
    // SAFETY: NUL-terminated SDDL and valid out-pointers.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            &mut size,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let attributes = MutexSecurityAttributes {
        length: std::mem::size_of::<MutexSecurityAttributes>() as u32,
        descriptor,
        inherit: 0,
    };
    let name = wide(name);
    // SAFETY: the attributes and the name outlive the call. An existing object is opened with
    // SYNCHRONIZE only and its own security descriptor is left as it is.
    let handle = unsafe {
        CreateMutexExW(
            (&raw const attributes).cast(),
            name.as_ptr(),
            0,
            SYNCHRONIZE,
        )
    };
    let error = io::Error::last_os_error();
    // SAFETY: allocated by the SDDL conversion; freed once.
    unsafe { LocalFree(descriptor) };
    if handle.is_null() {
        Err(error)
    } else {
        Ok(MutexHandle(handle as usize))
    }
}

/// One lease, held under both names for as long as the value lives.
struct AncestorMutex {
    _global: MutexHandle,
    _legacy: MutexHandle,
}
impl AncestorMutex {
    fn create(path: &Path, sid: &str) -> io::Result<Self> {
        let global = create_shared_mutex(&ancestor_mutex_name(path, sid)?)?;
        let name = wide(legacy_ancestor_mutex_name(path, sid)?);
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            _global: global,
            _legacy: MutexHandle(handle as usize),
        })
    }
}

fn named_mutex_exists(name: &str) -> io::Result<bool> {
    let name = wide(name);
    let handle = unsafe { OpenMutexW(SYNCHRONIZE, 0, name.as_ptr()) };
    if !handle.is_null() {
        drop(MutexHandle(handle as usize));
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(2) => Ok(false), // ERROR_FILE_NOT_FOUND alone proves no holder.
        Some(5) => Ok(true),  // ERROR_ACCESS_DENIED still proves existence.
        _ => Err(error),
    }
}

/// Live under either name is live. Absent needs both names proved absent.
fn ancestor_mutex_exists(path: &Path, sid: &str) -> io::Result<bool> {
    let global = named_mutex_exists(&ancestor_mutex_name(path, sid)?);
    let legacy = named_mutex_exists(&legacy_ancestor_mutex_name(path, sid)?);
    match (global, legacy) {
        (Ok(true), _) | (_, Ok(true)) => Ok(true),
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(false), Ok(false)) => Ok(false),
    }
}

fn cleanup_ancestor(entry: &AncestorEntry) {
    let result = (|| {
        let snapshot = DaclSnapshot::read(&entry.path)?;
        let mut aces = snapshot.aces()?;
        if let Some(index) = aces.iter().position(|ace| *ace == entry.ace) {
            aces.remove(index);
            write_dacl(&entry.path, &aces, snapshot.revision())?;
        }
        Ok::<_, io::Error>(())
    })();
    if let Err(error) = result {
        eprintln!(
            "rightkit-sandbox: ancestor ACE cleanup failed for {} ({}) : {error}",
            entry.path.display(),
            entry.sid_text
        );
    }
}
fn registry() -> &'static Mutex<HashMap<String, AncestorEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, AncestorEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}
fn key(path: &Path, sid: &str) -> String {
    format!("{sid}|{}", lexical_path_key(path))
}

fn sweep_chain(
    chain: &[PathBuf],
    owned: &[String],
    live: &HashMap<String, AncestorEntry>,
    grants: &HashMap<String, LeaseEntry>,
    editor: &DaclEditor<'_>,
) -> Result<usize, SandboxError> {
    let owned: Vec<_> = owned
        .iter()
        .map(|s| LocalSid::parse(s).map(|sid| (s, sid)))
        .collect::<io::Result<_>>()?;
    let mut removed = 0;
    for path in chain {
        let snapshot = DaclSnapshot::read(path).map_err(|e| ancestor_error(path, e))?;
        let original = snapshot.aces().map_err(|e| ancestor_error(path, e))?;
        let before = original.len();
        let mut aces = Vec::with_capacity(before);
        for ace in original {
            let mut stale = false;
            if let Some((ACCESS_ALLOWED_ACE_TYPE, flags, _, trustee)) = basic_ace(&ace) {
                if flags & INHERITED_ACE == 0 {
                    for (text, sid) in &owned {
                        let k = key(path, text);
                        if !live.contains_key(&k)
                            && !grants.contains_key(&k)
                            && unsafe { EqualSid(trustee, sid.0) } != 0
                        {
                            stale = proved_stale(path, text, ancestor_mutex_exists(path, text));
                            break;
                        }
                    }
                }
            }
            if !stale {
                aces.push(ace);
            }
        }
        if aces.len() != before {
            editor(path, &aces, snapshot.revision()).map_err(|e| ancestor_error(path, e))?;
            removed += before - aces.len();
        }
    }
    Ok(removed)
}

/// Remove stale explicit allow ACEs belonging to registered `rightkit.sandbox.` profiles
/// from physical ancestors of `path` (excluding `path`). Returns removed ACE count.
/// Live leases in this process & other processes of any user in any logon session (`Global\`
/// mutex namespace), plus unrelated SIDs, are preserved. An entry whose liveness cannot be
/// read is kept, never removed. Paths must exist.
/// Crashed holders become sweepable when their kernel handles close.
pub fn sweep_stale_appcontainer_ancestor_aces(path: &Path) -> Result<usize, SandboxError> {
    sweep_stale_appcontainer_ancestor_aces_for_owner(path, PROFILE_PREFIX)
}

/// Remove stale explicit allow ACEs belonging to registered profiles under `owner_prefix`
/// from physical ancestors of `path` (excluding `path`). Returns removed ACE count.
/// Prefixes require at least two nonempty lowercase ASCII letter/digit/hyphen segments,
/// separated by '.', ending with '.', within 2..=64 bytes. Ownership requires a 16-hex
/// suffix & a derived SID matching its persisted mapping. Paths must exist.
/// Live leases in this process & other processes of any user in any logon session (`Global\`
/// mutex namespace), plus unrelated SIDs, are preserved. An entry whose liveness cannot be
/// read is kept, never removed.
/// Crashed holders become sweepable when their kernel handles close.
pub fn sweep_stale_appcontainer_ancestor_aces_for_owner(
    path: &Path,
    owner_prefix: &str,
) -> Result<usize, SandboxError> {
    validate_owner_prefix(owner_prefix)?;
    let chain = ancestor_chain(path)?;
    let owned = owned_profile_sids(owner_prefix)?;
    let live = registry().lock().unwrap_or_else(|e| e.into_inner());
    let grants = lease_registry().lock().unwrap_or_else(|e| e.into_inner());
    let _acl = ACL_GRANT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    sweep_chain(&chain, &owned, &live, &grants, &write_dacl)
}

/// Ancestor acquisition behavior when access cannot be granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AncestorLeasePolicy {
    /// Fail & roll back if any ancestor cannot be leased.
    Strict,
    /// Record ancestors denied DACL writes or verification & lease remaining ancestors.
    BestEffort,
}

/// An ancestor skipped because its DACL could not grant access.
#[derive(Debug, Clone)]
pub struct UnleasableAncestor {
    pub path: PathBuf,
    pub win32_error: u32,
}

/// Grants traverse (SYNCHRONIZE | READ_ATTRIBUTES | LIST_DIRECTORY) on physical ancestors
/// of `path` (excluding `path`) for the given AppContainer SID for the lease lifetime.
pub struct AppContainerAncestorLease {
    _lease: AncestorLease,
}

impl AppContainerAncestorLease {
    /// Acquire an ancestor lease for a parseable `S-1-15-2-` SID (case-insensitive).
    /// Uses [`AncestorLeasePolicy::Strict`].
    pub fn acquire(path: &Path, appcontainer_sid: &str) -> Result<Self, SandboxError> {
        Self::acquire_with(path, appcontainer_sid, AncestorLeasePolicy::Strict)
    }

    /// Acquire with an explicit ancestor policy.
    /// BestEffort traversal through skipped ancestors relies on the token's bypass-traverse
    /// privilege. Directory listing there can fail, including upward git repository discovery;
    /// surface skipped paths to callers (e.g. via `GIT_CEILING_DIRECTORIES`) rather than abort.
    pub fn acquire_with(
        path: &Path,
        appcontainer_sid: &str,
        policy: AncestorLeasePolicy,
    ) -> Result<Self, SandboxError> {
        let text = appcontainer_sid.to_ascii_uppercase();
        if !text.starts_with("S-1-15-2-") || text.contains('\0') {
            return Err(SandboxError::InvalidPolicy(
                "ancestor lease requires an AppContainer SID starting with S-1-15-2-".into(),
            ));
        }
        let sid = LocalSid::parse(&text)
            .map_err(|e| SandboxError::InvalidPolicy(format!("invalid AppContainer SID: {e}")))?;
        // Canonical text shares refcounts with backend leases & sweep ownership keys.
        let text = sid_string(sid.0)?;
        Ok(Self {
            _lease: AncestorLease::acquire_with(path, &text, policy)?,
        })
    }

    /// Ancestors skipped under BestEffort; Strict leases always return an empty slice.
    pub fn unleasable(&self) -> &[UnleasableAncestor] {
        &self._lease.unleasable
    }
}

const _: fn() = || {
    fn a<T: Send + Sync>() {}
    a::<AppContainerAncestorLease>();
};

pub(super) struct AncestorLease {
    keys: Vec<String>,
    unleasable: Vec<UnleasableAncestor>,
}
impl AncestorLease {
    /// Ancestors skipped under [`AncestorLeasePolicy::BestEffort`]; empty for a strict lease.
    pub(super) fn unleasable(&self) -> &[UnleasableAncestor] {
        &self.unleasable
    }

    pub(super) fn acquire_with(
        path: &Path,
        sid_text: &str,
        policy: AncestorLeasePolicy,
    ) -> Result<Self, SandboxError> {
        Self::acquire_chain_with(
            &ancestor_chain(path)?,
            sid_text,
            &owned_profile_sids(PROFILE_PREFIX)?,
            &write_dacl,
            policy,
        )
    }

    #[cfg(test)]
    fn acquire_chain(
        chain: &[PathBuf],
        sid_text: &str,
        owned: &[String],
        editor: &DaclEditor<'_>,
    ) -> Result<Self, SandboxError> {
        Self::acquire_chain_with(chain, sid_text, owned, editor, AncestorLeasePolicy::Strict)
    }

    fn record_unleasable(
        &mut self,
        path: &Path,
        sid: &LocalSid,
        policy: AncestorLeasePolicy,
        error: &SandboxError,
    ) -> Result<bool, SandboxError> {
        if policy == AncestorLeasePolicy::BestEffort {
            if let SandboxError::AncestorAccess { win32_error, .. } = error {
                if io::Error::from_raw_os_error(*win32_error as i32).kind()
                    == io::ErrorKind::PermissionDenied
                {
                    let current = DaclSnapshot::read(path).map_err(|e| ancestor_error(path, e))?;
                    if !allows_ancestor(&current, sid).map_err(|e| ancestor_error(path, e))? {
                        self.unleasable.push(UnleasableAncestor {
                            path: path.to_path_buf(),
                            win32_error: *win32_error,
                        });
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    fn acquire_chain_with(
        chain: &[PathBuf],
        sid_text: &str,
        owned: &[String],
        editor: &DaclEditor<'_>,
        policy: AncestorLeasePolicy,
    ) -> Result<Self, SandboxError> {
        let sid = LocalSid::parse(sid_text)?;
        let mut lease = Self {
            keys: Vec::new(),
            unleasable: Vec::new(),
        };
        let result = (|| {
            let mut live = registry().lock().unwrap_or_else(|e| e.into_inner());
            let grants = lease_registry().lock().unwrap_or_else(|e| e.into_inner());
            let _acl = ACL_GRANT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            if policy == AncestorLeasePolicy::Strict {
                sweep_chain(chain, owned, &live, &grants, editor)?;
            }
            for path in chain {
                if policy == AncestorLeasePolicy::BestEffort {
                    // Only sweep writes may be skipped; reads stay fatal. An unread lease
                    // mutex keeps its entry.
                    let sweep_write_failed = std::cell::Cell::new(false);
                    let swept = sweep_chain(
                        std::slice::from_ref(path),
                        owned,
                        &live,
                        &grants,
                        &|path, aces, revision| {
                            let result = editor(path, aces, revision);
                            sweep_write_failed.set(result.is_err());
                            result
                        },
                    );
                    if let Err(error) = swept {
                        if sweep_write_failed.get()
                            && lease.record_unleasable(path, &sid, policy, &error)?
                        {
                            continue;
                        }
                        return Err(error);
                    }
                }
                let k = key(path, sid_text);
                if let Some(entry) = live.get_mut(&k) {
                    entry.count += 1;
                    lease.keys.push(k);
                    continue;
                }
                let snapshot = DaclSnapshot::read(path).map_err(|e| ancestor_error(path, e))?;
                if allows_ancestor(&snapshot, &sid).map_err(|e| ancestor_error(path, e))? {
                    continue;
                }
                let ace = allow_ace(&sid);
                let mut aces = snapshot.aces().map_err(|e| ancestor_error(path, e))?;
                // Explicit allows precede inherited ACEs, preserving canonical DACL order.
                let index = aces
                    .iter()
                    .position(|a| a[1] & INHERITED_ACE != 0)
                    .unwrap_or(aces.len());
                aces.insert(index, ace.clone());
                // Create before writing so a failed liveness proof cannot leave an untracked ACE.
                let mutex =
                    AncestorMutex::create(path, sid_text).map_err(|e| ancestor_error(path, e))?;
                if let Err(error) = editor(path, &aces, snapshot.revision()) {
                    let error = ancestor_error(path, error);
                    if lease.record_unleasable(path, &sid, policy, &error)? {
                        continue;
                    }
                    return Err(error);
                }
                live.insert(
                    k.clone(),
                    AncestorEntry {
                        count: 1,
                        path: path.clone(),
                        sid_text: sid_text.into(),
                        ace,
                        _mutex: mutex,
                    },
                );
                lease.keys.push(k);
                let verified = (|| {
                    let after = DaclSnapshot::read(path).map_err(|e| ancestor_error(path, e))?;
                    if !allows_ancestor(&after, &sid).map_err(|e| ancestor_error(path, e))? {
                        return Err(SandboxError::AncestorAccess {
                            path: path.clone(),
                            win32_error: 5,
                        });
                    }
                    Ok(())
                })();
                if let Err(error) = verified {
                    if lease.record_unleasable(path, &sid, policy, &error)? {
                        let k = lease.keys.pop().unwrap();
                        let entry = live.remove(&k).unwrap();
                        cleanup_ancestor(&entry);
                        continue;
                    }
                    return Err(error);
                }
            }
            Ok(())
        })();
        result?; // On failure guard rolls back every successful earlier addition.
        Ok(lease)
    }
}

impl Drop for AncestorLease {
    fn drop(&mut self) {
        let mut live = registry().lock().unwrap_or_else(|e| e.into_inner());
        let _acl = ACL_GRANT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for k in self.keys.drain(..).rev() {
            let Some(entry) = live.get_mut(&k) else {
                continue;
            };
            entry.count -= 1;
            if entry.count != 0 {
                continue;
            }
            let entry = live.remove(&k).unwrap();
            cleanup_ancestor(&entry);
        }
    }
}

#[cfg(test)]
#[path = "windows_ancestor_tests.rs"]
mod tests;
