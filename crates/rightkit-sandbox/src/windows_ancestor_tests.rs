//! Windows-only hermetic ACL tests; no elevated user or real AppContainer profile required.
use super::*;
use std::sync::atomic::AtomicUsize;

#[link(name = "advapi32")]
unsafe extern "system" {
    fn GetSecurityDescriptorDacl(
        descriptor: *mut c_void,
        present: *mut i32,
        dacl: *mut *mut c_void,
        defaulted: *mut i32,
    ) -> i32;
}

struct TempChain {
    root: PathBuf,
    workspace: PathBuf,
}
impl TempChain {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "rk-ancestors-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let workspace = root.join("ancestor").join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let root = physical_path(&root).unwrap();
        let workspace = physical_path(&workspace).unwrap();
        let tree = Self { root, workspace };
        // Protected DACLs make tests independent of machine-wide Packages grants.
        for path in tree.chain().iter().chain(std::iter::once(&tree.workspace)) {
            private_dacl(path);
        }
        tree
    }

    fn chain(&self) -> Vec<PathBuf> {
        ancestor_chain(&self.workspace)
            .unwrap()
            .into_iter()
            .take_while(|path| path.starts_with(&self.root))
            .collect()
    }

    fn profile(&self) -> (String, String) {
        let name = format!(
            "{PROFILE_PREFIX}{:016x}",
            fnv1a(&lexical_path_key(&self.workspace))
        );
        let sid = sid_string(derive_sid(&name).unwrap().0).unwrap();
        (name, sid)
    }
}
impl Drop for TempChain {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn private_dacl(path: &Path) {
    let user = current_user_sid().unwrap();
    let sddl = wide(format!("D:P(A;;FA;;;{user})"));
    let mut descriptor = std::ptr::null_mut();
    unsafe {
        assert_ne!(
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                std::ptr::null_mut()
            ),
            0
        );
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl = std::ptr::null_mut();
        assert_ne!(
            GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted),
            0
        );
        assert_ne!(present, 0);
        let status = SetNamedSecurityInfoW(
            wide(path).as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | 0x8000_0000, // PROTECTED_DACL_SECURITY_INFORMATION
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl,
            std::ptr::null_mut(),
        );
        LocalFree(descriptor);
        assert_eq!(status, 0);
    }
}

fn aces(path: &Path) -> Vec<Vec<u8>> {
    DaclSnapshot::read(path).unwrap().aces().unwrap()
}

fn sid_aces(path: &Path, sid_text: &str) -> Vec<Vec<u8>> {
    let sid = LocalSid::parse(sid_text).unwrap();
    aces(path)
        .into_iter()
        .filter(|ace| {
            basic_ace(ace)
                .is_some_and(|(_, _, _, trustee)| unsafe { EqualSid(trustee, sid.0) } != 0)
        })
        .collect()
}

fn add_ace(path: &Path, sid_text: &str, mask: u32, flags: u8, kind: u8) {
    let sid = LocalSid::parse(sid_text).unwrap();
    let mut ace = allow_ace(&sid);
    ace[0] = kind;
    ace[1] = flags;
    ace[4..8].copy_from_slice(&mask.to_le_bytes());
    let snapshot = DaclSnapshot::read(path).unwrap();
    let mut entries = snapshot.aces().unwrap();
    let index = if kind == ACCESS_DENIED_ACE_TYPE {
        0
    } else {
        entries.len()
    };
    entries.insert(index, ace);
    write_dacl(path, &entries, snapshot.revision()).unwrap();
}

fn sweep_test_chain(chain: &[PathBuf], owned: &[String]) -> usize {
    let live = registry().lock().unwrap_or_else(|e| e.into_inner());
    let grants = lease_registry().lock().unwrap_or_else(|e| e.into_inner());
    let _acl = ACL_GRANT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    sweep_chain(chain, owned, &live, &grants, &write_dacl).unwrap()
}

#[test]
fn owner_prefix_validation() {
    let longest = format!("a.{}.", "b".repeat(61));
    for prefix in [
        "rightkit.sandbox.",
        "coderight.sandbox.workspace.",
        "a.b.",
        "code-right.sandbox2.",
        longest.as_str(),
    ] {
        assert!(validate_owner_prefix(prefix).is_ok(), "{prefix:?}");
    }
    let too_long = format!("a.{}.", "b".repeat(62));
    for prefix in [
        "",
        ".",
        "a.",
        "S-1-15-2",
        "rightkit..x.",
        ".rightkit.sandbox.",
        "rightkit.sandbox..",
        "rightkit.sandbox",
        "Rightkit.sandbox.",
        "rightkit.sand_box.",
        "rightkit.sand box.",
        "rightkit.sand/box.",
        "rightkit.sandböx.",
        "rightkit.sandbox.\n",
        "rightkit.sandbox.\0",
        too_long.as_str(),
    ] {
        assert!(
            matches!(
                validate_owner_prefix(prefix),
                Err(SandboxError::InvalidPolicy(_))
            ),
            "{prefix:?}"
        );
        // Invalid prefixes fail before resolving paths or reading/writing ACLs.
        assert!(
            matches!(
                crate::sweep_stale_appcontainer_ancestor_aces_for_owner(Path::new(""), prefix),
                Err(SandboxError::InvalidPolicy(_))
            ),
            "{prefix:?}"
        );
    }
}

#[test]
fn owner_mapping_requires_exact_profile_format_and_derived_sid() {
    let prefix = "coderight.sandbox.workspace.";
    let name = format!("{prefix}0123456789abcdef");
    let sid = sid_string(derive_sid(&name).unwrap().0).unwrap();
    assert_eq!(
        owned_mapping(&name, &sid, prefix).unwrap(),
        Some(sid.clone())
    );
    assert_eq!(
        owned_mapping(&name.to_ascii_uppercase(), &sid, prefix).unwrap(),
        Some(sid.clone())
    );
    let other = sid_string(
        derive_sid("coderight.sandbox.workspace.fedcba9876543210")
            .unwrap()
            .0,
    )
    .unwrap();
    assert!(owned_mapping(&name, &other, prefix).unwrap().is_none());
    let wrong_prefix = "rightkit.sandbox.0123456789abcdef";
    let foreign = sid_string(derive_sid(wrong_prefix).unwrap().0).unwrap();
    assert!(owned_mapping(wrong_prefix, &foreign, prefix)
        .unwrap()
        .is_none());
    for suffix in ["0123456789abcdeg", "0123456789abcde", "0123456789abcdef0"] {
        let name = format!("{prefix}{suffix}");
        let sid = sid_string(derive_sid(&name).unwrap().0).unwrap();
        assert!(owned_mapping(&name, &sid, prefix).unwrap().is_none());
    }
}

#[test]
fn public_ancestor_lease_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<crate::AppContainerAncestorLease>();
}

#[test]
fn public_ancestor_lease_rejects_invalid_sid_before_path_access() {
    for sid in [
        "",
        "S-1-1-0",
        "S-1-15-3-1",
        "S-1-15-2-",
        "s-1-15-2-invalid",
        "S-1-15-2-1\0junk",
    ] {
        assert!(
            matches!(
                crate::AppContainerAncestorLease::acquire(Path::new(""), sid),
                Err(SandboxError::InvalidPolicy(_))
            ),
            "{sid:?}"
        );
    }
}

#[test]
fn lease_adds_this_folder_aces_then_removes_exact_additions() {
    let tree = TempChain::new();
    let chain = tree.chain();
    let (_, sid) = tree.profile();
    // A pre-existing, narrower ACE for same SID must survive guard teardown.
    add_ace(&chain[0], &sid, 0x0002_0000, 0, ACCESS_ALLOWED_ACE_TYPE);
    let before: Vec<_> = chain.iter().map(|p| aces(p)).collect();
    let lease = AncestorLease::acquire_chain(&chain, &sid, &[], &write_dacl).unwrap();
    assert_eq!(lease.keys.len(), chain.len());
    for path in &chain {
        let entries = sid_aces(path, &sid);
        assert!(entries.iter().any(|ace| ace[1] == 0
            && u32::from_le_bytes(ace[4..8].try_into().unwrap()) == ANCESTOR_ACCESS));
    }
    assert!(
        sid_aces(&tree.workspace, &sid).is_empty(),
        "ancestor grants must not inherit"
    );
    drop(lease);
    for (path, original) in chain.iter().zip(before) {
        assert_eq!(aces(path), original);
    }
}

#[test]
fn idempotent_reacquire_preserves_live_aces_until_last_guard() {
    let tree = TempChain::new();
    let chain = tree.chain();
    let (_, sid) = tree.profile();
    let owned = vec![sid.clone()];
    let first = AncestorLease::acquire_chain(&chain, &sid, &owned, &write_dacl).unwrap();
    let second = AncestorLease::acquire_chain(&chain, &sid, &owned, &write_dacl).unwrap();
    assert_eq!(sweep_test_chain(&chain, &owned), 0);
    drop(first);
    for path in &chain {
        assert_eq!(sid_aces(path, &sid).len(), 1);
    }
    drop(second);
    for path in &chain {
        assert!(sid_aces(path, &sid).is_empty());
    }
    let third = AncestorLease::acquire_chain(&chain, &sid, &owned, &write_dacl).unwrap();
    drop(third);
    for path in &chain {
        assert!(sid_aces(path, &sid).is_empty());
    }
}

#[test]
fn acquisition_sweeps_stale_broad_grants_before_adding_narrow_aces() {
    let tree = TempChain::new();
    let chain = tree.chain();
    let (_, sid) = tree.profile();
    for path in &chain {
        add_ace(
            path,
            &sid,
            FILE_GENERIC_READ | FILE_GENERIC_WRITE,
            0,
            ACCESS_ALLOWED_ACE_TYPE,
        );
    }
    let lease = AncestorLease::acquire_chain(&chain, &sid, std::slice::from_ref(&sid), &write_dacl)
        .unwrap();
    for path in &chain {
        let entries = sid_aces(path, &sid);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0][1], 0);
        assert_eq!(
            u32::from_le_bytes(entries[0][4..8].try_into().unwrap()),
            ANCESTOR_ACCESS
        );
    }
    drop(lease);
    for path in &chain {
        assert!(sid_aces(path, &sid).is_empty());
    }
}

#[test]
fn split_existing_profile_grants_are_skipped_without_editing() {
    let tree = TempChain::new();
    let chain = tree.chain();
    let (_, sid) = tree.profile();
    for path in &chain {
        add_ace(path, &sid, 0x81, 0, ACCESS_ALLOWED_ACE_TYPE);
        add_ace(path, &sid, 0x0010_0000, 0, ACCESS_ALLOWED_ACE_TYPE);
    }
    let before: Vec<_> = chain.iter().map(|p| aces(p)).collect();
    let editor = |_: &Path, _: &[Vec<u8>], _: u8| -> io::Result<()> {
        panic!("effective split grants must be skipped")
    };
    let lease = AncestorLease::acquire_chain(&chain, &sid, &[], &editor).unwrap();
    assert!(lease.keys.is_empty());
    drop(lease);
    for (path, original) in chain.iter().zip(before) {
        assert_eq!(aces(path), original);
    }
}

#[test]
fn stale_sweep_removes_only_verified_crate_profile_sids() {
    let tree = TempChain::new();
    let chain = tree.chain();
    let (name, sid) = tree.profile();
    let foreign_name = "coderight.ancestor-test";
    let foreign = sid_string(derive_sid(foreign_name).unwrap().0).unwrap();
    assert!(owned_mapping(foreign_name, &foreign, PROFILE_PREFIX)
        .unwrap()
        .is_none());
    assert!(
        owned_mapping(&name, &foreign, PROFILE_PREFIX)
            .unwrap()
            .is_none(),
        "mapping must match derived SID"
    );
    assert!(
        owned_mapping("rightkit.sandbox.not-a-hash", &sid, PROFILE_PREFIX)
            .unwrap()
            .is_none()
    );
    let owned = vec![owned_mapping(&name, &sid, PROFILE_PREFIX).unwrap().unwrap()];
    for path in &chain {
        add_ace(path, &sid, ANCESTOR_ACCESS, 0, ACCESS_ALLOWED_ACE_TYPE);
        add_ace(path, &foreign, ANCESTOR_ACCESS, 0, ACCESS_ALLOWED_ACE_TYPE);
        add_ace(
            path,
            ALL_APP_PACKAGES_SID,
            ANCESTOR_ACCESS,
            0,
            ACCESS_ALLOWED_ACE_TYPE,
        );
    }
    let unrelated: Vec<_> = chain
        .iter()
        .map(|p| (sid_aces(p, &foreign), sid_aces(p, ALL_APP_PACKAGES_SID)))
        .collect();
    assert_eq!(sweep_test_chain(&chain, &owned), chain.len());
    assert_eq!(sweep_test_chain(&chain, &owned), 0);
    for (path, (foreign_before, packages_before)) in chain.iter().zip(unrelated) {
        assert!(sid_aces(path, &sid).is_empty());
        assert_eq!(sid_aces(path, &foreign), foreign_before);
        assert_eq!(sid_aces(path, ALL_APP_PACKAGES_SID), packages_before);
    }
}

#[test]
fn all_application_packages_grant_skips_uneditable_ancestor() {
    let tree = TempChain::new();
    let chain = tree.chain();
    let (_, sid) = tree.profile();
    for path in &chain {
        add_ace(
            path,
            ALL_APP_PACKAGES_SID,
            ANCESTOR_ACCESS,
            0,
            ACCESS_ALLOWED_ACE_TYPE,
        );
    }
    let editor = |_: &Path, _: &[Vec<u8>], _: u8| -> io::Result<()> {
        panic!("already granted ancestor must not be edited")
    };
    let lease = AncestorLease::acquire_chain(&chain, &sid, &[], &editor).unwrap();
    assert!(lease.keys.is_empty());
    for path in &chain {
        assert!(sid_aces(path, &sid).is_empty());
    }
}

#[test]
fn uneditable_ancestor_reports_typed_path_and_win32_code_and_rolls_back() {
    let tree = TempChain::new();
    let chain = tree.chain();
    let (_, sid) = tree.profile();
    let writes = AtomicUsize::new(0);
    let editor = |path: &Path, entries: &[Vec<u8>], revision: u8| {
        if writes.fetch_add(1, Ordering::Relaxed) == 1 {
            Err(io::Error::from_raw_os_error(5))
        } else {
            write_dacl(path, entries, revision)
        }
    };
    match AncestorLease::acquire_chain(&chain, &sid, &[], &editor) {
        Err(SandboxError::AncestorAccess { path, win32_error }) => {
            assert_eq!(path, chain[1]);
            assert_eq!(win32_error, 5);
            let rendered = SandboxError::AncestorAccess {
                path: path.clone(),
                win32_error,
            }
            .to_string();
            assert!(rendered.contains(&path.display().to_string()));
            assert!(rendered.contains("Win32 error 5"));
        }
        _ => panic!("expected typed ancestor access failure"),
    }
    for path in &chain {
        assert!(
            sid_aces(path, &sid).is_empty(),
            "partial acquisition must roll back"
        );
    }
}

#[test]
fn deny_and_inherit_only_aces_do_not_count_as_effective_access() {
    let tree = TempChain::new();
    let path = &tree.chain()[0];
    let (_, sid_text) = tree.profile();
    let sid = LocalSid::parse(&sid_text).unwrap();
    add_ace(
        path,
        ALL_APP_PACKAGES_SID,
        ANCESTOR_ACCESS,
        INHERIT_ONLY_ACE | CONTAINER_INHERIT_ACE,
        ACCESS_ALLOWED_ACE_TYPE,
    );
    assert!(!allows_ancestor(&DaclSnapshot::read(path).unwrap(), &sid).unwrap());
    add_ace(path, &sid_text, ANCESTOR_ACCESS, 0, ACCESS_DENIED_ACE_TYPE);
    add_ace(
        path,
        ALL_APP_PACKAGES_SID,
        ANCESTOR_ACCESS,
        0,
        ACCESS_ALLOWED_ACE_TYPE,
    );
    assert!(!allows_ancestor(&DaclSnapshot::read(path).unwrap(), &sid).unwrap());
    assert!(matches!(
        AncestorLease::acquire_chain(std::slice::from_ref(path), &sid_text, &[], &write_dacl),
        Err(SandboxError::AncestorAccess { win32_error: 5, .. })
    ));
    assert_eq!(
        sid_aces(path, &sid_text).len(),
        1,
        "only original deny remains"
    );
}

#[test]
fn junction_chain_resolves_physical_ancestors_and_skips_drive_root() {
    let tree = TempChain::new();
    let link = tree.root.join("alias");
    create_junction(&link, &plain_path(&tree.workspace)).unwrap();
    let physical = ancestor_chain(&tree.workspace).unwrap();
    assert_eq!(ancestor_chain(&link).unwrap(), physical);
    assert!(
        physical.iter().all(|p| p.parent().is_some()),
        "volume root is never leased"
    );
    assert!(!physical.contains(&link));
    std::fs::remove_dir(link).unwrap();
}
