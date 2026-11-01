//! Orthic-only peer admission for the shared speech service.
//!
//! Decision (Adrian, relayed by the Dell lead chat, 2026-10-31): the service accepts "Only orthic apps for now".
//! `rightkit-daemon`'s `Listener` proves only that the peer is the current user (bogusyogi/rightkit#1319 tracks a
//! public peer code-signature API there). Until that ships, this module runs the check for the raw `Listener` host:
//! every accepted connection is verified here before one request byte is read (`transport::listen`).
//!
//! Identities (never guessed; read from the repo):
//! - macOS Developer ID team `6KLGD3LLKF` (`docs/RIGHT_RELEASE_BUILD_UPLOAD.md:51`, `docs/SIGNING.md:19`,
//!   `/Volumes/D/claude/docs/MAC_DEVELOPER_ID_SIGNING.md:11`; every Mach-O in the release receipt carries it,
//!   `docs/results/2026-11-01-overnight/levers-data/mac-build/sign-receipt.txt`).
//! - Windows Authenticode signer `CN=Damned Ventures LLC`, Azure Artifact Signing (`docs/WINDOWS-SIGNING.md:5`; the
//!   release `install` kind signs the exe and its sidecars, `right-release.config.mjs:138,146`).
//!
//! Dev and unsigned builds are admitted only when this crate is built for its tests or with `test-mode`
//! ([`BuildMode::current`]); there is no environment switch, so a release build always enforces.

use rightkit_daemon::PeerIdentity;

/// Orthic Labs Apple Developer team identifier (`kSecCodeInfoTeamIdentifier` / leaf `subject.OU`).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) const ORTHIC_MAC_TEAM_ID: &str = "6KLGD3LLKF";

/// Common name of the Orthic Authenticode signing certificate (Azure Artifact Signing).
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) const ORTHIC_WINDOWS_SIGNER_CN: &str = "Damned Ventures LLC";

/// What the platform check established about the peer process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PeerVerification {
    /// Proven: the peer's running code satisfies the Orthic signing requirement.
    Orthic,
    /// The check ran and the peer does not satisfy it (unsigned, ad-hoc, another signer). Carries the OS reason.
    NotOrthic(String),
    /// The check could not run to an answer (no audit token, process gone, pid reused, unsupported platform).
    Unverifiable(String),
}

/// Which policy applies. Fixed at compile time; never read from the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BuildMode {
    /// Product builds: only a verified Orthic peer is admitted. A `test-mode` build runs as
    /// `TestMode`, so there it is only named by tests.
    #[cfg_attr(all(feature = "test-mode", not(test)), allow(dead_code))]
    Release,
    /// This crate's tests and `test-mode` builds: unsigned local peers are admitted. Absent from product builds.
    #[cfg(any(test, feature = "test-mode"))]
    TestMode,
}

impl BuildMode {
    pub(super) const fn current() -> Self {
        #[cfg(any(test, feature = "test-mode"))]
        {
            Self::TestMode
        }
        #[cfg(not(any(test, feature = "test-mode")))]
        {
            Self::Release
        }
    }
}

/// The admission outcome for one connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Decision {
    /// Verified Orthic peer.
    Admit,
    /// Not verified Orthic, admitted only because this is a test-mode build.
    #[cfg(any(test, feature = "test-mode"))]
    AdmitUnverifiedTestMode,
    /// Refuse and close before reading anything.
    Refuse,
}

/// Pure admission rule. Fails closed: anything short of a proven Orthic signature is refused in a release build.
pub(super) fn decide(verification: &PeerVerification, mode: BuildMode) -> Decision {
    match (verification, mode) {
        (PeerVerification::Orthic, _) => Decision::Admit,
        #[cfg(any(test, feature = "test-mode"))]
        (_, BuildMode::TestMode) => Decision::AdmitUnverifiedTestMode,
        (_, BuildMode::Release) => Decision::Refuse,
    }
}

/// The macOS code requirement for `team_id`, or `None` when it is not a 10-character alphanumeric team id (the
/// value is spliced into a requirement string, so anything else is refused rather than escaped).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) fn mac_requirement(team_id: &str) -> Option<String> {
    let valid = team_id.len() == 10
        && team_id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    valid.then(|| format!("anchor apple generic and certificate leaf[subject.OU] = \"{team_id}\""))
}

/// Exact, case-sensitive signer common-name match. An empty name never matches.
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) fn windows_signer_matches(found: &str, expected: &str) -> bool {
    !found.is_empty() && found == expected
}

/// Verify the peer of an accepted connection on this platform.
pub(super) fn verify_peer(peer: &PeerIdentity) -> PeerVerification {
    platform::verify(peer)
}

/// Run the check and apply the policy. `true` admits the connection; `false` means the caller must close it
/// without reading. A refusal is logged with the peer pid and a content-free reason.
pub(super) fn admit(peer: &PeerIdentity) -> bool {
    let verification = verify_peer(peer);
    match decide(&verification, BuildMode::current()) {
        Decision::Admit => true,
        #[cfg(any(test, feature = "test-mode"))]
        Decision::AdmitUnverifiedTestMode => {
            tracing::debug!(event = "service_peer_unverified_test_mode", pid = ?peer.pid, ?verification);
            true
        }
        Decision::Refuse => {
            let (kind, reason) = match &verification {
                PeerVerification::Orthic => ("orthic", ""),
                PeerVerification::NotOrthic(reason) => ("not_orthic", reason.as_str()),
                PeerVerification::Unverifiable(reason) => ("unverifiable", reason.as_str()),
            };
            tracing::warn!(event = "service_peer_untrusted", pid = ?peer.pid, kind, reason);
            false
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use core_foundation::base::TCFType;
    use core_foundation::data::CFData;
    use security_framework::os::macos::code_signing::{
        Flags, GuestAttributes, SecCode, SecRequirement,
    };

    use super::{mac_requirement, PeerIdentity, PeerVerification, ORTHIC_MAC_TEAM_ID};

    pub(super) fn verify(peer: &PeerIdentity) -> PeerVerification {
        // The audit token names one process instance; a bare pid can be reused between accept and check, so a
        // peer without a token is not verified.
        let Some(token) = peer.audit_token else {
            return PeerVerification::Unverifiable(
                "the kernel gave no audit token for the peer".into(),
            );
        };
        verify_audit_token(&token, ORTHIC_MAC_TEAM_ID)
    }

    /// `SecCodeCopyGuestWithAttributes(kSecGuestAttributeAudit)` then `SecCodeCheckValidity` against the team
    /// requirement. Dynamic validation of the running process: unsigned and ad-hoc code cannot satisfy
    /// `anchor apple generic`.
    pub(super) fn verify_audit_token(token: &[u32; 8], team_id: &str) -> PeerVerification {
        let Some(text) = mac_requirement(team_id) else {
            return PeerVerification::Unverifiable(
                "the configured team identifier is malformed".into(),
            );
        };
        let requirement: SecRequirement = match text.parse() {
            Ok(requirement) => requirement,
            Err(error) => {
                return PeerVerification::Unverifiable(format!(
                    "requirement did not compile: {error}"
                ));
            }
        };
        let mut bytes = [0u8; 32];
        for (chunk, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(token) {
            chunk.copy_from_slice(&word.to_ne_bytes());
        }
        let data = CFData::from_buffer(&bytes);
        let mut attributes = GuestAttributes::new();
        attributes.set_audit_token(data.as_concrete_TypeRef());
        let guest = match SecCode::copy_guest_with_attribues(None, &attributes, Flags::NONE) {
            Ok(guest) => guest,
            Err(error) => {
                return PeerVerification::Unverifiable(format!(
                    "peer code object not found (OSStatus {}): {error}",
                    error.code()
                ));
            }
        };
        match guest.check_validity(Flags::NONE, &requirement) {
            Ok(()) => PeerVerification::Orthic,
            Err(error) => PeerVerification::NotOrthic(format!(
                "code requirement not satisfied (OSStatus {}): {error}",
                error.code()
            )),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::ffi::c_void;

        const TASK_AUDIT_TOKEN: u32 = 15;
        const TASK_AUDIT_TOKEN_COUNT: u32 = 8;

        extern "C" {
            fn mach_task_self() -> *const c_void;
            fn task_info(
                task: *const c_void,
                flavor: u32,
                out: *mut c_void,
                count: *mut u32,
            ) -> i32;
        }

        fn own_audit_token() -> [u32; 8] {
            let mut token = [0u32; 8];
            let mut count = TASK_AUDIT_TOKEN_COUNT;
            // SAFETY: TASK_AUDIT_TOKEN fills eight natural_t words into a buffer of exactly that size.
            let status = unsafe {
                task_info(
                    mach_task_self(),
                    TASK_AUDIT_TOKEN,
                    token.as_mut_ptr().cast(),
                    &mut count,
                )
            };
            assert_eq!(status, 0, "task_info(TASK_AUDIT_TOKEN) failed");
            token
        }

        #[test]
        fn a_running_process_that_is_not_signed_by_the_team_is_not_orthic() {
            // The test binary is ad-hoc or unsigned; no such binary can satisfy a Developer ID team requirement.
            let result = verify_audit_token(&own_audit_token(), "AAAAAAAAAA");
            assert!(
                matches!(result, PeerVerification::NotOrthic(_)),
                "got {result:?}"
            );
        }

        #[test]
        fn a_token_naming_no_process_is_unverifiable_never_orthic() {
            let result = verify_audit_token(&[0u32; 8], ORTHIC_MAC_TEAM_ID);
            assert!(
                matches!(result, PeerVerification::Unverifiable(_)),
                "got {result:?}"
            );
        }

        #[test]
        fn a_peer_without_an_audit_token_is_unverifiable() {
            let peer = PeerIdentity::default();
            assert!(matches!(verify(&peer), PeerVerification::Unverifiable(_)));
        }

        #[test]
        fn a_malformed_team_id_never_reaches_the_requirement_compiler() {
            let result = verify_audit_token(&own_audit_token(), "x\" or anchor apple");
            assert!(
                matches!(result, PeerVerification::Unverifiable(_)),
                "got {result:?}"
            );
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::ffi::{c_void, OsString};
    use std::os::windows::ffi::OsStringExt;
    use std::ptr::null_mut;

    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Security::Cryptography::{
        szOID_COMMON_NAME, CertGetNameStringW, CERT_NAME_ATTR_TYPE,
    };
    use windows_sys::Win32::Security::WinTrust::{
        WTHelperGetProvCertFromChain, WTHelperGetProvSignerFromChain,
        WTHelperProvDataFromStateData, WinVerifyTrust, WINTRUST_ACTION_GENERIC_VERIFY_V2,
        WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO, WTD_CACHE_ONLY_URL_RETRIEVAL,
        WTD_CHOICE_FILE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY,
        WTD_UI_NONE,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    use super::{windows_signer_matches, PeerIdentity, PeerVerification, ORTHIC_WINDOWS_SIGNER_CN};

    /// Closes the process handle. While it is open the process object exists, so its pid names one process.
    struct Process(HANDLE);

    impl Drop for Process {
        fn drop(&mut self) {
            // SAFETY: the handle came from OpenProcess and is closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    impl Process {
        fn open(pid: u32) -> Option<Self> {
            // SAFETY: plain call; a null result is the failure.
            let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
            (!handle.is_null()).then_some(Self(handle))
        }

        /// Creation time as `rightkit-daemon` records it in `PeerIdentity::started` on Windows.
        fn start_marker(&self) -> Option<(u64, u64)> {
            let zero = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let (mut creation, mut exit, mut kernel, mut user) = (zero, zero, zero, zero);
            // SAFETY: four valid out-parameters for an open process handle.
            let ok = unsafe {
                GetProcessTimes(self.0, &mut creation, &mut exit, &mut kernel, &mut user)
            };
            (ok != 0).then(|| {
                (
                    (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime),
                    0,
                )
            })
        }

        /// Win32 path of the process image, without a trailing NUL.
        fn image_path(&self) -> Option<Vec<u16>> {
            let mut buf = vec![0u16; 32768];
            let mut len = buf.len() as u32;
            // SAFETY: `buf` holds `len` u16 units; flag 0 is the Win32 path form.
            let ok = unsafe { QueryFullProcessImageNameW(self.0, 0, buf.as_mut_ptr(), &mut len) };
            if ok == 0 || len == 0 {
                return None;
            }
            buf.truncate(len as usize);
            Some(buf)
        }
    }

    pub(super) fn verify(peer: &PeerIdentity) -> PeerVerification {
        let Some(pid) = peer.pid else {
            return PeerVerification::Unverifiable("the OS gave no pid for the peer".into());
        };
        let Some(process) = Process::open(pid) else {
            return PeerVerification::Unverifiable(format!(
                "peer process could not be opened: {}",
                std::io::Error::last_os_error()
            ));
        };
        // The pid must still be the process that connected (creation time recorded at accept), and must stay so
        // across the image-path read.
        let before = process.start_marker();
        if before.is_none() || peer.started != before {
            return PeerVerification::Unverifiable(
                "the pid is not the process that connected".into(),
            );
        }
        let Some(mut path) = process.image_path() else {
            return PeerVerification::Unverifiable("peer image path unreadable".into());
        };
        if process.start_marker() != before {
            return PeerVerification::Unverifiable(
                "peer creation time changed while reading its image".into(),
            );
        }
        path.push(0);
        let result = verify_authenticode(&path, ORTHIC_WINDOWS_SIGNER_CN);
        // Re-check after the (slow) trust query: the handle pinned the process, so the pid still names it.
        if process.start_marker() != before {
            return PeerVerification::Unverifiable(
                "peer creation time changed during verification".into(),
            );
        }
        result
    }

    /// `WinVerifyTrust` (no UI, no network: cached URL retrieval only) on the NUL-terminated image path, then the
    /// leaf signer's common name against `expected_cn`.
    ///
    /// The check covers the image file on disk at verification time; Windows offers no handle-bound proof that the
    /// running pages equal that file.
    fn verify_authenticode(wide_path: &[u16], expected_cn: &str) -> PeerVerification {
        // SAFETY: structures are zero-initialised then filled; every pointer outlives the WinVerifyTrust calls;
        // the state opened by VERIFY is released by the CLOSE call below on every path.
        unsafe {
            let mut file = WINTRUST_FILE_INFO {
                cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
                pcwszFilePath: wide_path.as_ptr(),
                ..Default::default()
            };
            let mut data = WINTRUST_DATA {
                cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
                dwUIChoice: WTD_UI_NONE,
                fdwRevocationChecks: WTD_REVOKE_NONE,
                dwUnionChoice: WTD_CHOICE_FILE,
                Anonymous: WINTRUST_DATA_0 { pFile: &mut file },
                dwStateAction: WTD_STATEACTION_VERIFY,
                dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL,
                ..Default::default()
            };
            let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
            let status = WinVerifyTrust(
                INVALID_HANDLE_VALUE,
                &mut action,
                (&mut data as *mut WINTRUST_DATA).cast::<c_void>(),
            );
            let outcome = if status != 0 {
                PeerVerification::NotOrthic(format!("WinVerifyTrust 0x{:08X}", status as u32))
            } else {
                match signer_common_name(data.hWVTStateData) {
                    Some(name) if windows_signer_matches(&name, expected_cn) => {
                        PeerVerification::Orthic
                    }
                    Some(name) => {
                        PeerVerification::NotOrthic(format!("signed by another publisher ({name})"))
                    }
                    None => PeerVerification::Unverifiable("signer certificate unreadable".into()),
                }
            };
            data.dwStateAction = WTD_STATEACTION_CLOSE;
            WinVerifyTrust(
                INVALID_HANDLE_VALUE,
                &mut action,
                (&mut data as *mut WINTRUST_DATA).cast::<c_void>(),
            );
            outcome
        }
    }

    /// Common name of the leaf certificate of the first verified signer in the open WinVerifyTrust state.
    ///
    /// # Safety
    /// `state` must be the `hWVTStateData` of a `WTD_STATEACTION_VERIFY` call that has not been closed.
    unsafe fn signer_common_name(state: HANDLE) -> Option<String> {
        let provider = WTHelperProvDataFromStateData(state);
        if provider.is_null() {
            return None;
        }
        let signer = WTHelperGetProvSignerFromChain(provider, 0, 0, 0);
        if signer.is_null() {
            return None;
        }
        let cert = WTHelperGetProvCertFromChain(signer, 0);
        if cert.is_null() || (*cert).pCert.is_null() {
            return None;
        }
        let context = (*cert).pCert;
        let oid = szOID_COMMON_NAME as *const c_void;
        let needed = CertGetNameStringW(context, CERT_NAME_ATTR_TYPE, 0, oid, null_mut(), 0);
        if needed <= 1 {
            return None;
        }
        let mut buf = vec![0u16; needed as usize];
        let written = CertGetNameStringW(
            context,
            CERT_NAME_ATTR_TYPE,
            0,
            oid,
            buf.as_mut_ptr(),
            needed,
        );
        if written <= 1 {
            return None;
        }
        Some(
            OsString::from_wide(&buf[..written as usize - 1])
                .to_string_lossy()
                .into_owned(),
        )
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use super::{PeerIdentity, PeerVerification};

    /// No signing identity exists on this platform: every peer is unverifiable (refused in a release build).
    pub(super) fn verify(_peer: &PeerIdentity) -> PeerVerification {
        PeerVerification::Unverifiable(
            "no peer code-signature verification on this platform".into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_verifications() -> Vec<PeerVerification> {
        vec![
            PeerVerification::Orthic,
            PeerVerification::NotOrthic("ad hoc".into()),
            PeerVerification::Unverifiable("no token".into()),
        ]
    }

    #[test]
    fn release_admits_only_a_proven_orthic_peer() {
        for verification in all_verifications() {
            let expected = if verification == PeerVerification::Orthic {
                Decision::Admit
            } else {
                Decision::Refuse
            };
            assert_eq!(
                decide(&verification, BuildMode::Release),
                expected,
                "{verification:?}"
            );
        }
    }

    #[test]
    fn test_mode_admits_unsigned_peers_and_still_marks_an_orthic_one_admit() {
        assert_eq!(
            decide(&PeerVerification::Orthic, BuildMode::TestMode),
            Decision::Admit
        );
        assert_eq!(
            decide(
                &PeerVerification::NotOrthic("ad hoc".into()),
                BuildMode::TestMode
            ),
            Decision::AdmitUnverifiedTestMode
        );
        assert_eq!(
            decide(
                &PeerVerification::Unverifiable("none".into()),
                BuildMode::TestMode
            ),
            Decision::AdmitUnverifiedTestMode
        );
    }

    #[test]
    fn this_crates_tests_run_in_test_mode_and_a_product_build_is_release() {
        // `cfg(test)` is the only way this assertion compiles in; a build without it or `test-mode` is Release.
        assert_eq!(BuildMode::current(), BuildMode::TestMode);
    }

    #[test]
    fn the_mac_requirement_names_the_orthic_team_and_rejects_malformed_ids() {
        assert_eq!(
            mac_requirement(ORTHIC_MAC_TEAM_ID).as_deref(),
            Some("anchor apple generic and certificate leaf[subject.OU] = \"6KLGD3LLKF\"")
        );
        for bad in [
            "",
            "6KLGD3LLK",
            "6KLGD3LLKFX",
            "6klgd3llkf",
            "6KLGD3LLK\"",
            "6KLGD3LL K",
        ] {
            assert_eq!(mac_requirement(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_windows_signer_match_is_exact() {
        assert!(windows_signer_matches(
            "Damned Ventures LLC",
            ORTHIC_WINDOWS_SIGNER_CN
        ));
        for bad in [
            "",
            "damned ventures llc",
            "Damned Ventures LLC ",
            "Damned Ventures",
            "Someone Else",
        ] {
            assert!(
                !windows_signer_matches(bad, ORTHIC_WINDOWS_SIGNER_CN),
                "{bad:?}"
            );
        }
    }

    #[test]
    #[cfg(not(any(target_os = "macos", windows)))]
    fn an_unsupported_platform_is_unverifiable() {
        assert!(matches!(
            verify_peer(&PeerIdentity::default()),
            PeerVerification::Unverifiable(_)
        ));
    }
}
