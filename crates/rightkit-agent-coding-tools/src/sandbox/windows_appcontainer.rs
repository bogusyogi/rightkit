// RIGHTKIT-SERIOUS: the Windows AppContainer backend stays local (report T-3; form in docs/rightkit-exceptions/outside-M-rows.md,
// re-checked against rightkit-sandbox 0.6.1). 0.6.1 `Sandbox::spawn_pty` returns `SandboxError::Unsupported("sandboxed PTY is
// implemented for macOS only")` off macOS (spawn.rs), and `AppContainerProfile` exposes no raw CreateProcessW with a caller
// attribute list, so `managed_pty_windows.rs` cannot be confined through RightKit. The profile, lease and pinned-root
// registries here serve that path, so the async piped spawn built on them stays with it. No DENY ACE is written here: it is
// inert against a lowbox token (RightKit #887), so a grant that contains a protected secret is refused instead.
// ---------------------------------------------------------------------------
// Windows AppContainer — the confinement boundary drivers previously lacked
// ---------------------------------------------------------------------------
//
// A Job Object bounds how much a process may consume (process count, kill on
// close). It does not bound what a process may reach: under a Job Object alone a
// driver reads and writes anything the user can. Linux gets Landlock, macOS gets
// Seatbelt, and Windows got nothing comparable until this module.
//
// The boundary here is an AppContainer (lowbox token), which is deny-by-default
// for files and registry. Access is granted by ACLing specific directories to
// the container SID, and network is granted by capability rather than filtered
// after the fact — withholding `internetClient` means no egress at all.
//
// Unlike macOS, Windows has no in-process equivalent of `sandbox_init`: an
// AppContainer can only be established at process creation, via a
// `SECURITY_CAPABILITIES` entry in a `STARTUPINFOEX` attribute list. That is why
// this path uses `CreateProcessW` directly rather than `std::process::Command`,
// which cannot pass process-thread attributes.
//
// The Job Object remains in use alongside this; the two are complementary.

#[cfg(windows)]
mod imp {
    use std::ffi::{c_void, OsStr};
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};

    use rightkit_sandbox::{
        sweep_stale_appcontainer_ancestor_aces_for_owner, AncestorLeasePolicy,
        AppContainerAncestorLease,
    };
    use windows::core::{HRESULT, PCWSTR, PWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, HANDLE, HLOCAL, INVALID_HANDLE_VALUE,
    };
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::Isolation::{
        CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
    };
    use windows::Win32::Security::{FreeSid, PSID, SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES};
    use windows::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList,
        ResumeThread, UpdateProcThreadAttribute, CREATE_NO_WINDOW, CREATE_SUSPENDED,
        CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST,
        PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTF_USESTDHANDLES, STARTUPINFOEXW,
        STARTUPINFOW_FLAGS,
    };

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLogicalDrives() -> u32;
    }

    #[link(name = "userenv")]
    unsafe extern "system" {
        fn GetAppContainerFolderPath(app_container_sid: PCWSTR, folder_path: *mut PWSTR) -> i32;
    }

    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoTaskMemFree(value: *const c_void);
    }

    /// Append one argument using the CommandLineToArgvW quoting rules.
    ///
    /// Quoting every argument unconditionally is wrong: `cmd.exe` reads a quoted
    /// `"/c"` as a file name rather than a switch, so the child starts and then
    /// immediately fails. Quote only when the argument is empty or contains
    /// whitespace or a quote, and double any backslashes that precede a quote.
    fn append_quoted_arg(out: &mut String, arg: &str) {
        let needs_quotes = arg.is_empty() || arg.contains([' ', '\t', '"']);
        if !needs_quotes {
            out.push_str(arg);
            return;
        }
        out.push('"');
        let mut backslashes = 0usize;
        for c in arg.chars() {
            match c {
                '\\' => {
                    backslashes += 1;
                    out.push('\\');
                }
                '"' => {
                    // Escape the quote and every backslash that runs into it.
                    for _ in 0..=backslashes {
                        out.push('\\');
                    }
                    backslashes = 0;
                    out.push('"');
                }
                _ => {
                    backslashes = 0;
                    out.push(c);
                }
            }
        }
        // Trailing backslashes would otherwise escape the closing quote.
        for _ in 0..backslashes {
            out.push('\\');
        }
        out.push('"');
    }

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Copy the user's global git config into the container profile folder.
    /// Best effort: a missing or unreadable source leaves git config-less.
    fn mirror_global_git_config(profile_folder: &Path) {
        let source = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|home| PathBuf::from(home).join(".gitconfig"));
        let target = profile_folder.join(".gitconfig");
        match source.and_then(|source| std::fs::read(source).ok()) {
            Some(bytes) => {
                if std::fs::read(&target).ok().as_deref() != Some(bytes.as_slice()) {
                    if let Err(error) = std::fs::write(&target, bytes) {
                        tracing::warn!(path = %target.display(), %error, "sandbox: could not mirror global git config");
                    }
                }
            }
            None => {
                let _ = std::fs::remove_file(&target);
            }
        }
    }

    fn confined_environment(
        cwd: &Path,
        profile_folder: &Path,
        extra_env: &[(String, String)],
        git_ceilings: &[PathBuf],
    ) -> Vec<u16> {
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let system_drive = Path::new(&system_root)
            .components()
            .next()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_else(|| "C:".into());
        let cwd_drive = cwd
            .components()
            .next()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .filter(|drive| drive.ends_with(':'))
            .unwrap_or_else(|| system_drive.clone());
        let system32 = Path::new(&system_root).join("System32");
        let cwd = cwd.display().to_string();
        let mut entries = Vec::new();
        // CreateProcess requires hidden per-drive current-directory entries
        // when a custom environment block is supplied. Preserve every logical
        // drive, using requested cwd for its drive and a root for others.
        let logical_drives = unsafe { GetLogicalDrives() };
        for index in 0..26 {
            if logical_drives & (1 << index) == 0 {
                continue;
            }
            let drive = format!("{}:", (b'A' + index as u8) as char);
            let value = if drive.eq_ignore_ascii_case(&cwd_drive) {
                cwd.clone()
            } else {
                format!("{drive}\\")
            };
            entries.push(format!("={drive}={value}"));
        }
        if !entries
            .iter()
            .any(|entry| entry.starts_with(&format!("={cwd_drive}=")))
        {
            entries.push(format!("={cwd_drive}={cwd}"));
        }
        entries.extend([
            format!("ComSpec={}\\cmd.exe", system32.display()),
            format!("Path={}", confined_path(&system32, &system_root)),
            format!("SystemDrive={system_drive}"),
            format!("SystemRoot={system_root}"),
            format!("LOCALAPPDATA={}", profile_folder.display()),
            format!("TEMP={}", profile_folder.join("Temp").display()),
            format!("TMP={}", profile_folder.join("Temp").display()),
            format!("windir={system_root}"),
        ]);
        // Git reads `$HOME/.gitconfig`, falling back to USERPROFILE. A traverse
        // lease on the user profile lets the confined child see that file but
        // never open it, and git then aborts every command. Point HOME at the
        // container's own profile folder and mirror the user's global config
        // there so identity and aliases still apply.
        if !extra_env
            .iter()
            .any(|(key, _)| key.eq_ignore_ascii_case("HOME"))
        {
            mirror_global_git_config(profile_folder);
            entries.push(format!("HOME={}", profile_folder.display()));
        }
        // Non-secret machine/user facts that command resolution and common
        // tooling depend on. PATHEXT is required for PowerShell/cmd to resolve
        // `git` to `git.exe`; secrets remain scrubbed because only this
        // allowlist (never the full parent environment) is forwarded.
        for key in [
            "USERPROFILE",
            "APPDATA",
            "HOMEDRIVE",
            "HOMEPATH",
            "PATHEXT",
            "PSModulePath",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramW6432",
            "CommonProgramFiles",
            "CommonProgramFiles(x86)",
            "CommonProgramW6432",
            "ProgramData",
            "PUBLIC",
            "ALLUSERSPROFILE",
            "USERNAME",
            "USERDOMAIN",
            "COMPUTERNAME",
            "OS",
            "NUMBER_OF_PROCESSORS",
            "PROCESSOR_ARCHITECTURE",
        ] {
            if let Some(value) = std::env::var_os(key) {
                entries.push(format!("{key}={}", value.to_string_lossy()));
            }
        }
        // Node resolves every module through realpath, which on Windows lstat()s
        // the drive root; AppContainer tokens cannot read `C:\` attributes, so
        // any confined Node child (plugins, MCP servers, hooks) dies with EPERM.
        // Keep paths as given instead. Caveat: symlinked node_modules layouts
        // (pnpm) resolve by link path under this flag.
        const NODE_SANDBOX_FLAGS: &str = "--preserve-symlinks --preserve-symlinks-main";
        let caller_node_options = extra_env
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("NODE_OPTIONS"))
            .map(|(_, value)| value.clone());
        entries.push(match caller_node_options {
            Some(value) => format!("NODE_OPTIONS={value} {NODE_SANDBOX_FLAGS}"),
            None => format!("NODE_OPTIONS={NODE_SANDBOX_FLAGS}"),
        });
        // Git stops discovery at each ceiling without statting it, so a
        // workspace under an ancestor RightKit could not lease never reaches
        // that inaccessible directory. Caller-supplied ceilings are kept.
        let caller_git_ceilings = extra_env
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("GIT_CEILING_DIRECTORIES"))
            .map(|(_, value)| value.clone());
        let ceilings: Vec<String> = git_ceilings
            .iter()
            .map(|path| path.display().to_string())
            .chain(caller_git_ceilings)
            .filter(|value| !value.is_empty())
            .collect();
        if !ceilings.is_empty() {
            // Git on Windows separates GIT_CEILING_DIRECTORIES entries with ';'.
            // The leading empty entry tells Git not to resolve the entries that
            // follow, which would otherwise stat the unleasable ancestors.
            entries.push(format!("GIT_CEILING_DIRECTORIES=;{}", ceilings.join(";")));
        }
        entries.extend(
            extra_env
                .iter()
                .filter(|(key, _)| {
                    !key.eq_ignore_ascii_case("NODE_OPTIONS")
                        && !key.eq_ignore_ascii_case("GIT_CEILING_DIRECTORIES")
                        && !key.eq_ignore_ascii_case(super::SANDBOX_READ_ROOTS_ENV)
                })
                .map(|(key, value)| format!("{key}={value}")),
        );
        entries.sort_by_key(|entry| entry.to_ascii_lowercase());

        let mut block = Vec::new();
        for entry in entries {
            block.extend(OsStr::new(&entry).encode_wide());
            block.push(0);
        }
        block.push(0);
        block
    }

    /// Confined `Path`: the system directories first (so they cannot be
    /// shadowed), then the caller's effective PATH so user-installed tools such
    /// as `git` and `ping` resolve exactly as in the user's own shell.
    fn confined_path(system32: &Path, system_root: &str) -> String {
        confined_path_from(system32, system_root, std::env::var_os("PATH"))
    }

    /// Pure core of [`confined_path`]: PATH entries only name directories, so
    /// forwarding them widens no filesystem grant (ACL leases are separate).
    fn confined_path_from(
        system32: &Path,
        system_root: &str,
        inherited: Option<std::ffi::OsString>,
    ) -> String {
        let mut dirs: Vec<String> = vec![
            system32.display().to_string(),
            system_root.to_string(),
            system32.join("Wbem").display().to_string(),
            system32
                .join("WindowsPowerShell")
                .join("v1.0")
                .display()
                .to_string(),
        ];
        if let Some(inherited) = inherited {
            for dir in std::env::split_paths(&inherited) {
                let dir = dir.display().to_string();
                if dir.is_empty() || dirs.iter().any(|d| d.eq_ignore_ascii_case(&dir)) {
                    continue;
                }
                dirs.push(dir);
            }
        }
        dirs.join(";")
    }

    /// An AppContainer profile plus its SID, which together form the confinement
    /// boundary for one driver.
    pub struct AppContainerProfile {
        name: String,
        sid: PSID,
        /// Canonical workspace root this profile was prepared for. Empty until
        /// `for_workspace` completes; requested working directories are
        /// validated against it before translation onto `launch_dir`.
        workspace_root: PathBuf,
        /// The workspace alias a confined child must start beneath. Empty
        /// means the caller-supplied cwd; otherwise a junction whose ancestors
        /// the container token can read (see `ensure_workspace_alias`).
        launch_dir: PathBuf,
        /// Whether launches from this profile carry the `internetClient`
        /// capability (outbound network). Off unless a caller opts in per
        /// command; the profile SID itself never changes.
        network: bool,
        /// ACL grants made for this profile's commands. Revoked when the
        /// profile and every process spawned from it are gone.
        leases: std::sync::Arc<LeaseSet>,
    }

    const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
    const SE_FILE_OBJECT: u32 = 1;
    const GRANT_ACCESS: i32 = 1;
    const REVOKE_ACCESS: i32 = 4;
    /// `SE_GROUP_ENABLED` — attribute for a capability SID in the token.
    const SE_GROUP_ENABLED: u32 = 0x0000_0004;
    /// Well-known `internetClient` capability SID: outbound network for an
    /// AppContainer (no inbound, no private-network/LAN servers).
    const INTERNET_CLIENT_CAPABILITY_SID: &str = "S-1-15-3-1";
    const NO_MULTIPLE_TRUSTEE: i32 = 0;
    const TRUSTEE_IS_SID: i32 = 0;
    const TRUSTEE_IS_UNKNOWN: i32 = 0;
    const SUB_CONTAINERS_AND_OBJECTS_INHERIT: u32 = 0x3;
    const FILE_GENERIC_READ: u32 = 0x0012_0089;
    const FILE_GENERIC_WRITE: u32 = 0x0012_0116;
    const FILE_GENERIC_EXECUTE: u32 = 0x0012_00a0;
    const DELETE: u32 = 0x0001_0000;
    const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
    const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const GENERIC_WRITE_ACCESS: u32 = 0x4000_0000;
    const OPEN_EXISTING_CREATION: u32 = 3;
    const ACL_REVISION: u8 = 2;
    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0x0;
    const OBJECT_INHERIT_ACE: u8 = 0x1;
    const CONTAINER_INHERIT_ACE: u8 = 0x2;
    const NO_PROPAGATE_INHERIT_ACE: u8 = 0x4;
    const INHERIT_ONLY_ACE: u8 = 0x8;

    #[repr(C)]
    struct TrusteeW {
        multiple_trustee: *mut TrusteeW,
        multiple_trustee_operation: i32,
        trustee_form: i32,
        trustee_type: i32,
        name: *mut u16,
    }

    #[repr(C)]
    struct ExplicitAccessW {
        access_permissions: u32,
        access_mode: i32,
        inheritance: u32,
        trustee: TrusteeW,
    }

    /// Win32 `ACL` header — the descriptor-borrowed DACL layout walked when
    /// deciding whether a grant is already present.
    #[repr(C)]
    struct Acl {
        acl_revision: u8,
        _sbz1: u8,
        acl_size: u16,
        ace_count: u16,
        _sbz2: u16,
    }

    /// Win32 `ACCESS_ALLOWED_ACE`; `sid_start` is the first DWORD of the
    /// variable-length SID stored inline at the ACE tail.
    #[repr(C)]
    struct AccessAllowedAce {
        header: AceHeader,
        mask: u32,
        sid_start: u32,
    }

    /// Win32 `ACE_HEADER` — the leading bytes every ACE in an ACL shares.
    #[repr(C)]
    struct AceHeader {
        ace_type: u8,
        ace_flags: u8,
        ace_size: u16,
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn GetAce(acl: *mut c_void, index: u32, ace: *mut *mut c_void) -> i32;
        fn EqualSid(sid1: *mut c_void, sid2: *mut c_void) -> i32;
        fn GetNamedSecurityInfoW(
            path: *mut u16,
            object_type: u32,
            information: u32,
            owner: *mut *mut c_void,
            group: *mut *mut c_void,
            dacl: *mut *mut c_void,
            sacl: *mut *mut c_void,
            descriptor: *mut *mut c_void,
        ) -> u32;
        fn SetEntriesInAclW(
            count: u32,
            entries: *mut ExplicitAccessW,
            old_acl: *mut c_void,
            new_acl: *mut *mut c_void,
        ) -> u32;
        fn ConvertStringSidToSidW(string_sid: *const u16, sid: *mut PSID) -> i32;
        fn InitializeSecurityDescriptor(descriptor: *mut c_void, revision: u32) -> i32;
        fn SetSecurityDescriptorDacl(
            descriptor: *mut c_void,
            present: i32,
            dacl: *mut c_void,
            defaulted: i32,
        ) -> i32;
        fn SetFileSecurityW(path: *const u16, information: u32, descriptor: *mut c_void) -> i32;
        fn SetNamedSecurityInfoW(
            path: *mut u16,
            object_type: u32,
            information: u32,
            owner: *mut c_void,
            group: *mut c_void,
            dacl: *mut c_void,
            sacl: *mut c_void,
        ) -> u32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
        fn CreateFileW(
            name: *const u16,
            desired_access: u32,
            share_mode: u32,
            security: *const c_void,
            creation_disposition: u32,
            flags_and_attributes: u32,
            template: *mut c_void,
        ) -> *mut c_void;
        fn DeviceIoControl(
            device: *mut c_void,
            control_code: u32,
            in_buffer: *const c_void,
            in_buffer_size: u32,
            out_buffer: *mut c_void,
            out_buffer_size: u32,
            bytes_returned: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
    }

    /// `SetNamedSecurityInfoW` performs a read-modify-write of the DACL, so
    /// concurrent grants in this process must serialize or they silently
    /// lose each other's ACEs.
    static ACL_GRANT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Noninheriting grants must update only the object itself.
    /// SetNamedSecurityInfoW propagates the directory's inherited ACLs through
    /// existing descendants even for a non-inheriting ACE. On a repository or
    /// drive ancestor that walks unrelated trees before the first Git/provider
    /// call, and repeats on revocation. SetFileSecurityW does not propagate.
    /// Inheriting workspace/image grants still use the propagating API.
    unsafe fn write_path_dacl(path: *mut u16, dacl: *mut c_void, inheritance: u32) -> u32 {
        if inheritance != 0 {
            return unsafe {
                SetNamedSecurityInfoW(
                    path,
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    dacl,
                    std::ptr::null_mut(),
                )
            };
        }
        // SECURITY_DESCRIPTOR is pointer-aligned absolute descriptor storage;
        // InitializeSecurityDescriptor initializes its revision/control fields.
        let mut descriptor = windows::Win32::Security::SECURITY_DESCRIPTOR::default();
        let descriptor = std::ptr::from_mut(&mut descriptor).cast::<c_void>();
        // SAFETY: descriptor owns correctly sized/aligned storage; path is
        // NUL-terminated & dacl stays allocated through the synchronous write.
        unsafe {
            if InitializeSecurityDescriptor(descriptor, 1) == 0
                || SetSecurityDescriptorDacl(descriptor, 1, dacl, 0) == 0
                || SetFileSecurityW(path, DACL_SECURITY_INFORMATION, descriptor) == 0
            {
                return windows::Win32::Foundation::GetLastError().0;
            }
        }
        0
    }

    /// The outcome of one `apply_sid_path_grant` call. Callers ignore the
    /// payload; returning it lets tests prove whether the DACL was rewritten
    /// without a process-global counter that would race across parallel
    /// tests.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum GrantWrite {
        /// The request was merged and `SetNamedSecurityInfoW` rewrote the
        /// object's DACL.
        Written,
        /// The live DACL already covered the request — no merge ran and no
        /// tree-wide inheritance propagation was paid for.
        Skipped,
    }

    /// `S-1-15-2-1` — ALL APPLICATION PACKAGES. Every lowbox token satisfies
    /// this SID, so one grant covers every workspace container while the
    /// leaf workspace DACL stays the access boundary. A single well-known
    /// ACE also keeps concurrent preparations across processes merging the
    /// same ACE instead of losing each other's per-container ACEs.
    fn app_packages_sid() -> std::io::Result<PSID> {
        let wide: Vec<u16> = "S-1-15-2-1".encode_utf16().chain(Some(0)).collect();
        let mut sid = PSID(std::ptr::null_mut());
        // SAFETY: `wide` is NUL-terminated and `sid` is a valid out-pointer;
        // a successful result is a LocalAlloc buffer the caller frees.
        if unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut sid) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(sid)
    }

    /// Whether an allow ACE's flag/mask pair already grants everything a
    /// request for `permissions` with `inheritance` would produce.
    ///
    /// The ACE must reach the object itself (no `INHERIT_ONLY`), hold every
    /// requested permission bit, and propagate at least as deep as
    /// requested — a `NO_PROPAGATE` ACE stops at immediate children and can
    /// never cover an unbounded propagation request. Anything narrower still
    /// takes the merge path so a grant is never silently weakened.
    fn ace_covers_grant(ace_flags: u8, ace_mask: u32, permissions: u32, inheritance: u32) -> bool {
        if ace_flags & INHERIT_ONLY_ACE != 0 {
            return false;
        }
        if ace_mask & permissions != permissions {
            return false;
        }
        let scope = OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE;
        let requested_scope = (inheritance as u8) & scope;
        if ace_flags & scope & requested_scope != requested_scope {
            return false;
        }
        requested_scope == 0
            || (inheritance as u8) & NO_PROPAGATE_INHERIT_ACE != 0
            || ace_flags & NO_PROPAGATE_INHERIT_ACE == 0
    }

    /// Scan `dacl` — borrowed from a live `GetNamedSecurityInfoW` security
    /// descriptor — for an allow ACE that already covers a grant of
    /// `permissions`/`inheritance` to `sid`.
    ///
    /// SAFETY: `dacl` must be null or point at a valid DACL inside a
    /// security descriptor that outlives the call; `sid` must be a valid
    /// SID.
    unsafe fn dacl_covers_grant(
        dacl: *mut c_void,
        sid: PSID,
        permissions: u32,
        inheritance: u32,
    ) -> bool {
        if dacl.is_null() {
            return false;
        }
        // SAFETY: dacl points at the ACL inside the caller's live
        // descriptor; only the fixed header is read here.
        let (revision, acl_size, ace_count) = unsafe {
            let acl = dacl as *const Acl;
            ((*acl).acl_revision, (*acl).acl_size, (*acl).ace_count)
        };
        // Only the standard ACL layout is walked — anything else falls back
        // to the merge path.
        if revision != ACL_REVISION {
            return false;
        }
        for index in 0..u32::from(ace_count) {
            let mut ace: *mut c_void = std::ptr::null_mut();
            // SAFETY: index stays below AceCount and ace is a valid
            // out-pointer; the returned pointer borrows from the ACL.
            if unsafe { GetAce(dacl, index, &mut ace) } == 0 || ace.is_null() {
                continue;
            }
            // The ACE must lie inside the ACL before its bytes are trusted.
            let offset = (ace as usize).wrapping_sub(dacl as usize);
            if offset >= usize::from(acl_size) {
                continue;
            }
            // SAFETY: ace is DWORD-aligned inside the kernel-produced DACL,
            // which stores every ACE contiguously, so the fixed
            // ACCESS_ALLOWED_ACE prefix is readable before its fields are
            // trusted.
            let allowed = unsafe { &*(ace as *const AccessAllowedAce) };
            if allowed.header.ace_type != ACCESS_ALLOWED_ACE_TYPE
                || usize::from(allowed.header.ace_size) < std::mem::size_of::<AccessAllowedAce>()
                || offset + usize::from(allowed.header.ace_size) > usize::from(acl_size)
            {
                continue;
            }
            if !ace_covers_grant(
                allowed.header.ace_flags,
                allowed.mask,
                permissions,
                inheritance,
            ) {
                continue;
            }
            // SAFETY: the ACE is ACCESS_ALLOWED and spans at least the
            // inline SID start inside the ACL; `sid` is a live SID. Both
            // are valid EqualSid operands.
            if unsafe { EqualSid(&allowed.sid_start as *const u32 as *mut c_void, sid.0) } != 0 {
                return true;
            }
        }
        false
    }

    /// Merge one allow ACE for `sid` into `path`'s DACL — unless the live
    /// DACL already covers the request, in which case nothing is written.
    ///
    /// The skip matters on the confined-spawn hot path: `for_workspace` and
    /// `prepare_launch` re-grant the same roots for every launch, and an
    /// unconditional `SetNamedSecurityInfoW` propagates the descriptor
    /// across the entire workspace and toolchain trees each time. The
    /// descriptor is re-read on every call — never cached — so a skip only
    /// ever reflects current kernel state, and any request the stored ACEs
    /// do not fully cover still merges and propagates exactly as before.
    fn apply_sid_path_grant(
        path: &Path,
        sid: PSID,
        permissions: u32,
        inheritance: u32,
    ) -> std::io::Result<GrantWrite> {
        let _grant_guard = ACL_GRANT_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut wide_path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe {
            let mut old_dacl = std::ptr::null_mut();
            let mut descriptor = std::ptr::null_mut();
            let get_result = GetNamedSecurityInfoW(
                wide_path.as_mut_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut old_dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            );
            if get_result != 0 {
                return Err(std::io::Error::from_raw_os_error(get_result as i32));
            }

            if dacl_covers_grant(old_dacl, sid, permissions, inheritance) {
                let _ = LocalFree(descriptor);
                return Ok(GrantWrite::Skipped);
            }

            let mut entry = ExplicitAccessW {
                access_permissions: permissions,
                access_mode: GRANT_ACCESS,
                inheritance,
                trustee: TrusteeW {
                    multiple_trustee: std::ptr::null_mut(),
                    multiple_trustee_operation: NO_MULTIPLE_TRUSTEE,
                    trustee_form: TRUSTEE_IS_SID,
                    trustee_type: TRUSTEE_IS_UNKNOWN,
                    name: sid.0.cast(),
                },
            };
            let mut new_dacl = std::ptr::null_mut();
            let merge_result = SetEntriesInAclW(1, &mut entry, old_dacl, &mut new_dacl);
            if merge_result != 0 {
                let _ = LocalFree(descriptor);
                return Err(std::io::Error::from_raw_os_error(merge_result as i32));
            }

            let set_result = write_path_dacl(wide_path.as_mut_ptr(), new_dacl, inheritance);
            let _ = LocalFree(new_dacl);
            let _ = LocalFree(descriptor);
            if set_result != 0 {
                return Err(std::io::Error::from_raw_os_error(set_result as i32));
            }
        }
        Ok(GrantWrite::Written)
    }

    /// Prefix an I/O error with the failing step, preserving its kind and
    /// raw OS code text so launch refusals name the exact operation.
    fn step_error(step: impl std::fmt::Display, error: std::io::Error) -> std::io::Error {
        std::io::Error::new(error.kind(), format!("{step}: {error}"))
    }

    /// Strip the `\\?\` verbatim prefix `canonicalize` adds; ACL APIs and
    /// junction targets want plain drive-letter spellings.
    fn plain_path(path: &Path) -> PathBuf {
        let text = path.to_string_lossy();
        match text.strip_prefix("\\\\?\\") {
            Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
            _ => path.to_path_buf(),
        }
    }

    /// Whether `path` already carries an ACE for `sid` that grants write
    /// access — a stale over-broad grant (for example a leftover Modify ACE)
    /// that a read-only request must not inherit.
    fn path_has_write_grant(path: &Path, sid: PSID) -> bool {
        path_dacl_grants(path, sid, FILE_GENERIC_WRITE)
    }

    /// Whether ALL APPLICATION PACKAGES already holds read/execute on `path`
    /// itself, so every AppContainer can load an image from it without a
    /// per-profile lease (Program Files, for example, whose DACL the user
    /// cannot change).
    fn app_packages_can_read_execute(path: &Path) -> bool {
        let Ok(sid) = app_packages_sid() else {
            return false;
        };
        let covered = path_dacl_grants(path, sid, FILE_GENERIC_READ | FILE_GENERIC_EXECUTE);
        // SAFETY: app_packages_sid returns a LocalAlloc SID owned here.
        unsafe { LocalFree(sid.0) };
        covered
    }

    fn path_dacl_grants(path: &Path, sid: PSID, permissions: u32) -> bool {
        let mut wide_path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: wide_path is NUL-terminated; out-pointers are valid; the
        // descriptor is freed after the borrowed DACL is inspected.
        unsafe {
            let mut dacl = std::ptr::null_mut();
            let mut descriptor = std::ptr::null_mut();
            if GetNamedSecurityInfoW(
                wide_path.as_mut_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            ) != 0
            {
                return false;
            }
            let covered = dacl_covers_grant(dacl, sid, permissions, 0);
            let _ = LocalFree(descriptor);
            covered
        }
    }

    fn grant_sid_path_access(path: &Path, sid: PSID, writable: bool) -> std::io::Result<()> {
        let permissions = if writable {
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE
        } else {
            FILE_GENERIC_READ | FILE_GENERIC_EXECUTE
        };
        let inheritance = if path.is_dir() {
            SUB_CONTAINERS_AND_OBJECTS_INHERIT
        } else {
            0
        };
        apply_sid_path_grant(path, sid, permissions, inheritance).map(|_| ())
    }

    /// Parse a string SID. The caller frees it with `LocalFree`.
    fn sid_from_string(text: &str) -> std::io::Result<PSID> {
        let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let mut sid = PSID(std::ptr::null_mut());
        // SAFETY: `wide` is NUL-terminated and `sid` is a valid out-pointer;
        // success yields a LocalAlloc buffer the caller frees.
        if unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut sid) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(sid)
    }

    /// Owns SIDs allocated by `ConvertStringSidToSidW` for the duration of a
    /// launch (capability SIDs referenced by the attribute list).
    struct OwnedSids(Vec<PSID>);

    impl Drop for OwnedSids {
        fn drop(&mut self) {
            for sid in self.0.drain(..) {
                // SAFETY: each SID came from ConvertStringSidToSidW and is
                // freed exactly once here.
                unsafe { LocalFree(sid.0) };
            }
        }
    }

    /// Token capabilities for one launch; owns the capability SIDs the
    /// `SECURITY_CAPABILITIES` structure points into. The heap buffers do not
    /// move when the set does, so the embedded pointers stay valid for its
    /// lifetime.
    pub(crate) struct CapabilitySet {
        _sids: OwnedSids,
        _attrs: Vec<SID_AND_ATTRIBUTES>,
        capabilities: SECURITY_CAPABILITIES,
    }

    impl CapabilitySet {
        /// A fresh `SECURITY_CAPABILITIES` view valid while `self` lives.
        pub(crate) fn security_capabilities(&self) -> SECURITY_CAPABILITIES {
            SECURITY_CAPABILITIES {
                AppContainerSid: self.capabilities.AppContainerSid,
                Capabilities: self.capabilities.Capabilities,
                CapabilityCount: self.capabilities.CapabilityCount,
                Reserved: 0,
            }
        }
    }

    /// Whether the directory tree under `root` holds at most `budget`
    /// entries. Bounded walk; reparse points are not followed.
    fn tree_within_budget(root: &Path, budget: usize) -> bool {
        let mut stack = vec![root.to_path_buf()];
        let mut seen = 0usize;
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                seen += 1;
                if seen > budget {
                    return false;
                }
                if let Ok(kind) = entry.file_type() {
                    if kind.is_dir() && !kind.is_symlink() {
                        stack.push(entry.path());
                    }
                }
            }
        }
        true
    }

    /// Merge one explicit entry (`REVOKE_ACCESS`) into
    /// `path`'s DACL. Unlike [`apply_sid_path_grant`] there is no coverage
    /// skip: a revoke must always rewrite so inherited copies are removed
    /// from the whole tree.
    fn apply_sid_path_entry(
        path: &Path,
        sid: PSID,
        access_mode: i32,
        permissions: u32,
        inheritance: u32,
    ) -> std::io::Result<()> {
        let _grant_guard = ACL_GRANT_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut wide_path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe {
            let mut old_dacl = std::ptr::null_mut();
            let mut descriptor = std::ptr::null_mut();
            let get_result = GetNamedSecurityInfoW(
                wide_path.as_mut_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut old_dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            );
            if get_result != 0 {
                return Err(std::io::Error::from_raw_os_error(get_result as i32));
            }
            let mut entry = ExplicitAccessW {
                access_permissions: permissions,
                access_mode,
                inheritance,
                trustee: TrusteeW {
                    multiple_trustee: std::ptr::null_mut(),
                    multiple_trustee_operation: NO_MULTIPLE_TRUSTEE,
                    trustee_form: TRUSTEE_IS_SID,
                    trustee_type: TRUSTEE_IS_UNKNOWN,
                    name: sid.0.cast(),
                },
            };
            let mut new_dacl = std::ptr::null_mut();
            let merge_result = SetEntriesInAclW(1, &mut entry, old_dacl, &mut new_dacl);
            if merge_result != 0 {
                let _ = LocalFree(descriptor);
                return Err(std::io::Error::from_raw_os_error(merge_result as i32));
            }
            let set_result = write_path_dacl(wide_path.as_mut_ptr(), new_dacl, inheritance);
            let _ = LocalFree(new_dacl);
            let _ = LocalFree(descriptor);
            if set_result != 0 {
                return Err(std::io::Error::from_raw_os_error(set_result as i32));
            }
        }
        Ok(())
    }

    /// Process-wide reference counts of live ACL grants. One grant may back
    /// several concurrent commands (same workspace); the ACE is revoked only
    /// when the last command using it ends.
    struct LeaseEntry {
        count: usize,
        writable: bool,
    }

    type LeaseMap = std::collections::HashMap<String, LeaseEntry>;

    fn lease_registry() -> &'static std::sync::Mutex<LeaseMap> {
        static REGISTRY: std::sync::OnceLock<std::sync::Mutex<LeaseMap>> =
            std::sync::OnceLock::new();
        REGISTRY.get_or_init(Default::default)
    }

    /// One per-command allow ACL entry for a container SID (a grant on a
    /// workspace/tool folder/approved path). Dropping the last lease on a key
    /// revokes the entry, so grants no longer outlive the command that needed
    /// them. No DENY entry is ever written: a lowbox token is checked in a
    /// second pass that honours only allow entries for its container SID,
    /// capability SIDs and ALL APPLICATION PACKAGES, so a DENY ACE is inert
    /// against an AppContainer process (RightKit #887). A protected secret
    /// inside a grant is refused instead (`refuse_protected_secrets_within`).
    pub(crate) struct GrantLease {
        key: String,
        writable: bool,
        sid_text: String,
        path: PathBuf,
    }

    fn lease_key(sid_text: &str, path: &Path) -> String {
        format!("allow|{sid_text}|{}", lease_path_key(path))
    }

    /// Fail closed when `granted_root` is, or contains, a protected secret
    /// location (`.ssh`, cloud credentials, browser profiles, ...). Called
    /// before any ACE is written for the root.
    fn refuse_protected_secrets_within(granted_root: &Path) -> std::io::Result<()> {
        let protected = crate::sandbox::secret_paths_within(granted_root);
        if protected.is_empty() && !crate::sandbox::path_is_protected_secret(granted_root) {
            return Ok(());
        }
        let listed = protected
            .iter()
            .take(3)
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} holds a protected location ({listed}) and cannot be granted: a deny entry                  does not bind an AppContainer process, so the grant is refused",
                granted_root.display()
            ),
        ))
    }

    /// Case-folded, separator-normalised lease-registry key (Windows paths).
    fn lease_path_key(path: &Path) -> String {
        path.to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_ascii_lowercase()
    }

    impl GrantLease {
        fn acquire(
            sid: PSID,
            sid_text: &str,
            path: &Path,
            writable: bool,
        ) -> std::io::Result<Self> {
            let key = lease_key(sid_text, path);
            let mut registry = lease_registry()
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match registry.get_mut(&key) {
                Some(entry) => {
                    if writable && !entry.writable {
                        grant_sid_path_access(path, sid, writable)?;
                        entry.writable |= writable;
                    }
                    entry.count += 1;
                }
                None => {
                    // No live lease in this process owns the key, so a
                    // write-capable ACE for this SID is stale (leaked by a
                    // crashed run): remove it before granting the narrower
                    // read/execute access.
                    if !writable && path_has_write_grant(path, sid) {
                        apply_sid_path_entry(
                            path,
                            sid,
                            REVOKE_ACCESS,
                            0,
                            if path.is_dir() {
                                SUB_CONTAINERS_AND_OBJECTS_INHERIT
                            } else {
                                0
                            },
                        )
                        .map_err(|e| {
                            step_error(format!("revoke stale write ACE on {}", path.display()), e)
                        })?;
                    }
                    grant_sid_path_access(path, sid, writable).map_err(|e| {
                        step_error(
                            format!(
                                "grant {} ACE on {}",
                                if writable { "modify" } else { "read/execute" },
                                path.display()
                            ),
                            e,
                        )
                    })?;
                    registry.insert(key.clone(), LeaseEntry { count: 1, writable });
                }
            }
            Ok(Self {
                key,
                writable,
                sid_text: sid_text.to_string(),
                path: path.to_path_buf(),
            })
        }
    }

    impl Drop for GrantLease {
        fn drop(&mut self) {
            let mut registry = lease_registry()
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(entry) = registry.get_mut(&self.key) else {
                return;
            };
            entry.count = entry.count.saturating_sub(1);
            if entry.count > 0 {
                return;
            }
            registry.remove(&self.key);
            let revoked = sid_from_string(&self.sid_text).and_then(|sid| {
                let outcome = apply_sid_path_entry(
                    &self.path,
                    sid,
                    REVOKE_ACCESS,
                    0,
                    if self.path.is_dir() {
                        SUB_CONTAINERS_AND_OBJECTS_INHERIT
                    } else {
                        0
                    },
                );
                // SAFETY: sid came from ConvertStringSidToSidW; freed once.
                unsafe { LocalFree(sid.0) };
                outcome
            });
            if let Err(error) = revoked {
                tracing::warn!(
                    path = %self.path.display(),
                    %error,
                    "sandbox: failed to revoke a per-command ACL grant"
                );
            }
        }
    }

    /// The live leases of one profile; shared with every process spawned from
    /// it so the grants live exactly as long as the longest-running command.
    #[derive(Default)]
    pub(crate) struct LeaseSet {
        items: std::sync::Mutex<Vec<GrantLease>>,
        /// Physical-ancestor traverse leases from RightKit, one per leased path.
        /// Each removes exactly its own ACEs when this set drops.
        ancestors: std::sync::Mutex<Vec<(PathBuf, AppContainerAncestorLease)>>,
        /// Deepest ancestor RightKit could not lease for each leased path, as
        /// exported to sandboxed children via `GIT_CEILING_DIRECTORIES`.
        git_ceilings: std::sync::Mutex<Vec<PathBuf>>,
        /// Loopback exemption for a network-enabled container (see
        /// [`LoopbackLease`]); empty for network-denied launches.
        loopback: std::sync::Mutex<Vec<LoopbackLease>>,
        /// Pinned read-root uses held by this profile's launches; each drop
        /// releases one user of the process-wide pinned lease.
        pins: std::sync::Mutex<Vec<PinUse>>,
    }

    struct PinnedEntry<T> {
        value: T,
        users: usize,
        release_requested: bool,
    }

    /// Process-wide table of shared leases keyed by grant. `acquire` builds
    /// the value once (the expensive ACL application) and later calls reuse
    /// it; the value is dropped (revoking its ACEs) only after a release
    /// request once no launch still uses it.
    pub(crate) struct PinnedTable<T> {
        entries: std::sync::Mutex<std::collections::HashMap<String, PinnedEntry<T>>>,
    }

    impl<T> Default for PinnedTable<T> {
        fn default() -> Self {
            Self {
                entries: Default::default(),
            }
        }
    }

    impl<T> PinnedTable<T> {
        pub(crate) fn acquire<R>(
            &self,
            key: &str,
            make: impl FnOnce() -> std::io::Result<T>,
            view: impl FnOnce(&T) -> R,
        ) -> std::io::Result<R> {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if !entries.contains_key(key) {
                let value = make()?;
                entries.insert(
                    key.to_string(),
                    PinnedEntry {
                        value,
                        users: 0,
                        release_requested: false,
                    },
                );
            }
            let entry = entries.get_mut(key).expect("entry inserted above");
            entry.users += 1;
            entry.release_requested = false;
            Ok(view(&entry.value))
        }

        /// One launch finished with `key`; drops the value if a release was
        /// requested and this was the last user.
        pub(crate) fn release_user(&self, key: &str) {
            let dropped = {
                let mut entries = self
                    .entries
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                let Some(entry) = entries.get_mut(key) else {
                    return;
                };
                entry.users = entry.users.saturating_sub(1);
                if entry.users == 0 && entry.release_requested {
                    entries.remove(key)
                } else {
                    None
                }
            };
            drop(dropped);
        }

        /// Remove every idle entry now and every busy one at its last user.
        pub(crate) fn request_release_all(&self) {
            let dropped: Vec<PinnedEntry<T>> = {
                let mut entries = self
                    .entries
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                let idle: Vec<String> = entries
                    .iter()
                    .filter(|(_, entry)| entry.users == 0)
                    .map(|(key, _)| key.clone())
                    .collect();
                for entry in entries.values_mut() {
                    entry.release_requested = true;
                }
                idle.iter().filter_map(|key| entries.remove(key)).collect()
            };
            drop(dropped);
        }
    }

    fn pinned_read_roots() -> &'static PinnedTable<LeaseSet> {
        static TABLE: std::sync::OnceLock<PinnedTable<LeaseSet>> = std::sync::OnceLock::new();
        TABLE.get_or_init(Default::default)
    }

    /// One launch's use of a pinned read root.
    struct PinUse {
        key: String,
    }

    impl Drop for PinUse {
        fn drop(&mut self) {
            pinned_read_roots().release_user(&self.key);
        }
    }

    /// Revoke the pinned interpreter read-root leases (daemon shutdown or
    /// tests). Idle grants are revoked immediately; grants still used by a
    /// running command are revoked when its last launch ends.
    pub fn release_pinned_read_roots() {
        pinned_read_roots().request_release_all();
    }

    /// Owner prefix of the per-workspace profiles this module creates. Only ACEs
    /// for SIDs derived from this prefix are swept as stale; live leases are kept.
    const WORKSPACE_OWNER_PREFIX: &str = "rightkit-agent.sandbox.workspace.";

    /// Keeps the typed `SandboxError` reachable through `io::Error::get_ref`, so
    /// `AncestorAccess` is never flattened into an untyped message.
    fn ancestor_io_error(error: rightkit_sandbox::SandboxError) -> std::io::Error {
        let kind = match &error {
            rightkit_sandbox::SandboxError::AncestorAccess { .. } => {
                std::io::ErrorKind::PermissionDenied
            }
            _ => std::io::ErrorKind::Other,
        };
        std::io::Error::new(kind, error)
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
        fn GetProcessHeap() -> *mut c_void;
        fn HeapFree(heap: *mut c_void, flags: u32, memory: *mut c_void) -> i32;
    }

    /// `NetworkIsolation*AppContainer*` entry points, resolved at run time so
    /// the crate has no link-time dependency on `FirewallAPI.dll`.
    struct LoopbackApi {
        get: unsafe extern "system" fn(*mut u32, *mut *mut SID_AND_ATTRIBUTES) -> u32,
        set: unsafe extern "system" fn(u32, *mut SID_AND_ATTRIBUTES) -> u32,
    }

    fn loopback_api() -> std::io::Result<&'static LoopbackApi> {
        static API: std::sync::OnceLock<Result<LoopbackApi, String>> = std::sync::OnceLock::new();
        API.get_or_init(|| {
            let name = wide("FirewallAPI.dll");
            // SAFETY: `name` is NUL-terminated; the module stays loaded for
            // the life of the process.
            let module = unsafe { LoadLibraryW(name.as_ptr()) };
            if module.is_null() {
                return Err(format!(
                    "load FirewallAPI.dll: {}",
                    std::io::Error::last_os_error()
                ));
            }
            // SAFETY: `module` is a live module handle and every name is a
            // NUL-terminated ASCII export name.
            let (get, set) = unsafe {
                (
                    GetProcAddress(
                        module,
                        c"NetworkIsolationGetAppContainerConfig"
                            .as_ptr()
                            .cast::<u8>(),
                    ),
                    GetProcAddress(
                        module,
                        c"NetworkIsolationSetAppContainerConfig"
                            .as_ptr()
                            .cast::<u8>(),
                    ),
                )
            };
            if get.is_null() || set.is_null() {
                return Err(
                    "FirewallAPI.dll lacks the NetworkIsolation AppContainer exports".into(),
                );
            }
            // SAFETY: the exports have exactly these signatures (netfw.h).
            Ok(unsafe {
                LoopbackApi {
                    get: std::mem::transmute::<
                        *mut c_void,
                        unsafe extern "system" fn(*mut u32, *mut *mut SID_AND_ATTRIBUTES) -> u32,
                    >(get),
                    set: std::mem::transmute::<
                        *mut c_void,
                        unsafe extern "system" fn(u32, *mut SID_AND_ATTRIBUTES) -> u32,
                    >(set),
                }
            })
        })
        .as_ref()
        .map_err(|message| std::io::Error::other(message.clone()))
    }

    /// Add (`add`) or remove the container from Windows' loopback-exempt list.
    /// Windows blocks loopback for every AppContainer — `internetClient` only
    /// opens the external network — so a network-enabled command that must
    /// reach a local service needs this exemption. The list is replaced
    /// wholesale by the API, so the current list is read, merged, and written
    /// back. Writing it requires elevation; unelevated callers get
    /// `ERROR_ACCESS_DENIED` and the container simply has no loopback.
    fn update_loopback_exemption(sid: PSID, add: bool) -> std::io::Result<()> {
        let api = loopback_api()?;
        let mut count = 0u32;
        let mut list: *mut SID_AND_ATTRIBUTES = std::ptr::null_mut();
        // SAFETY: out-pointers are valid; a success result yields an
        // heap-allocated array released below with HeapFree.
        let status = unsafe { (api.get)(&mut count, &mut list) };
        if status != 0 {
            return Err(step_error(
                "read the loopback exemption list",
                std::io::Error::from_raw_os_error(status as i32),
            ));
        }
        let existing: &[SID_AND_ATTRIBUTES] = if list.is_null() || count == 0 {
            &[]
        } else {
            // SAFETY: the API returned `count` contiguous entries at `list`.
            unsafe { std::slice::from_raw_parts(list, count as usize) }
        };
        let mut merged: Vec<SID_AND_ATTRIBUTES> = Vec::with_capacity(existing.len() + 1);
        let mut present = false;
        for entry in existing {
            // SAFETY: both SIDs are valid for the duration of the call.
            if unsafe { EqualSid(entry.Sid.0, sid.0) } != 0 {
                present = true;
                if !add {
                    continue;
                }
            }
            merged.push(SID_AND_ATTRIBUTES {
                Sid: entry.Sid,
                Attributes: entry.Attributes,
            });
        }
        if add && !present {
            merged.push(SID_AND_ATTRIBUTES {
                Sid: sid,
                Attributes: 0,
            });
        }
        let result = if add == present {
            // Already in the requested state.
            Ok(())
        } else {
            // SAFETY: `merged` holds `len` valid entries whose SIDs outlive
            // this call (API-owned until freed below, or `sid`).
            let status = unsafe { (api.set)(merged.len() as u32, merged.as_mut_ptr()) };
            if status == 0 {
                Ok(())
            } else {
                Err(step_error(
                    if add {
                        "add the container to the loopback exemption list"
                    } else {
                        "remove the container from the loopback exemption list"
                    },
                    std::io::Error::from_raw_os_error(status as i32),
                ))
            }
        };
        if !list.is_null() {
            // NetworkIsolationGetAppContainerConfig allocates the array and
            // every SID on the process heap; the caller frees each with
            // HeapFree. NetworkIsolationFreeAppContainers belongs to the
            // differently shaped Enum result and corrupts the heap here.
            // SAFETY: `list` holds `count` entries from that call; each SID
            // and the array are freed exactly once, after their last use.
            unsafe {
                let heap = GetProcessHeap();
                for entry in std::slice::from_raw_parts(list, count as usize) {
                    if !entry.Sid.0.is_null() {
                        HeapFree(heap, 0, entry.Sid.0);
                    }
                }
                HeapFree(heap, 0, list.cast());
            }
        }
        result
    }

    /// Process-wide reference counts of live loopback exemptions, plus the
    /// containers whose exemption could not be established (not retried: the
    /// failure is environmental, typically a missing elevation).
    #[derive(Default)]
    struct LoopbackRegistry {
        live: std::collections::HashMap<String, usize>,
        failed: std::collections::HashSet<String>,
    }

    fn loopback_registry() -> &'static std::sync::Mutex<LoopbackRegistry> {
        static REGISTRY: std::sync::OnceLock<std::sync::Mutex<LoopbackRegistry>> =
            std::sync::OnceLock::new();
        REGISTRY.get_or_init(Default::default)
    }

    /// A loopback exemption held for the lifetime of a profile's commands and
    /// removed when the last one in the process ends, like every other grant.
    pub(crate) struct LoopbackLease {
        sid_text: String,
    }

    impl LoopbackLease {
        fn acquire(sid: PSID, sid_text: &str) -> std::io::Result<Option<Self>> {
            let mut registry = loopback_registry()
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if registry.failed.contains(sid_text) {
                return Ok(None);
            }
            if let Some(count) = registry.live.get_mut(sid_text) {
                *count += 1;
                return Ok(Some(Self {
                    sid_text: sid_text.to_string(),
                }));
            }
            if let Err(error) = update_loopback_exemption(sid, true) {
                registry.failed.insert(sid_text.to_string());
                return Err(error);
            }
            registry.live.insert(sid_text.to_string(), 1);
            Ok(Some(Self {
                sid_text: sid_text.to_string(),
            }))
        }
    }

    impl Drop for LoopbackLease {
        fn drop(&mut self) {
            let mut registry = loopback_registry()
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(count) = registry.live.get_mut(&self.sid_text) else {
                return;
            };
            *count = count.saturating_sub(1);
            if *count > 0 {
                return;
            }
            registry.live.remove(&self.sid_text);
            let outcome = sid_from_string(&self.sid_text).and_then(|sid| {
                let outcome = update_loopback_exemption(sid, false);
                // SAFETY: sid came from ConvertStringSidToSidW; freed once.
                unsafe { LocalFree(sid.0) };
                outcome
            });
            if let Err(error) = outcome {
                tracing::warn!(
                    "[sandbox] failed to remove the loopback exemption for {}: {error}",
                    self.sid_text
                );
            }
        }
    }

    /// Create a directory junction (mount-point reparse point) at `link`
    /// targeting `target`, without spawning `mklink`. `link` must not exist;
    /// on failure the empty directory is removed again.
    fn create_junction(link: &Path, target: &Path) -> std::io::Result<()> {
        std::fs::create_dir(link)?;
        let outcome = (|| {
            let wide_link = wide(&link.to_string_lossy());
            // SAFETY: opening the empty directory just created above, with
            // OPEN_REPARSE_POINT so the reparse write lands on the link itself.
            let handle = unsafe {
                CreateFileW(
                    wide_link.as_ptr(),
                    GENERIC_WRITE_ACCESS,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING_CREATION,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                    std::ptr::null_mut(),
                )
            };
            if handle.is_null() || handle == INVALID_HANDLE_VALUE.0 {
                return Err(std::io::Error::last_os_error());
            }
            let result = (|| {
                // The substitute name must use the NT namespace spelling.
                let target_text = target.to_string_lossy();
                let substitute: Vec<u16> = format!("\\??\\{target_text}").encode_utf16().collect();
                let print: Vec<u16> = target_text.encode_utf16().collect();
                // REPARSE_DATA_BUFFER for a mount point:
                //   tag u32 | data-len u16 | reserved u16
                //   | subst-offset u16 | subst-len u16
                //   | print-offset u16 | print-len u16 | path buffer
                // The path buffer stores each name NUL-terminated — Windows
                // rejects a buffer whose names are packed without the
                // terminators (ERROR_INVALID_REPARSE_DATA).
                let subst_bytes = substitute.len() * 2 + 2;
                let print_bytes = print.len() * 2 + 2;
                let data_len = (8 + subst_bytes + print_bytes) as u16;
                let mut buffer = Vec::<u8>::with_capacity(8 + usize::from(data_len));
                buffer.extend(IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
                buffer.extend(data_len.to_le_bytes());
                buffer.extend(0u16.to_le_bytes());
                buffer.extend(0u16.to_le_bytes());
                buffer.extend(((substitute.len() * 2) as u16).to_le_bytes());
                buffer.extend((subst_bytes as u16).to_le_bytes());
                buffer.extend(((print.len() * 2) as u16).to_le_bytes());
                for unit in &substitute {
                    buffer.extend(unit.to_le_bytes());
                }
                buffer.extend(0u16.to_le_bytes());
                for unit in &print {
                    buffer.extend(unit.to_le_bytes());
                }
                buffer.extend(0u16.to_le_bytes());
                let mut returned = 0u32;
                // SAFETY: `handle` is a live directory handle and `buffer`
                // holds a well-formed mount-point reparse buffer.
                if unsafe {
                    DeviceIoControl(
                        handle,
                        FSCTL_SET_REPARSE_POINT,
                        buffer.as_ptr().cast(),
                        buffer.len() as u32,
                        std::ptr::null_mut(),
                        0,
                        &mut returned,
                        std::ptr::null_mut(),
                    )
                } == 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            })();
            // SAFETY: handle is owned here and closed exactly once.
            unsafe {
                let _ = CloseHandle(HANDLE(handle));
            }
            result
        })();
        if outcome.is_err() {
            let _ = std::fs::remove_dir(link);
        }
        outcome
    }

    /// Root beneath which per-workspace launch junctions live. `%SystemRoot%`
    /// and `%SystemRoot%\Temp` carry `ALL APPLICATION PACKAGES` ACEs on stock
    /// Windows, so PowerShell's ancestor probe succeeds under this root even
    /// though it fails under `C:\Users`.
    fn workspace_alias_root() -> PathBuf {
        let system_root = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\Windows"));
        system_root.join("Temp").join("rightkit-agent")
    }

    /// The workspace alias a confined child must start beneath to observe the
    /// workspace.
    ///
    /// PowerShell's FileSystem provider normalizes the startup directory by
    /// probing file attributes on every ancestor (the same check Microsoft
    /// documents in PowerShell issue #876). A lowbox token satisfies allow
    /// ACEs only for its container SID, ALL (RESTRICTED) APPLICATION
    /// PACKAGES, and S-1-15-3-* capability SIDs — Everyone and user groups
    /// do not help — and a workspace ancestor such as `C:\Users` carries no
    /// usable ACE while also refusing unelevated ACL changes. When any
    /// ancestor denies the probe, PowerShell silently falls back to the
    /// drive root and every relative command runs against the wrong
    /// directory.
    ///
    /// The workspace itself is already granted, so a directory junction
    /// under `%SystemRoot%\Temp\host app` — a root the token can read and
    /// the caller can write — aliases the workspace with ancestors that are
    /// all covered. The child then lands on the workspace contents while
    /// the workspace DACL keeps full confinement on every real object.
    /// The alias must be established or the launch refused: launching with
    /// the profile-rooted path would reproduce that drive-root fallback, so
    /// preparation returns an error rather than a different directory.
    fn ensure_workspace_alias(root: &Path, hash: u64) -> std::io::Result<PathBuf> {
        ensure_workspace_alias_at(&workspace_alias_root(), root, hash)
    }

    /// Ensure `base\ws-<hash>` is a junction resolving to `root`. The alias
    /// root and junction are granted to ALL APPLICATION PACKAGES — every
    /// container token can traverse them, while each workspace's own DACL
    /// remains the access boundary for real objects. Returns the verified
    /// junction.
    ///
    /// Concurrent first use races on the same link name: one caller creates
    /// it while others observe it mid-creation. Losers re-check the link
    /// until it resolves to `root`; a foreign or stale entry is removed once
    /// and recreated; a link that never settles is an error — never a
    /// different launch directory.
    fn ensure_workspace_alias_at(base: &Path, root: &Path, hash: u64) -> std::io::Result<PathBuf> {
        let sid = app_packages_sid()?;
        let outcome = settle_workspace_alias(base, root, sid, hash);
        // SAFETY: `sid` came from ConvertStringSidToSidW's LocalAlloc buffer
        // and is freed exactly once here.
        unsafe { LocalFree(sid.0) };
        outcome
    }

    fn settle_workspace_alias(
        base: &Path,
        root: &Path,
        sid: PSID,
        hash: u64,
    ) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(base)?;
        apply_sid_path_grant(
            base,
            sid,
            FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
            SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        )?;
        let link = base.join(format!("ws-{hash:016x}"));
        // Strip the verbatim prefix so the junction stores a plain
        // drive-letter target.
        let target_text = {
            let text = root.to_string_lossy();
            text.strip_prefix("\\\\?\\")
                .map(str::to_owned)
                .unwrap_or_else(|| text.into_owned())
        };
        let target = Path::new(&target_text);
        for _attempt in 0..40 {
            if link.symlink_metadata().is_ok() {
                match std::fs::read_link(&link) {
                    Ok(current)
                        if std::fs::canonicalize(&current)
                            .map(|resolved| resolved == root)
                            .unwrap_or(false) =>
                    {
                        apply_sid_path_grant(
                            &link,
                            sid,
                            FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
                            0,
                        )?;
                        return Ok(link);
                    }
                    Ok(_) => {
                        // A junction at our name pointing somewhere else:
                        // remove it and retry creation.
                        std::fs::remove_dir(&link)?;
                        continue;
                    }
                    Err(_) => {
                        // Mid-creation race (peer holds the handle with no
                        // sharing, so removal fails) or a stale plain
                        // directory: remove when possible, else wait.
                        if std::fs::remove_dir(&link).is_ok() {
                            continue;
                        }
                    }
                }
            } else {
                match create_junction(&link, target) {
                    Ok(()) => {
                        apply_sid_path_grant(
                            &link,
                            sid,
                            FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
                            0,
                        )?;
                        return Ok(link);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Err(std::io::Error::other(format!(
            "workspace launch alias {} did not settle to {}",
            link.display(),
            root.display()
        )))
    }

    /// Whether a `CreateAppContainerProfile` failure may adopt an existing
    /// profile. Kept as a pure decision so the policy is testable without
    /// calling into userenv.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum CreateDisposition {
        /// `ERROR_ALREADY_EXISTS` — the profile is registered, so deriving
        /// its SID is adoption of a real profile, not a guess.
        Adopt,
        /// Any other error — including the concurrent-creation race, which
        /// userenv reports under varying HRESULTs (`E_UNEXPECTED`,
        /// `HRESULT_FROM_WIN32(10)` observed so far). Retried briefly so the
        /// next call lands on create/adopt; if the error persists it is
        /// returned, never converted into an adopted profile — SID
        /// derivation is not a creation-readiness check.
        RetryOrFail,
    }

    fn classify_create_error(code: HRESULT) -> CreateDisposition {
        if code == HRESULT::from_win32(ERROR_ALREADY_EXISTS.0) {
            CreateDisposition::Adopt
        } else {
            CreateDisposition::RetryOrFail
        }
    }

    impl AppContainerProfile {
        /// Resolve a stable, workspace-scoped profile and grant it access only
        /// to that workspace. A separate profile per root prevents a command
        /// confined for one workspace from inheriting ACLs granted to another.
        pub fn for_workspace(workspace_root: &Path) -> std::io::Result<Self> {
            let canonical_root = std::fs::canonicalize(workspace_root)
                .unwrap_or_else(|_| workspace_root.to_path_buf());
            let mut hash = 0xcbf29ce484222325u64;
            for unit in canonical_root
                .to_string_lossy()
                .to_ascii_lowercase()
                .encode_utf16()
            {
                for byte in unit.to_le_bytes() {
                    hash ^= u64::from(byte);
                    hash = hash.wrapping_mul(0x100000001b3);
                }
            }
            let mut profile = Self::ensure(
                &format!("rightkit-agent.sandbox.workspace.{hash:016x}"),
                "Agent workspace sandbox",
            )?;
            // A wide grant must not expose protected locations that happen to
            // live beneath it (for example a workspace that is the user
            // profile). A deny entry cannot carve them out for an AppContainer
            // token (RightKit #887), so the root is refused before any ACE is
            // written.
            refuse_protected_secrets_within(&canonical_root).map_err(|e| {
                step_error(
                    format!("protected paths in workspace {}", canonical_root.display()),
                    e,
                )
            })?;
            profile
                .lease_path_access(&canonical_root, true)
                .map_err(|e| {
                    step_error(format!("lease workspace {}", canonical_root.display()), e)
                })?;
            profile.lease_ancestor_access(&canonical_root)?;
            // Preparation is fail-closed: without a verified alias the
            // profile-rooted cwd reproduces the drive-root fallback, so no
            // launch may proceed from it.
            profile.workspace_root = canonical_root.clone();
            profile.launch_dir = ensure_workspace_alias(&canonical_root, hash).map_err(|e| {
                step_error(
                    format!("prepare workspace alias for {}", canonical_root.display()),
                    e,
                )
            })?;
            Ok(profile)
        }

        /// Create the named profile, or adopt it if a previous run already made
        /// one. Profiles persist across runs by design, so `ALREADY_EXISTS` is a
        /// normal outcome, not a failure.
        ///
        /// SID derivation is only consulted for the explicit already-exists
        /// outcome — it is not a creation-readiness check and must not convert
        /// an arbitrary creation failure into adoption. The demonstrated
        /// concurrent-creation race (userenv surfaces varying transient
        /// HRESULTs while a racing creator finishes) is absorbed by a bounded
        /// retry whose next call lands on the ordinary create/adopt paths; an
        /// error that outlives the window propagates unchanged.
        pub fn ensure(name: &str, display_name: &str) -> std::io::Result<Self> {
            const PROFILE_CREATE_ATTEMPTS: usize = 8;
            let wname = wide(name);
            let wdisplay = wide(display_name);
            let mut attempt = 0usize;
            let sid = loop {
                // SAFETY: both strings are NUL-terminated and outlive the
                // call; no capabilities are granted, so the container has no
                // network and no broker access until a caller opts in.
                let created = unsafe {
                    CreateAppContainerProfile(
                        PCWSTR(wname.as_ptr()),
                        PCWSTR(wdisplay.as_ptr()),
                        PCWSTR(wdisplay.as_ptr()),
                        None,
                    )
                };
                match created {
                    Ok(sid) => break sid,
                    Err(error) => match classify_create_error(error.code()) {
                        CreateDisposition::Adopt => {
                            // SAFETY: same string invariants as above.
                            break unsafe {
                                DeriveAppContainerSidFromAppContainerName(PCWSTR(wname.as_ptr()))
                            }
                            .map_err(|derive| {
                                std::io::Error::other(format!(
                                    "adopt existing AppContainer profile: {derive}"
                                ))
                            })?;
                        }
                        CreateDisposition::RetryOrFail => {
                            attempt += 1;
                            if attempt >= PROFILE_CREATE_ATTEMPTS {
                                return Err(std::io::Error::other(format!(
                                    "create AppContainer profile: {error}"
                                )));
                            }
                            std::thread::sleep(std::time::Duration::from_millis(50));
                        }
                    },
                }
            };
            Ok(Self {
                name: name.to_string(),
                sid,
                workspace_root: PathBuf::new(),
                launch_dir: PathBuf::new(),
                network: false,
                leases: Default::default(),
            })
        }

        /// Allow (or withhold) outbound network for commands launched from
        /// this profile, via the `internetClient` capability. Default off.
        pub fn with_network(mut self, allow: bool) -> Self {
            self.network = allow;
            self
        }

        pub fn network_allowed(&self) -> bool {
            self.network
        }

        /// Grant `path` to this profile for the lifetime of its commands; the
        /// grant is revoked when the last command using it ends. Replaces the
        /// former permanent ACE.
        pub(crate) fn lease_path_access(&self, path: &Path, writable: bool) -> std::io::Result<()> {
            self.lease_path_access_in(&self.leases, path, writable)
        }

        fn lease_path_access_in(
            &self,
            leases: &LeaseSet,
            path: &Path,
            writable: bool,
        ) -> std::io::Result<()> {
            let sid_text = self.sid_string()?;
            let key = lease_key(&sid_text, path);
            let mut items = leases
                .items
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if items
                .iter()
                .any(|lease| lease.key == key && (lease.writable || !writable))
            {
                return Ok(());
            }
            let lease = GrantLease::acquire(self.sid, &sid_text, path, writable)?;
            items.push(lease);
            Ok(())
        }

        /// Lease traverse access on every physical ancestor of `path` for this
        /// profile's SID, through RightKit's AppContainer ancestor lease with
        /// `AncestorLeasePolicy::BestEffort`: an ancestor whose DACL write is
        /// access-denied (no WRITE_DAC, e.g. under `C:\Users`) is skipped rather
        /// than failing the launch. Each skipped ancestor is logged with its
        /// Win32 error, and the deepest one is recorded as a Git ceiling for
        /// sandboxed children. Other ACL failures remain typed `AncestorAccess`
        /// errors. Stale ACEs left by crashed runs of this owner's profiles are
        /// swept first; a sweep failure is logged and does not block the launch.
        fn lease_ancestor_access(&self, path: &Path) -> std::io::Result<()> {
            self.lease_ancestor_access_in(&self.leases, path)
        }

        fn lease_ancestor_access_in(&self, leases: &LeaseSet, path: &Path) -> std::io::Result<()> {
            let sid_text = self.sid_string()?;
            let mut ancestors = leases
                .ancestors
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if ancestors.iter().any(|(leased, _)| leased.as_path() == path) {
                return Ok(());
            }
            // The sweep walks every registered profile of this owner against
            // every ancestor DACL, so its cost grows with the number of
            // profiles ever created on the machine (one per workspace). Stale
            // ACEs only come from crashed *other* processes, so one sweep per
            // ancestor chain per daemon process is sufficient; repeating it on
            // every launch stalled each child spawn for seconds and starved
            // capture/discovery/worktree requests past their client timeouts.
            let sweep_key = path.parent().unwrap_or(path).to_path_buf();
            let first_sweep = {
                static SWEPT: std::sync::OnceLock<
                    std::sync::Mutex<std::collections::HashSet<PathBuf>>,
                > = std::sync::OnceLock::new();
                SWEPT
                    .get_or_init(Default::default)
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .insert(sweep_key)
            };
            if !first_sweep {
                // already swept in this process
            } else if let Err(error) =
                sweep_stale_appcontainer_ancestor_aces_for_owner(path, WORKSPACE_OWNER_PREFIX)
            {
                tracing::warn!(
                    path = %path.display(), %error,
                    "sandbox: stale ancestor ACE sweep failed; continuing without cleanup"
                );
            }
            let lease = AppContainerAncestorLease::acquire_with(
                path,
                &sid_text,
                AncestorLeasePolicy::BestEffort,
            )
            .map_err(ancestor_io_error)?;
            let unleasable = lease.unleasable();
            for ancestor in unleasable {
                tracing::warn!(
                    path = %ancestor.path.display(),
                    win32_error = ancestor.win32_error,
                    workspace = %path.display(),
                    "sandbox: ancestor traverse lease skipped (DACL write denied); Git discovery is bounded by GIT_CEILING_DIRECTORIES"
                );
            }
            // Ancestors nearest the workspace are the deepest; Git must not reach
            // any of them, so the deepest unleasable one bounds discovery.
            if let Some(deepest) = unleasable
                .iter()
                .max_by_key(|ancestor| ancestor.path.components().count())
            {
                let ceiling = plain_path(&deepest.path);
                let mut ceilings = leases
                    .git_ceilings
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if !ceilings.contains(&ceiling) {
                    ceilings.push(ceiling);
                }
            }
            ancestors.push((path.to_path_buf(), lease));
            Ok(())
        }

        /// Per-command extra path grant (an approved sandbox escalation).
        /// Protected locations are refused here and can never be granted.
        pub fn lease_extra_access(&self, path: &Path, writable: bool) -> std::io::Result<()> {
            self.lease_extra_access_in(&self.leases, path, writable)
        }

        fn lease_extra_access_in(
            &self,
            leases: &LeaseSet,
            path: &Path,
            writable: bool,
        ) -> std::io::Result<()> {
            if crate::sandbox::path_is_protected_secret(path) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "{} is a protected location and cannot be granted",
                        path.display()
                    ),
                ));
            }
            let canonical = std::fs::canonicalize(path).map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!("granted path {} does not resolve: {error}", path.display()),
                )
            })?;
            if canonical.parent().is_none() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "refusing to grant a filesystem root to a sandboxed command",
                ));
            }
            refuse_protected_secrets_within(&canonical)?;
            self.lease_path_access_in(leases, &canonical, writable)?;
            self.lease_ancestor_access_in(leases, &canonical)
        }

        /// Read/execute lease on an interpreter install directory, taken once
        /// per daemon process per container SID and reused by every later
        /// launch. Granting an inheritable ACE rewrites the DACL of every
        /// existing descendant (Windows does no check-time inheritance), and
        /// so does revoking it; doing both per launch exhausted short
        /// per-command budgets. The grant is held in a process-wide table with
        /// a user refcount and removed by [`release_pinned_read_roots`]
        /// (immediately when idle, otherwise when the last launch using it
        /// ends). Same bounds as [`Self::lease_extra_access`]: read-only,
        /// protected paths refused; the caller has already restricted the
        /// root to interpreter directories.
        fn lease_pinned_read_root(&self, root: &Path) -> std::io::Result<()> {
            self.pin_lease(root, |set| self.lease_extra_access_in(set, root, false))
        }

        /// Shared pinned-lease plumbing: build the grants into a process-wide
        /// set once per (SID, path), then register this launch as a user.
        fn pin_lease(
            &self,
            path: &Path,
            make: impl FnOnce(&LeaseSet) -> std::io::Result<()>,
        ) -> std::io::Result<()> {
            let sid_text = self.sid_string()?;
            let key = lease_key(&sid_text, path);
            let ceilings = pinned_read_roots().acquire(
                &key,
                || {
                    let set = LeaseSet::default();
                    make(&set)?;
                    Ok(set)
                },
                |set| {
                    set.git_ceilings
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .clone()
                },
            )?;
            self.leases
                .pins
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(PinUse { key });
            let mut mine = self
                .leases
                .git_ceilings
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            for ceiling in ceilings {
                if !mine.contains(&ceiling) {
                    mine.push(ceiling);
                }
            }
            Ok(())
        }

        pub fn name(&self) -> &str {
            &self.name
        }

        /// The container SID in `S-1-15-2-…` string form, as `icacls` expects.
        pub fn sid_string(&self) -> std::io::Result<String> {
            let mut raw = PWSTR::null();
            // SAFETY: self.sid is a valid SID owned by this profile; raw is a
            // valid out-pointer freed below.
            unsafe { ConvertSidToStringSidW(self.sid, &mut raw) }
                .map_err(|error| std::io::Error::other(format!("SID to string: {error}")))?;
            // SAFETY: ConvertSidToStringSidW populated raw with a NUL-terminated
            // string allocated with LocalAlloc.
            let text = unsafe { raw.to_string() }
                .map_err(|error| std::io::Error::other(format!("SID string decode: {error}")))?;
            // SAFETY: raw came from ConvertSidToStringSidW and is freed once.
            unsafe { windows::Win32::Foundation::LocalFree(Some(HLOCAL(raw.0 as *mut _))) };
            Ok(text)
        }

        fn folder_path(&self) -> std::io::Result<PathBuf> {
            let sid = self.sid_string()?;
            let wsid = wide(&sid);
            let mut raw = PWSTR::null();
            // SAFETY: wsid is NUL-terminated and raw is a valid output pointer.
            let status = unsafe { GetAppContainerFolderPath(PCWSTR(wsid.as_ptr()), &mut raw) };
            if status < 0 {
                return Err(std::io::Error::other(format!(
                    "get AppContainer folder path failed: HRESULT 0x{status:08x}"
                )));
            }
            if raw.is_null() {
                return Err(std::io::Error::other(
                    "get AppContainer folder path returned a null path",
                ));
            }
            // SAFETY: successful GetAppContainerFolderPath returns a
            // NUL-terminated CoTaskMemAlloc string owned until freed below.
            let path = unsafe { raw.to_string() }
                .map(PathBuf::from)
                .map_err(|error| {
                    std::io::Error::other(format!("decode AppContainer folder path: {error}"))
                });
            // SAFETY: raw was allocated by GetAppContainerFolderPath.
            unsafe { CoTaskMemFree(raw.0 as *const c_void) };
            path
        }

        /// Grant the container read/write access beneath `path` without spawning
        /// an ACL helper process. Strict local execution permits no child
        /// processes, so AppContainer preparation must stay in-process too.
        pub fn grant_path_access(&self, path: &Path, writable: bool) -> std::io::Result<()> {
            grant_sid_path_access(path, self.sid, writable)
        }

        /// Capability-free token description: no `internetClient` means no
        /// egress, and no broker capabilities means no user-folder access.
        #[allow(dead_code)]
        pub(crate) fn security_capabilities(&self) -> SECURITY_CAPABILITIES {
            SECURITY_CAPABILITIES {
                AppContainerSid: self.sid,
                Capabilities: std::ptr::null_mut(),
                CapabilityCount: 0,
                Reserved: 0,
            }
        }

        /// The token capabilities for one launch from this profile: none by
        /// default, plus `internetClient` when the profile allows network.
        /// A network-enabled profile also takes its loopback exemption lease
        /// here (best effort; see [`update_loopback_exemption`]). The returned
        /// set owns the capability SIDs and must outlive `CreateProcessW`.
        pub(crate) fn capability_set(&self) -> std::io::Result<CapabilitySet> {
            let mut sids = OwnedSids(Vec::new());
            let mut attrs: Vec<SID_AND_ATTRIBUTES> = Vec::new();
            if self.network {
                let sid = sid_from_string(INTERNET_CLIENT_CAPABILITY_SID)
                    .map_err(|e| step_error("internetClient capability SID", e))?;
                sids.0.push(sid);
                attrs.push(SID_AND_ATTRIBUTES {
                    Sid: sid,
                    Attributes: SE_GROUP_ENABLED,
                });
                self.lease_loopback();
            }
            let mut capabilities = self.security_capabilities();
            if !attrs.is_empty() {
                capabilities.Capabilities = attrs.as_mut_ptr();
                capabilities.CapabilityCount = attrs.len() as u32;
            }
            Ok(CapabilitySet {
                _sids: sids,
                _attrs: attrs,
                capabilities,
            })
        }

        /// Take (once per profile) the loopback exemption a network-enabled
        /// container needs to reach local services. Failure is reported on
        /// stderr and never blocks the launch: external network still works,
        /// only loopback stays refused by Windows.
        fn lease_loopback(&self) {
            let mut held = self
                .leases
                .loopback
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if !held.is_empty() {
                return;
            }
            let sid_text = match self.sid_string() {
                Ok(text) => text,
                Err(error) => {
                    tracing::warn!("[sandbox] loopback exemption skipped (container SID): {error}");
                    return;
                }
            };
            match LoopbackLease::acquire(self.sid, &sid_text) {
                Ok(Some(lease)) => held.push(lease),
                Ok(None) => {}
                Err(error) => tracing::warn!(
                    "[sandbox] loopback exemption unavailable for {sid_text}: {error} \
                     (Windows refuses loopback for AppContainers without it; adding it needs \
                     an elevated process — external network is unaffected)"
                ),
            }
        }

        /// Keeps this profile's ACL grants (and loopback exemption) alive for
        /// a child that is not a [`ConfinedProcess`] (the ConPTY backend);
        /// the grants are revoked when the last keep-alive and profile drop.
        pub(crate) fn lease_keepalive(&self) -> std::sync::Arc<dyn std::any::Any + Send + Sync> {
            self.leases.clone()
        }

        /// Read/execute access for the image a confined launch executes.
        ///
        /// The containing directory is granted (so sibling DLLs and tool
        /// payloads resolve) when it is modest in size. A large directory —
        /// a Cargo `deps` folder, a monorepo toolchain — would turn every
        /// launch and every revoke into a DACL rewrite of each descendant,
        /// holding the process-wide ACL locks for seconds; there only the
        /// image file itself is granted.
        fn lease_image_access(&self, image_dir: &Path, program: &Path) -> std::io::Result<()> {
            self.lease_image_access_in(&self.leases, image_dir, program)
        }

        fn lease_image_access_in(
            &self,
            leases: &LeaseSet,
            image_dir: &Path,
            program: &Path,
        ) -> std::io::Result<()> {
            const IMAGE_TREE_ENTRY_BUDGET: usize = 8192;
            if tree_within_budget(image_dir, IMAGE_TREE_ENTRY_BUDGET) {
                return self.lease_path_access_in(leases, image_dir, false);
            }
            let image_file = std::fs::canonicalize(program)
                .map(|resolved| plain_path(&resolved))
                .unwrap_or_else(|_| program.to_path_buf());
            tracing::debug!(
                dir = %image_dir.display(),
                image = %image_file.display(),
                "sandbox: image directory exceeds the lease budget; granting the image file only"
            );
            self.lease_path_access_in(leases, &image_file, false)
        }

        /// Shared image ACL and environment preparation for pipe and PTY launches.
        pub(crate) fn prepare_launch(
            &self,
            program: &Path,
            cwd: &Path,
            extra_env: &[(String, String)],
        ) -> std::io::Result<Vec<u16>> {
            // Shell-backed tools can launch the host Node runtime as a child
            // (hooks, media wrappers, MCP). PATH visibility alone is insufficient:
            // version-manager installs lack ALL APPLICATION PACKAGES access.
            // Lease only the resolved runtime image directory, read/execute,
            // under the same command lifetime as the immediate image.
            let node_runtime = super::resolve_program("node.exe").ok();
            let images = std::iter::once((program.to_path_buf(), false))
                .chain(node_runtime.map(|node| (node, true)));
            for (image, is_node_runtime) in images {
                let program = image.as_path();
                // AppContainer does not inherit the caller's executable ACL. A
                // workspace-scoped profile therefore cannot launch a toolchain
                // image living outside workspace (which Windows may report as
                // ERROR_ENVVAR_NOT_FOUND/203). Grant read/execute only on image's
                // containing directory; no write or capability is added.
                let image_dir = program.parent().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "confined program must have a containing directory",
                    )
                })?;
                // Version managers (nvm-windows, volta, scoop, ...) expose tools
                // through symlinks/junctions whose link objects the user cannot
                // re-ACL. Resolve to the real directory so the read/execute
                // grant lands on the actual image location.
                let real_image_dir = std::fs::canonicalize(image_dir)
                    .map(|resolved| plain_path(&resolved))
                    .unwrap_or_else(|_| image_dir.to_path_buf());
                let image_dir = real_image_dir.as_path();
                let system_root =
                    std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
                let system32 = Path::new(&system_root).join("System32");
                let image_dir_text = image_dir.to_string_lossy().to_ascii_lowercase();
                let system32_text = system32.to_string_lossy().to_ascii_lowercase();
                let is_system_image = image_dir_text == system32_text
                    || image_dir_text.starts_with(&(system32_text + "\\"));
                // Program Files trees already grant ALL APPLICATION PACKAGES
                // read/execute and are not writable by the user, so a per-profile
                // DACL lease there would only fail (git, node, ...).
                let under_program_files = ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"]
                    .iter()
                    .filter_map(std::env::var_os)
                    .any(|root| {
                        let root = root.to_string_lossy().to_ascii_lowercase();
                        !root.is_empty()
                            && (image_dir_text == root
                                || image_dir_text
                                    .starts_with(&format!("{}\\", root.trim_end_matches('\\'))))
                    });
                if !is_system_image
                    && !under_program_files
                    && !app_packages_can_read_execute(image_dir)
                {
                    // The host Node runtime is the same directory on every
                    // launch: pin its grant once per process instead of
                    // rewriting (and revoking) its tree around each command.
                    let leased = refuse_protected_secrets_within(image_dir).and_then(|()| {
                        if is_node_runtime {
                            self.pin_lease(image_dir, |set| {
                                self.lease_image_access_in(set, image_dir, program)
                            })
                        } else {
                            self.lease_image_access(image_dir, program)
                        }
                    });
                    leased.map_err(|e| {
                        step_error(
                            format!(
                                "lease image dir {} (program {})",
                                image_dir.display(),
                                program.display()
                            ),
                            e,
                        )
                    })?;
                }
            }

            // Caller-requested read/execute roots (for example the install
            // directory of a resolved Python interpreter, whose standard
            // library lives beside it). Read-only, per container SID, and
            // revoked with the command's other leases. Roots that cover a
            // per-user directory are refused; a failed lease is logged and the
            // launch proceeds so the child reports the access failure itself.
            let user_dirs: Vec<PathBuf> = ["USERPROFILE", "LOCALAPPDATA", "APPDATA"]
                .iter()
                .filter_map(std::env::var_os)
                .map(PathBuf::from)
                .collect();
            for root in super::parse_sandbox_read_roots(extra_env) {
                let resolved = std::fs::canonicalize(&root)
                    .map(|resolved| plain_path(&resolved))
                    .unwrap_or_else(|_| root.clone());
                if super::read_root_is_too_broad(&resolved, &user_dirs) {
                    tracing::warn!(
                        root = %root.display(),
                        "sandbox: refused a read root that covers a per-user directory"
                    );
                    continue;
                }
                if let Err(error) = self.lease_pinned_read_root(&resolved) {
                    tracing::warn!(
                        root = %resolved.display(), %error,
                        "sandbox: could not lease a requested read root"
                    );
                }
            }

            let profile_folder = self.folder_path()?;
            let git_ceilings = self
                .leases
                .git_ceilings
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            Ok(confined_environment(
                cwd,
                &profile_folder,
                extra_env,
                &git_ceilings,
            ))
        }

        /// Validate `requested` against this profile's workspace and translate
        /// it onto the launch alias. This is the single translation shared by
        /// piped and PTY confined launches.
        ///
        /// The request must resolve to an existing directory inside the
        /// canonical workspace root; `..`, symlinks, or any other spelling
        /// that escapes the root is rejected. The alias-relative result is
        /// canonicalized back to the request before it is returned — a failed
        /// validation is an error, never a different working directory.
        pub(crate) fn confined_child_cwd(&self, requested: &Path) -> std::io::Result<PathBuf> {
            if self.launch_dir.as_os_str().is_empty() || self.workspace_root.as_os_str().is_empty()
            {
                return Ok(requested.to_path_buf());
            }
            let resolved = std::fs::canonicalize(requested).map_err(|error| {
                std::io::Error::other(format!(
                    "confined working directory {} does not resolve: {error}",
                    requested.display()
                ))
            })?;
            let relative = resolved.strip_prefix(&self.workspace_root).map_err(|_| {
                std::io::Error::other(format!(
                    "confined working directory {} escapes the workspace {}",
                    requested.display(),
                    self.workspace_root.display()
                ))
            })?;
            let candidate = self.launch_dir.join(relative);
            let checked = std::fs::canonicalize(&candidate).map_err(|error| {
                std::io::Error::other(format!(
                    "confined working directory {} failed alias validation: {error}",
                    requested.display()
                ))
            })?;
            if checked != resolved {
                return Err(std::io::Error::other(format!(
                    "confined working directory {} resolved to {} through the workspace alias",
                    requested.display(),
                    checked.display()
                )));
            }
            Ok(candidate)
        }

        /// Spawn `program` inside this AppContainer.
        ///
        /// `stdio` optionally supplies inheritable stdin/stdout/stderr handles so
        /// callers can attach pipes; `None` inherits the parent's.
        pub fn spawn_confined(
            &self,
            program: &Path,
            args: &[String],
            cwd: &Path,
            stdio: Option<(HANDLE, HANDLE, HANDLE)>,
        ) -> std::io::Result<ConfinedProcess> {
            self.spawn_confined_with_env(program, args, cwd, stdio, &[])
        }

        pub fn spawn_confined_with_env(
            &self,
            program: &Path,
            args: &[String],
            cwd: &Path,
            stdio: Option<(HANDLE, HANDLE, HANDLE)>,
            extra_env: &[(String, String)],
        ) -> std::io::Result<ConfinedProcess> {
            self.spawn_confined_with_options(program, args, cwd, stdio, extra_env, false)
        }

        /// Spawn with an explicit start state. `suspended` creates the main
        /// thread suspended so the caller can assign the process to its Job
        /// Object before it executes a single instruction, then call
        /// [`ConfinedProcess::resume`].
        pub fn spawn_confined_with_options(
            &self,
            program: &Path,
            args: &[String],
            cwd: &Path,
            stdio: Option<(HANDLE, HANDLE, HANDLE)>,
            extra_env: &[(String, String)],
            suspended: bool,
        ) -> std::io::Result<ConfinedProcess> {
            // Every refusal names its step on stderr (test daemons keep it in
            // %TEMP%\host_e2e-daemon-*.log): a confined launch that
            // cannot be established is never retried unconfined.
            self.spawn_confined_inner(program, args, cwd, stdio, extra_env, suspended)
                .map_err(|error| {
                    tracing::warn!(
                        "[sandbox] confined spawn refused: container={} program={} cwd={} error={error}",
                        self.name,
                        program.display(),
                        cwd.display()
                    );
                    error
                })
        }

        fn spawn_confined_inner(
            &self,
            program: &Path,
            args: &[String],
            cwd: &Path,
            stdio: Option<(HANDLE, HANDLE, HANDLE)>,
            extra_env: &[(String, String)],
            suspended: bool,
        ) -> std::io::Result<ConfinedProcess> {
            let mut command_line = String::new();
            command_line.push('"');
            command_line.push_str(&program.display().to_string());
            command_line.push('"');
            // cmd's command tail is shell source, not CommandLineToArgvW argv.
            // Backslash-escaping its quotes changes paths and redirections.
            let cmd_tail = program
                .file_name()
                .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("cmd.exe"))
                && args.len() >= 2
                && args[args.len() - 2].eq_ignore_ascii_case("/c");
            if cmd_tail {
                command_line.push_str(" /D /S");
            }
            for (index, arg) in args.iter().enumerate() {
                command_line.push(' ');
                if cmd_tail && index == args.len() - 1 {
                    command_line.push('"');
                    command_line.push_str(arg);
                    command_line.push('"');
                } else {
                    append_quoted_arg(&mut command_line, arg);
                }
            }
            let mut wcommand = wide(&command_line);
            let wprogram = wide(&program.display().to_string());
            let child_cwd = self
                .confined_child_cwd(cwd)
                .map_err(|e| step_error("validate confined working directory", e))?;
            let wcwd = wide(&child_cwd.display().to_string());
            let environment = self
                .prepare_launch(program, &child_cwd, extra_env)
                .map_err(|e| step_error("prepare confined launch", e))?;
            let inherited_handles = stdio
                .as_ref()
                .map(|&(stdin, stdout, stderr)| [stdin, stdout, stderr]);

            let mut startup = STARTUPINFOEXW::default();
            startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;

            // Size, then populate an attribute list holding the AppContainer's
            // SECURITY_CAPABILITIES and, for piped stdio, an explicit handle list.
            let mut size: usize = 0;
            // SAFETY: the first call intentionally fails to report the size.
            let _ = unsafe {
                InitializeProcThreadAttributeList(
                    None,
                    if inherited_handles.is_some() { 2 } else { 1 },
                    None,
                    &mut size,
                )
            };
            let mut buffer = vec![0u8; size];
            let attribute_list = LPPROC_THREAD_ATTRIBUTE_LIST(buffer.as_mut_ptr() as _);
            // SAFETY: buffer is `size` bytes as reported above and outlives the
            // CreateProcessW call below.
            unsafe {
                InitializeProcThreadAttributeList(
                    Some(attribute_list),
                    if inherited_handles.is_some() { 2 } else { 1 },
                    None,
                    &mut size,
                )
                .map_err(|error| std::io::Error::other(format!("init attribute list: {error}")))?;
            }
            // Outbound network is a capability, not a filter: the token either
            // carries `internetClient` or the container has no egress at all.
            // `capability_set` owns the capability SIDs and must outlive
            // CreateProcessW.
            let capability_set = match self.capability_set() {
                Ok(set) => set,
                Err(error) => {
                    // SAFETY: the list was initialized above.
                    unsafe { DeleteProcThreadAttributeList(attribute_list) };
                    return Err(error);
                }
            };
            let capabilities = capability_set.security_capabilities();
            // SAFETY: capabilities (and the SIDs it references) outlive the
            // CreateProcessW call; the size matches the struct we pass.
            let update = unsafe {
                UpdateProcThreadAttribute(
                    attribute_list,
                    0,
                    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                    Some(&capabilities as *const _ as *const _),
                    std::mem::size_of::<SECURITY_CAPABILITIES>(),
                    None,
                    None,
                )
            };
            if let Err(error) = update {
                // SAFETY: the list was initialized above.
                unsafe { DeleteProcThreadAttributeList(attribute_list) };
                return Err(std::io::Error::other(format!(
                    "set AppContainer attribute: {error}"
                )));
            }
            if let Some(handles) = inherited_handles.as_ref() {
                // SAFETY: handles remain valid and inheritable through
                // CreateProcessW; the attribute list only borrows this array.
                if let Err(error) = unsafe {
                    UpdateProcThreadAttribute(
                        attribute_list,
                        0,
                        PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                        Some(handles.as_ptr().cast()),
                        std::mem::size_of_val(handles),
                        None,
                        None,
                    )
                } {
                    // SAFETY: the list was initialized above.
                    unsafe { DeleteProcThreadAttributeList(attribute_list) };
                    return Err(std::io::Error::other(format!(
                        "set inherited handle list: {error}"
                    )));
                }
            }
            startup.lpAttributeList = attribute_list;

            let inherit = stdio.is_some();
            if let Some((stdin, stdout, stderr)) = stdio {
                startup.StartupInfo.dwFlags = STARTUPINFOW_FLAGS(STARTF_USESTDHANDLES.0);
                startup.StartupInfo.hStdInput = stdin;
                startup.StartupInfo.hStdOutput = stdout;
                startup.StartupInfo.hStdError = stderr;
            }

            let mut info = PROCESS_INFORMATION::default();
            // SAFETY: command line and cwd are NUL-terminated and mutable as
            // CreateProcessW requires; startup/info are valid out-params.
            let spawned = unsafe {
                CreateProcessW(
                    PCWSTR(wprogram.as_ptr()),
                    Some(PWSTR(wcommand.as_mut_ptr())),
                    None,
                    None,
                    inherit,
                    // Piped children must not share the host console: attached to
                    // the TUI's ConPTY, PowerShell startup repaints/clears the
                    // user's screen mid-command. A private hidden console keeps
                    // the pipes as the only output channel.
                    EXTENDED_STARTUPINFO_PRESENT
                        | CREATE_UNICODE_ENVIRONMENT
                        // Always hidden: without a parent console (detached
                        // daemon) a console child would otherwise open a window.
                        | CREATE_NO_WINDOW
                        | (if suspended { CREATE_SUSPENDED } else { Default::default() }),
                    Some(environment.as_ptr().cast()),
                    PCWSTR(wcwd.as_ptr()),
                    &startup.StartupInfo,
                    &mut info,
                )
            };
            // SAFETY: the list was initialized above and is no longer needed.
            unsafe { DeleteProcThreadAttributeList(attribute_list) };
            spawned.map_err(|error| {
                std::io::Error::other(format!(
                    "CreateProcessW in AppContainer {}: {error}",
                    self.name
                ))
            })?;
            drop(capability_set);
            Ok(ConfinedProcess {
                process: info.hProcess,
                thread: info.hThread,
                pid: info.dwProcessId,
                _leases: self.leases.clone(),
            })
        }
    }

    impl Drop for AppContainerProfile {
        fn drop(&mut self) {
            if !self.sid.is_invalid() {
                // SAFETY: sid came from CreateAppContainerProfile or
                // DeriveAppContainerSidFromAppContainerName and is freed once.
                unsafe { FreeSid(self.sid) };
            }
        }
    }

    /// A process confined to an AppContainer.
    pub struct ConfinedProcess {
        process: HANDLE,
        thread: HANDLE,
        pid: u32,
        /// Keeps the profile's ACL grants alive until this process is gone;
        /// the last drop revokes them.
        _leases: std::sync::Arc<LeaseSet>,
    }

    // SAFETY: process and thread are owned kernel handles. Windows permits
    // waiting on, terminating, and closing these handles from any thread;
    // this value is not cloned, so ownership remains exclusive.
    unsafe impl Send for ConfinedProcess {}

    impl ConfinedProcess {
        pub fn pid(&self) -> u32 {
            self.pid
        }

        pub fn handle(&self) -> HANDLE {
            self.process
        }

        /// Resume a process created suspended (see
        /// [`AppContainerProfile::spawn_confined_with_options`]).
        pub fn resume(&self) -> std::io::Result<()> {
            // SAFETY: self.thread is the live primary-thread handle returned
            // by CreateProcessW and owned by this value.
            if unsafe { ResumeThread(self.thread) } == u32::MAX {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }

        /// Wait for exit and return the exit code.
        pub fn wait(&self) -> std::io::Result<u32> {
            use windows::Win32::System::Threading::{
                GetExitCodeProcess, WaitForSingleObject, INFINITE,
            };
            // SAFETY: self.process is a live process handle owned by this value.
            unsafe { WaitForSingleObject(self.process, INFINITE) };
            let mut code = 0u32;
            // SAFETY: same handle; code is a valid out-param.
            unsafe { GetExitCodeProcess(self.process, &mut code) }
                .map_err(|error| std::io::Error::other(format!("exit code: {error}")))?;
            Ok(code)
        }
    }

    impl Drop for ConfinedProcess {
        fn drop(&mut self) {
            for handle in [self.thread, self.process] {
                if !handle.is_invalid() && handle != INVALID_HANDLE_VALUE {
                    // SAFETY: handles came from CreateProcessW; closed once.
                    let _ = unsafe { CloseHandle(handle) };
                }
            }
        }
    }
}

#[cfg(windows)]
pub use imp::{release_pinned_read_roots, AppContainerProfile, ConfinedProcess};

#[cfg(windows)]
mod async_adapter {
    use super::imp::{AppContainerProfile, ConfinedProcess};
    use std::ffi::c_void;
    use std::future::Future;
    use std::io::{self, Read, Write};
    use std::os::windows::io::{FromRawHandle, RawHandle};
    use std::path::{Path, PathBuf};
    use std::pin::Pin;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tokio::task::JoinHandle;
    use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, TerminateProcess, WaitForSingleObject,
    };

    const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
    const PIPE_ACCESS_INBOUND: u32 = 0x0000_0001;
    const PIPE_ACCESS_OUTBOUND: u32 = 0x0000_0002;
    const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
    const PIPE_REJECT_REMOTE_CLIENTS: u32 = 0x0000_0008;
    const OPEN_EXISTING: u32 = 3;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const ERROR_PIPE_CONNECTED: u32 = 535;
    const INVALID_HANDLE_VALUE: *mut c_void = (-1isize) as *mut c_void;

    #[repr(C)]
    struct SecurityAttributes {
        length: u32,
        descriptor: *mut c_void,
        inherit_handle: i32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
        fn SetHandleInformation(handle: *mut c_void, mask: u32, flags: u32) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn GetLastError() -> u32;
        fn CreateNamedPipeW(
            name: *const u16,
            open_mode: u32,
            pipe_mode: u32,
            max_instances: u32,
            out_buffer_size: u32,
            in_buffer_size: u32,
            default_timeout: u32,
            attributes: *const SecurityAttributes,
        ) -> *mut c_void;
        fn CreateFileW(
            name: *const u16,
            desired_access: u32,
            share_mode: u32,
            security: *const c_void,
            creation_disposition: u32,
            flags_and_attributes: u32,
            template: *mut c_void,
        ) -> *mut c_void;
        fn ConnectNamedPipe(handle: *mut c_void, overlapped: *mut c_void) -> i32;
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            string_security_descriptor: *const u16,
            string_sd_revision: u32,
            security_descriptor: *mut *mut c_void,
            security_descriptor_size: *mut u32,
        ) -> i32;
    }

    fn io_error(operation: &str) -> io::Error {
        io::Error::other(format!("{operation}: {}", io::Error::last_os_error()))
    }

    fn set_inheritable(handle: RawHandle, inheritable: bool) -> io::Result<()> {
        let flags = if inheritable { HANDLE_FLAG_INHERIT } else { 0 };
        // SAFETY: handle is a live kernel handle owned by the pipe setup.
        if unsafe { SetHandleInformation(handle as *mut c_void, HANDLE_FLAG_INHERIT, flags) } == 0 {
            return Err(io_error("SetHandleInformation"));
        }
        Ok(())
    }

    fn close_raw(handle: RawHandle) {
        // SAFETY: each handle is closed exactly once by pipe setup cleanup.
        unsafe {
            let _ = CloseHandle(handle as *mut c_void);
        }
    }

    struct PipeSecurityDescriptor {
        descriptor: *mut c_void,
    }

    impl PipeSecurityDescriptor {
        fn new(appcontainer_sid: &str) -> io::Result<Self> {
            let user_sid = rightkit_daemon::current_user_sid()?;
            let sddl =
                format!("D:P(A;;GA;;;{user_sid})(A;;GA;;;{appcontainer_sid})S:(ML;;NW;;;S-1-16-0)");
            let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
            let mut descriptor = std::ptr::null_mut();
            let mut descriptor_size = 0u32;
            // SAFETY: wide is NUL-terminated and output pointers are valid.
            if unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    wide.as_ptr(),
                    1,
                    &mut descriptor,
                    &mut descriptor_size,
                )
            } == 0
            {
                return Err(io_error(
                    "ConvertStringSecurityDescriptorToSecurityDescriptorW",
                ));
            }
            Ok(Self { descriptor })
        }

        fn attributes(&self) -> SecurityAttributes {
            SecurityAttributes {
                length: std::mem::size_of::<SecurityAttributes>() as u32,
                descriptor: self.descriptor,
                inherit_handle: 0,
            }
        }
    }

    impl Drop for PipeSecurityDescriptor {
        fn drop(&mut self) {
            // SAFETY: descriptor was allocated by SDDL conversion and is freed once.
            unsafe { LocalFree(self.descriptor) };
        }
    }

    struct PipeHandles {
        child_stdin: RawHandle,
        parent_stdin: std::fs::File,
        child_stdout: RawHandle,
        parent_stdout: std::fs::File,
        child_stderr: RawHandle,
        parent_stderr: std::fs::File,
    }

    fn make_named_pipe(
        stdin: bool,
        security: &PipeSecurityDescriptor,
    ) -> io::Result<(std::fs::File, RawHandle)> {
        static PIPE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            r"\\.\pipe\rightkit-agent-appcontainer-{}-{}",
            std::process::id(),
            PIPE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let wide_name: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let server_mode = (if stdin {
            PIPE_ACCESS_OUTBOUND
        } else {
            PIPE_ACCESS_INBOUND
        }) | FILE_FLAG_FIRST_PIPE_INSTANCE;
        let client_access = if stdin { GENERIC_READ } else { GENERIC_WRITE };
        let attributes = security.attributes();
        // SAFETY: name and attributes remain live through creation call.
        let server = unsafe {
            CreateNamedPipeW(
                wide_name.as_ptr(),
                server_mode,
                PIPE_REJECT_REMOTE_CLIENTS,
                1,
                64 * 1024,
                64 * 1024,
                0,
                &attributes,
            )
        };
        if server.is_null() || server == INVALID_HANDLE_VALUE {
            return Err(io_error("CreateNamedPipeW"));
        }
        // SAFETY: client opens unique server endpoint with requested direction.
        let client = unsafe {
            CreateFileW(
                wide_name.as_ptr(),
                client_access,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if client.is_null() || client == INVALID_HANDLE_VALUE {
            let error = io_error("CreateFileW pipe client");
            close_raw(server);
            return Err(error);
        }
        // SAFETY: client is a live child endpoint; server is synchronously connected.
        let connected = unsafe { ConnectNamedPipe(server, std::ptr::null_mut()) } != 0;
        if !connected && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
            let error = io_error("ConnectNamedPipe");
            close_raw(client);
            close_raw(server);
            return Err(error);
        }
        if let Err(error) = set_inheritable(client, true) {
            close_raw(client);
            close_raw(server);
            return Err(error);
        }
        // SAFETY: server is the parent endpoint and ownership transfers to File.
        let parent = unsafe { std::fs::File::from_raw_handle(server as RawHandle) };
        Ok((parent, client as RawHandle))
    }

    fn make_pipes(_with_stdin: bool, appcontainer_sid: &str) -> io::Result<PipeHandles> {
        let security = PipeSecurityDescriptor::new(appcontainer_sid)?;
        // Use the same ACL-bound pipe for null & writable stdin. A bare
        // NUL path can resolve inside a verbatim workspace path on Windows;
        // closing the parent endpoint after spawn gives the child real EOF.
        let (parent_stdin, child_stdin) = make_named_pipe(true, &security)?;
        let (parent_stdout, child_stdout) = match make_named_pipe(false, &security) {
            Ok(pair) => pair,
            Err(error) => {
                close_raw(child_stdin);
                return Err(error);
            }
        };
        let (parent_stderr, child_stderr) = match make_named_pipe(false, &security) {
            Ok(pair) => pair,
            Err(error) => {
                close_raw(child_stdin);
                close_raw(child_stdout);
                return Err(error);
            }
        };
        Ok(PipeHandles {
            child_stdin,
            parent_stdin,
            child_stdout,
            parent_stdout,
            child_stderr,
            parent_stderr,
        })
    }

    struct BlockingReader {
        file: Arc<Mutex<std::fs::File>>,
        pending: Option<JoinHandle<io::Result<Vec<u8>>>>,
        /// Bytes read from the pipe that did not fit in the caller's
        /// `ReadBuf`. A blocking read returns up to 8 KiB regardless of how
        /// much room the caller offered, so the remainder is carried here and
        /// drained on subsequent polls instead of overflowing the buffer.
        leftover: Vec<u8>,
    }

    impl BlockingReader {
        fn new(file: std::fs::File) -> Self {
            Self {
                file: Arc::new(Mutex::new(file)),
                pending: None,
                leftover: Vec::new(),
            }
        }

        /// Move as much of `leftover` into `output` as it will hold.
        fn drain_leftover(&mut self, output: &mut ReadBuf<'_>) {
            let take = self.leftover.len().min(output.remaining());
            if take > 0 {
                output.put_slice(&self.leftover[..take]);
                self.leftover.drain(..take);
            }
        }
    }

    impl AsyncRead for BlockingReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            output: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            if !self.leftover.is_empty() {
                self.drain_leftover(output);
                return Poll::Ready(Ok(()));
            }
            if output.remaining() == 0 {
                return Poll::Ready(Ok(()));
            }
            if let Some(mut pending) = self.pending.take() {
                match Pin::new(&mut pending).poll(cx) {
                    Poll::Pending => {
                        self.pending = Some(pending);
                        return Poll::Pending;
                    }
                    Poll::Ready(Ok(Ok(bytes))) => {
                        let take = bytes.len().min(output.remaining());
                        output.put_slice(&bytes[..take]);
                        if take < bytes.len() {
                            self.leftover.extend_from_slice(&bytes[take..]);
                        }
                        return Poll::Ready(Ok(()));
                    }
                    Poll::Ready(Ok(Err(error))) => return Poll::Ready(Err(error)),
                    Poll::Ready(Err(error)) => {
                        return Poll::Ready(Err(io::Error::other(format!(
                            "blocking pipe reader task: {error}"
                        ))))
                    }
                }
            }
            let file = Arc::clone(&self.file);
            self.pending = Some(tokio::task::spawn_blocking(move || {
                let mut bytes = vec![0u8; 8192];
                let count = match file
                    .lock()
                    .map_err(|_| io::Error::other("blocking pipe reader lock poisoned"))?
                    .read(&mut bytes)
                {
                    Ok(count) => count,
                    // Windows named pipes report a closed writer as
                    // ERROR_BROKEN_PIPE, which is EOF for AsyncRead.
                    Err(error) if error.kind() == io::ErrorKind::BrokenPipe => 0,
                    Err(error) => return Err(error),
                };
                bytes.truncate(count);
                Ok(bytes)
            }));
            self.poll_read(cx, output)
        }
    }

    struct BlockingWriter {
        file: Arc<Mutex<std::fs::File>>,
        pending: Option<JoinHandle<io::Result<usize>>>,
        flush_pending: Option<JoinHandle<io::Result<()>>>,
    }

    impl BlockingWriter {
        fn new(file: std::fs::File) -> Self {
            Self {
                file: Arc::new(Mutex::new(file)),
                pending: None,
                flush_pending: None,
            }
        }
    }

    impl AsyncWrite for BlockingWriter {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            if let Some(mut pending) = self.pending.take() {
                match Pin::new(&mut pending).poll(cx) {
                    Poll::Pending => {
                        self.pending = Some(pending);
                        return Poll::Pending;
                    }
                    Poll::Ready(Ok(result)) => return Poll::Ready(result),
                    Poll::Ready(Err(error)) => {
                        return Poll::Ready(Err(io::Error::other(format!(
                            "blocking pipe writer task: {error}"
                        ))))
                    }
                }
            }
            let file = Arc::clone(&self.file);
            let bytes = bytes.to_vec();
            self.pending = Some(tokio::task::spawn_blocking(move || {
                file.lock()
                    .map_err(|_| io::Error::other("blocking pipe writer lock poisoned"))?
                    .write(&bytes)
            }));
            self.poll_write(cx, &[])
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            if let Some(mut pending) = self.flush_pending.take() {
                match Pin::new(&mut pending).poll(cx) {
                    Poll::Pending => {
                        self.flush_pending = Some(pending);
                        return Poll::Pending;
                    }
                    Poll::Ready(Ok(result)) => return Poll::Ready(result),
                    Poll::Ready(Err(error)) => {
                        return Poll::Ready(Err(io::Error::other(format!(
                            "blocking pipe flush task: {error}"
                        ))))
                    }
                }
            }
            let file = Arc::clone(&self.file);
            self.flush_pending = Some(tokio::task::spawn_blocking(move || {
                file.lock()
                    .map_err(|_| io::Error::other("blocking pipe flush lock poisoned"))?
                    .flush()
            }));
            self.poll_flush(cx)
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    pub type AsyncStdin = Box<dyn AsyncWrite + Send + Unpin>;
    pub type AsyncOutput = Box<dyn AsyncRead + Send + Unpin>;

    pub struct ConfinedPipedChild {
        /// Kill-on-close Job Object this child was assigned to while it was
        /// still suspended, so it can never run (or spawn) outside the Job.
        /// Declared before `process` so it drops first: survivors are killed
        /// before `process` releases the profile's ACL leases.
        job: Option<crate::sandbox::WindowsJob>,
        process: ConfinedProcess,
        stdin: Option<AsyncStdin>,
        stdout: Option<AsyncOutput>,
        stderr: Option<AsyncOutput>,
    }

    impl ConfinedPipedChild {
        pub fn pid(&self) -> u32 {
            self.process.pid()
        }

        pub fn take_stdin(&mut self) -> Option<AsyncStdin> {
            self.stdin.take()
        }

        pub fn take_stdout(&mut self) -> Option<AsyncOutput> {
            self.stdout.take()
        }

        pub fn take_stderr(&mut self) -> Option<AsyncOutput> {
            self.stderr.take()
        }

        /// The Job Object this child lives in (assigned before it started).
        pub fn job(&self) -> Option<&crate::sandbox::WindowsJob> {
            self.job.as_ref()
        }

        /// Hand the Job Object to a caller that owns the tree's lifetime.
        pub fn take_job(&mut self) -> Option<crate::sandbox::WindowsJob> {
            self.job.take()
        }

        /// Kill the child and, through its Job Object, every descendant.
        pub fn terminate(&self) -> io::Result<()> {
            if let Some(job) = &self.job {
                if job.terminate().is_ok() {
                    return Ok(());
                }
            }
            // SAFETY: process handle remains owned by self through call.
            unsafe { TerminateProcess(self.process.handle(), 1) }
                .map_err(|error| io::Error::other(format!("terminate AppContainer child: {error}")))
        }

        pub async fn wait_code(&mut self) -> io::Result<u32> {
            // Cancellation must not leave a blocking waiter holding a raw
            // handle after this borrowed child drops. A zero-time wait stays
            // within the borrow and lets supervisor deadlines release locks.
            loop {
                // SAFETY: this child owns the handle for the entire call.
                match unsafe { WaitForSingleObject(self.process.handle(), 0) } {
                    WAIT_OBJECT_0 => break,
                    WAIT_TIMEOUT => {
                        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                    }
                    _ => return Err(io::Error::last_os_error()),
                }
            }
            let mut code = 0u32;
            // SAFETY: code is a valid out-parameter; the handle is still owned.
            unsafe { GetExitCodeProcess(self.process.handle(), &mut code) }
                .map_err(|error| io::Error::other(format!("exit code: {error}")))?;
            Ok(code)
        }
    }

    pub fn resolve_program(program: &str) -> io::Result<PathBuf> {
        let path = Path::new(program);
        if path.is_absolute() {
            return Ok(path.to_path_buf());
        }
        if let Some(path_var) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path_var) {
                let candidate = dir.join(path);
                if candidate.is_file() {
                    return Ok(candidate);
                }
                if path.extension().is_none() {
                    let candidate = dir.join(format!("{program}.exe"));
                    if candidate.is_file() {
                        return Ok(candidate);
                    }
                }
            }
        }
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        for dir in [
            PathBuf::from(&system_root).join("System32"),
            PathBuf::from(&system_root)
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0"),
        ] {
            let candidate = dir.join(path);
            if candidate.is_file() {
                return Ok(candidate);
            }
            if path.extension().is_none() {
                let candidate = dir.join(format!("{program}.exe"));
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("resolve Windows program '{program}' for AppContainer"),
        ))
    }

    pub fn spawn_confined_piped(
        profile: &AppContainerProfile,
        program: &str,
        args: &[String],
        cwd: &Path,
        with_stdin: bool,
    ) -> io::Result<ConfinedPipedChild> {
        spawn_confined_piped_with_env(profile, program, args, cwd, with_stdin, &[])
    }

    pub fn spawn_confined_piped_with_env(
        profile: &AppContainerProfile,
        program: &str,
        args: &[String],
        cwd: &Path,
        with_stdin: bool,
        env: &[(String, String)],
    ) -> io::Result<ConfinedPipedChild> {
        let requested_program = program;
        let program = resolve_program(requested_program).map_err(|error| {
            tracing::warn!("[sandbox] confined spawn refused: cannot resolve program '{requested_program}': {error}");
            error
        })?;
        // CreateProcessW does not execute batch files. Use the system cmd
        // image explicitly while retaining this profile, pipes and Job Object.
        // Quote every literal argument; refuse cmd expansions/quote escapes
        // rather than converting data arguments into additional shell effects.
        let batch_args;
        let program = if program.extension().is_some_and(|extension| {
            extension.to_string_lossy().eq_ignore_ascii_case("cmd")
                || extension.to_string_lossy().eq_ignore_ascii_case("bat")
        }) {
            let mut command = String::new();
            for arg in
                std::iter::once(program.to_string_lossy().into_owned()).chain(args.iter().cloned())
            {
                if arg.contains(['"', '%', '!', '\r', '\n', '\0']) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "batch argument contains a cmd expansion or quote escape",
                    ));
                }
                if !command.is_empty() {
                    command.push(' ');
                }
                command.push('"');
                command.push_str(&arg);
                command.push('"');
            }
            batch_args = vec!["/C".to_string(), command];
            let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
            PathBuf::from(system_root).join("System32").join("cmd.exe")
        } else {
            batch_args = args.to_vec();
            program
        };
        let args = batch_args.as_slice();
        // A `.exe`/`.com` that is not a PE image can never start, but asking
        // the kernel to create it inside an AppContainer stalls for many
        // seconds before failing, which starves bounded discovery probes.
        // Refuse it up front (fail closed, same error class as a bad image).
        if program.extension().is_some_and(|extension| {
            extension.to_string_lossy().eq_ignore_ascii_case("exe")
                || extension.to_string_lossy().eq_ignore_ascii_case("com")
        }) {
            use std::io::Read;
            let mut magic = [0u8; 2];
            let is_pe = std::fs::File::open(&program)
                .and_then(|mut file| file.read_exact(&mut magic))
                .is_ok()
                && &magic == b"MZ";
            if !is_pe {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} is not a valid executable image", program.display()),
                ));
            }
        }
        let appcontainer_sid = profile.sid_string().map_err(|error| {
            tracing::warn!("[sandbox] confined spawn refused: container SID unavailable: {error}");
            error
        })?;
        let pipes = make_pipes(with_stdin, &appcontainer_sid).map_err(|error| {
            tracing::warn!("[sandbox] confined spawn refused: stdio pipe setup failed for '{requested_program}': {error}");
            error
        })?;
        let stdio = (
            HANDLE(pipes.child_stdin as *mut c_void),
            HANDLE(pipes.child_stdout as *mut c_void),
            HANDLE(pipes.child_stderr as *mut c_void),
        );
        // Created suspended: the process is assigned to its Job Object before
        // its first instruction runs, so no fast child can spawn descendants
        // outside the Job. Any failure before resume kills the process and
        // fails the launch closed.
        let process = match profile.spawn_confined_with_options(
            &program,
            args,
            cwd,
            Some(stdio),
            env,
            true,
        ) {
            Ok(process) => process,
            Err(error) => {
                close_raw(pipes.child_stdin);
                close_raw(pipes.child_stdout);
                close_raw(pipes.child_stderr);
                return Err(error);
            }
        };
        close_raw(pipes.child_stdin);
        close_raw(pipes.child_stdout);
        close_raw(pipes.child_stderr);
        let job = match crate::sandbox::shell_job().and_then(|job| {
            job.assign_process_id(process.pid())?;
            Ok(job)
        }) {
            Ok(job) => job,
            Err(error) => {
                // SAFETY: the suspended process handle is owned by `process`.
                let _ = unsafe { TerminateProcess(process.handle(), 1) };
                tracing::warn!(
                    "[sandbox] confined spawn refused: pid {} could not be assigned to its Job Object before start (terminated): {error}",
                    process.pid()
                );
                return Err(io::Error::other(format!(
                    "assign confined process to its Job Object before start: {error}"
                )));
            }
        };
        if let Err(error) = process.resume() {
            let _ = job.terminate();
            tracing::warn!(
                "[sandbox] confined spawn refused: pid {} failed to resume after Job Object assignment (terminated): {error}",
                process.pid()
            );
            return Err(io::Error::other(format!(
                "resume confined process after Job Object assignment: {error}"
            )));
        }
        Ok(ConfinedPipedChild {
            job: Some(job),
            process,
            stdin: if with_stdin {
                Some(Box::new(BlockingWriter::new(pipes.parent_stdin)))
            } else {
                drop(pipes.parent_stdin);
                None
            },
            stdout: Some(Box::new(BlockingReader::new(pipes.parent_stdout))),
            stderr: Some(Box::new(BlockingReader::new(pipes.parent_stderr))),
        })
    }
}

#[cfg(windows)]
pub use async_adapter::{
    spawn_confined_piped, spawn_confined_piped_with_env, AsyncOutput, AsyncStdin,
    ConfinedPipedChild,
};

#[cfg(windows)]
pub(crate) use async_adapter::resolve_program;
