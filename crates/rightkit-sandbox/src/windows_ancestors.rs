//! Physical-ancestor leases. Included only by the Windows AppContainer backend.
use super::*;
use std::os::windows::ffi::OsStringExt;

pub(super) const PROFILE_PREFIX: &str = "rightkit.sandbox.";
const ANCESTOR_ACCESS: u32 = 0x0010_0081; // SYNCHRONIZE | READ_ATTRIBUTES | LIST_DIRECTORY
const ACCESS_DENIED_ACE_TYPE: u8 = 1;
const INHERITED_ACE: u8 = 0x10;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetFinalPathNameByHandleW(handle: *mut c_void, path: *mut u16, len: u32, flags: u32) -> u32;
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

fn owned_mapping(profile: &str, mapped_sid: &str) -> io::Result<Option<String>> {
    let profile = profile.to_ascii_lowercase();
    let Some(suffix) = profile.strip_prefix(PROFILE_PREFIX) else {
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
fn owned_profile_sids() -> io::Result<Vec<String>> {
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
        if let Some(derived) = owned_mapping(&profile, &mapped_sid)? {
            owned.push(derived);
        }
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
    let status = unsafe {
        SetNamedSecurityInfoW(
            wide(path).as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl,
            std::ptr::null_mut(),
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status as i32))
    }
}

struct AncestorEntry {
    count: usize,
    path: PathBuf,
    sid_text: String,
    ace: Vec<u8>,
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
        let mut aces = snapshot.aces().map_err(|e| ancestor_error(path, e))?;
        let before = aces.len();
        aces.retain(|ace| {
            let Some((kind, flags, _, trustee)) = basic_ace(ace) else {
                return true;
            };
            if kind != ACCESS_ALLOWED_ACE_TYPE || flags & INHERITED_ACE != 0 {
                return true;
            }
            !owned.iter().any(|(text, sid)| {
                let k = key(path, text);
                !live.contains_key(&k)
                    && !grants.contains_key(&k)
                    && unsafe { EqualSid(trustee, sid.0) } != 0
            })
        });
        if aces.len() != before {
            editor(path, &aces, snapshot.revision()).map_err(|e| ancestor_error(path, e))?;
            removed += before - aces.len();
        }
    }
    Ok(removed)
}

/// Remove stale explicit allow ACEs belonging to registered `rightkit.sandbox.` profiles
/// from physical ancestors of `path` (excluding `path`). Returns removed ACE count.
/// Live leases in this process & unrelated SIDs are preserved. Paths must exist.
/// Do not sweep while another process using this crate has live sandbox children.
pub fn sweep_stale_appcontainer_ancestor_aces(path: &Path) -> Result<usize, SandboxError> {
    let chain = ancestor_chain(path)?;
    let owned = owned_profile_sids()?;
    let live = registry().lock().unwrap_or_else(|e| e.into_inner());
    let grants = lease_registry().lock().unwrap_or_else(|e| e.into_inner());
    let _acl = ACL_GRANT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    sweep_chain(&chain, &owned, &live, &grants, &write_dacl)
}

pub(super) struct AncestorLease {
    keys: Vec<String>,
}
impl AncestorLease {
    pub(super) fn acquire(path: &Path, sid_text: &str) -> Result<Self, SandboxError> {
        Self::acquire_chain(
            &ancestor_chain(path)?,
            sid_text,
            &owned_profile_sids()?,
            &write_dacl,
        )
    }

    fn acquire_chain(
        chain: &[PathBuf],
        sid_text: &str,
        owned: &[String],
        editor: &DaclEditor<'_>,
    ) -> Result<Self, SandboxError> {
        let sid = LocalSid::parse(sid_text)?;
        let mut lease = Self { keys: Vec::new() };
        let result = (|| {
            let mut live = registry().lock().unwrap_or_else(|e| e.into_inner());
            let grants = lease_registry().lock().unwrap_or_else(|e| e.into_inner());
            let _acl = ACL_GRANT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            sweep_chain(chain, owned, &live, &grants, editor)?;
            for path in chain {
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
                editor(path, &aces, snapshot.revision()).map_err(|e| ancestor_error(path, e))?;
                live.insert(
                    k.clone(),
                    AncestorEntry {
                        count: 1,
                        path: path.clone(),
                        sid_text: sid_text.into(),
                        ace,
                    },
                );
                lease.keys.push(k);
                let after = DaclSnapshot::read(path).map_err(|e| ancestor_error(path, e))?;
                if !allows_ancestor(&after, &sid).map_err(|e| ancestor_error(path, e))? {
                    return Err(SandboxError::AncestorAccess {
                        path: path.clone(),
                        win32_error: 5,
                    });
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
    }
}

#[cfg(test)]
#[path = "windows_ancestor_tests.rs"]
mod tests;
