//! Server-side peer code-signature check for hosts that accept only signed peers
//! (rightkit#1319, and the peer-identity half of rightkit#1501).
//!
//! [`Listener::accept`](crate::Listener::accept) proves the peer runs as the current user, and
//! nothing more. [`verify_peer`] takes the [`PeerIdentity`] of an accepted connection and a
//! [`PeerPolicy`] and answers whether the process behind it is code-signed by one of the
//! identities the policy names, so a raw-`Listener` host (the shared speech service) and a
//! `Handler` host (an owner-only route such as the licence-snapshot delivery) use the same RightKit
//! code instead of carrying Security.framework or `WinVerifyTrust` FFI of their own.
//!
//! * **macOS** - `SecCodeCopyGuestWithAttributes` with `kSecGuestAttributeAudit` from the peer's
//!   audit token (the kernel's record of the connecting process instance, never a bare pid that
//!   could be reused), then `SecCodeCheckValidity` against
//!   `anchor apple generic and certificate leaf[subject.OU] = "<team id>"`. This is dynamic
//!   validation of the running code: unsigned, ad-hoc and other-team code cannot satisfy it.
//! * **Windows** - `WinVerifyTrust` (no UI, cached URL retrieval only) on the peer's image path
//!   as the kernel reports it for the process handle that is open during the check, then the
//!   leaf signer's common name against the policy. The check covers the image file on disk at
//!   verification time; Windows offers no handle-bound proof that the running pages equal that
//!   file.
//! * **Elsewhere** - no signing identity exists, so every peer is refused
//!   ([`HostIdentityReason::Unverified`]).
//!
//! Fail closed: a policy that names no identity for this platform, a peer with no audit token or
//! a pid that is not the process that connected, a process that exited, an unreadable image, an
//! unsigned or untrusted binary and a signer that is not named are all `Err`. A pid is pinned by
//! its start time recorded at accept (the same pin as
//! [`CallContext::verify_peer_executable`](crate::CallContext::verify_peer_executable)).

use crate::host_identity::{verify_peer_process, HostExpectation, HostIdentityError, HostProcess};
use crate::peer::PeerIdentity;
use std::path::{Path, PathBuf};

/// A code-signing identity a peer may carry.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SigningIdentity {
    /// macOS: the Apple Developer team identifier (`kSecCodeInfoTeamIdentifier`, the leaf
    /// certificate's `subject.OU`), exactly ten upper-case letters or digits. Anything else
    /// never matches (the value is spliced into a requirement string, so it is refused rather
    /// than escaped).
    MacTeamId(String),
    /// macOS: a Developer ID Application signature with this exact team and code-signing
    /// identifier. Use this identity alone when a same-team executable must not be admitted.
    MacDeveloperId {
        /// Apple Developer team identifier.
        team_id: String,
        /// The signed executable's code-signing identifier.
        identifier: String,
    },
    /// Windows: the common name of the Authenticode leaf signer certificate, compared exactly
    /// and case-sensitively. An empty name never matches.
    WindowsCommonName(String),
}

/// Which signed peers a host accepts. Build it with [`PeerPolicy::signed_by`] and
/// [`or_signed_by`](Self::or_signed_by); only the identities that belong to the running
/// platform are consulted, and a policy with none for it refuses every peer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PeerPolicy {
    signers: Vec<SigningIdentity>,
    executables: Vec<PathBuf>,
}

impl PeerPolicy {
    /// Accept a peer signed by `identity`.
    pub fn signed_by(identity: SigningIdentity) -> Self {
        Self {
            signers: vec![identity],
            executables: Vec::new(),
        }
    }

    /// Also accept a peer signed by `identity` (the macOS and the Windows identity of one
    /// product, say).
    #[must_use]
    pub fn or_signed_by(mut self, identity: SigningIdentity) -> Self {
        self.signers.push(identity);
        self
    }

    /// Additionally require the peer's executable to be one of these paths (compared as
    /// [`HostExpectation`] compares). Without any, the signature alone decides.
    #[must_use]
    pub fn only_executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.executables.push(path.into());
        self
    }

    /// The identities this policy names.
    pub fn signers(&self) -> &[SigningIdentity] {
        &self.signers
    }

    /// This policy as a [`HostExpectation`] (the optional executable pins plus the signature
    /// verifier), for an API that takes an expectation, such as
    /// `LicenseGate::deliver_from_owner(cx, &policy.host_expectation(), frame)` in
    /// `rightkit-agent-server` (rightkit#1501). Used with `CallContext::verify_peer_executable`
    /// it gives the same answer as `CallContext::verify_peer_signed`.
    pub fn host_expectation(&self) -> HostExpectation {
        expectation(self)
    }
}

/// The private-IPC method that carries the owner's licence snapshot (rightkit#1501). The host
/// registers it on its private service and allows only the owner app; its handler authenticates
/// the peer with [`CallContext::verify_peer_signed`](crate::CallContext::verify_peer_signed) or a
/// [`PeerPolicy::host_expectation`] before reading the frame, so the loopback HTTP route
/// `/internal/license-snapshot` (bearer token only) can be retired.
pub const LICENSE_SNAPSHOT_METHOD: &str = "license.snapshot";

/// Why a signature check did not admit a binary.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SignatureError {
    /// The check ran and the binary does not satisfy it: unsigned, ad-hoc, untrusted chain or a
    /// signer that is not the one named. Carries the OS reason.
    NotSigned(String),
    /// The check could not run to an answer (no audit token, process gone, unreadable file).
    Unverifiable(String),
    /// This platform has no signing identity to check.
    Unsupported(&'static str),
}

impl std::fmt::Display for SignatureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignatureError::NotSigned(why) => {
                write!(f, "the code signature is not acceptable ({why})")
            }
            SignatureError::Unverifiable(why) => {
                write!(f, "the code signature could not be checked ({why})")
            }
            SignatureError::Unsupported(why) => {
                write!(f, "code signatures are not checked here ({why})")
            }
        }
    }
}

impl std::error::Error for SignatureError {}

/// Is the process behind `peer` (the identity recorded when the connection was accepted) one the
/// `policy` admits? Returns the identified process, the same [`HostProcess`] the executable check
/// returns, so the caller can log its pid and path.
///
/// Call it per connection, before reading a request, and do not cache the answer past the
/// connection. See the module docs for what each platform verifies and for the fail-closed rules.
pub fn verify_peer(
    peer: &PeerIdentity,
    policy: &PeerPolicy,
) -> Result<HostProcess, HostIdentityError> {
    verify_peer_process(Some(peer), &expectation(policy))
}

/// The [`HostExpectation`] that encodes `policy`: the optional executable pins plus a verifier
/// that runs the platform signature check. Used by [`verify_peer`] and by
/// `CallContext::verify_peer_signed`.
pub(crate) fn expectation(policy: &PeerPolicy) -> HostExpectation {
    let signers = policy.signers.clone();
    let check = move |host: &HostProcess| check_signature(host, &signers);
    let mut expect = HostExpectation::any_executable_verified(check.clone());
    if !policy.executables.is_empty() {
        expect = HostExpectation::any_of(policy.executables.clone()).with_verifier(check);
    }
    expect
}

fn check_signature(host: &HostProcess, signers: &[SigningIdentity]) -> Result<(), String> {
    if signers.is_empty() {
        return Err("the policy names no signing identity".to_string());
    }
    platform::check(host, signers).map_err(|e| e.to_string())
}

/// What `WinVerifyTrust` established about a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticodeSigner {
    /// The common name of the leaf signer certificate.
    pub common_name: String,
}

/// Expected Authenticode publisher whose short-lived leaf certificates rotate.
/// All values must come from trusted caller policy, independently of the candidate file.
#[derive(Debug, Clone, Copy)]
pub struct WindowsPublisherPolicy<'a> {
    /// Exact signer common name.
    pub common_name: &'a str,
    /// Publisher-specific OID in the signed leaf's enhanced key usage extension.
    pub identity_eku: &'a str,
    /// SHA-256 of the DER-encoded CA one level below the root.
    pub parent_ca_sha256: [u8; 32],
    /// SHA-256 of the DER-encoded trust root.
    pub root_ca_sha256: [u8; 32],
}

/// Verify the Authenticode signature of the file at `path` (Windows, `WinVerifyTrust` with no UI
/// and cached URL retrieval only) and return its signer (rightkit#405). With
/// `expected_common_name` the signer's common name must equal it exactly. On other platforms
/// the answer is [`SignatureError::Unsupported`]: nothing is ever reported as verified there.
pub fn verify_authenticode(
    path: &Path,
    expected_common_name: Option<&str>,
) -> Result<AuthenticodeSigner, SignatureError> {
    platform::authenticode(path, expected_common_name)
}

/// Verify Authenticode against an already open executable. Windows derives the provider
/// path from the handle and supplies that same handle to WinVerifyTrust. The caller must
/// keep writes excluded and independently bind product identity and launch to the file.
/// Other platforms return [`SignatureError::Unsupported`].
pub fn verify_held_authenticode(
    file: &std::fs::File,
    expected_common_name: Option<&str>,
) -> Result<AuthenticodeSigner, SignatureError> {
    #[cfg(windows)]
    {
        let path = rightkit_fs::handle::final_path_verbatim(file).map_err(|error| {
            SignatureError::Unverifiable(format!("cannot resolve held executable: {error}"))
        })?;
        platform::authenticode_with_handle(&path, expected_common_name, Some(file), None)
    }
    #[cfg(not(windows))]
    {
        let _ = (file, expected_common_name);
        Err(SignatureError::Unsupported(
            "Authenticode is a Windows signature",
        ))
    }
}

/// Verify a write-excluded, open Windows executable against trusted publisher policy.
/// Require WinVerifyTrust revocation checks and a verified chain matching the explicit pins.
/// The caller must retain the same file for product identity and launch binding.
pub fn verify_held_windows_publisher(
    file: &std::fs::File,
    policy: &WindowsPublisherPolicy<'_>,
) -> Result<AuthenticodeSigner, SignatureError> {
    #[cfg(windows)]
    {
        let path = rightkit_fs::handle::final_path_verbatim(file).map_err(|error| {
            SignatureError::Unverifiable(format!("cannot resolve held executable: {error}"))
        })?;
        platform::authenticode_with_handle(
            &path,
            Some(policy.common_name),
            Some(file),
            Some(policy),
        )
    }
    #[cfg(not(windows))]
    {
        let _ = (file, policy);
        Err(SignatureError::Unsupported(
            "Authenticode is a Windows signature",
        ))
    }
}

/// The macOS code requirement for `team_id`, or `None` when it is not a ten-character
/// upper-case alphanumeric team identifier.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn mac_requirement(team_id: &str) -> Option<String> {
    let valid = team_id.len() == 10
        && team_id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    valid.then(|| format!("anchor apple generic and certificate leaf[subject.OU] = \"{team_id}\""))
}

#[cfg(any(target_os = "macos", test))]
fn mac_developer_id_requirement(team_id: &str, identifier: &str) -> Option<String> {
    if identifier.is_empty()
        || !identifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
    {
        return None;
    }
    let team = mac_requirement(team_id)?;
    // Match the Developer ID Application certificate requirements used by RightKitUpdate.
    Some(format!(
        "{team} and identifier \"{identifier}\" \
         and certificate 1[field.1.2.840.113635.100.6.2.6] \
         and certificate leaf[field.1.2.840.113635.100.6.1.13]"
    ))
}

/// Exact, case-sensitive signer common-name match. An empty name never matches.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn signer_matches(found: &str, expected: &str) -> bool {
    !found.is_empty() && found == expected
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{
        mac_developer_id_requirement, mac_requirement, AuthenticodeSigner, SignatureError,
        SigningIdentity,
    };
    use crate::host_identity::HostProcess;
    use core_foundation::base::TCFType;
    use core_foundation::data::CFData;
    use security_framework::os::macos::code_signing::{
        Flags, GuestAttributes, SecCode, SecRequirement,
    };
    use std::path::Path;

    pub(super) fn authenticode(
        _path: &Path,
        _expected: Option<&str>,
    ) -> Result<AuthenticodeSigner, SignatureError> {
        Err(SignatureError::Unsupported(
            "Authenticode is a Windows signature",
        ))
    }

    pub(super) fn check(
        host: &HostProcess,
        signers: &[SigningIdentity],
    ) -> Result<(), SignatureError> {
        // The audit token names one process instance; a bare pid can be reused between accept
        // and check, so a peer without a token is not verified.
        let Some(token) = host.audit_token else {
            return Err(SignatureError::Unverifiable(
                "the kernel gave no audit token for the peer".into(),
            ));
        };
        let mut requirements = signers.iter().filter_map(|s| match s {
            SigningIdentity::MacTeamId(team) => Some(mac_requirement(team)),
            SigningIdentity::MacDeveloperId {
                team_id,
                identifier,
            } => Some(mac_developer_id_requirement(team_id, identifier)),
            _ => None,
        });
        let Some(first) = requirements.next() else {
            return Err(SignatureError::Unsupported(
                "the policy names no macOS team identifier",
            ));
        };
        let mut last = verify_audit_token(&token, first.as_deref());
        for requirement in requirements {
            if last.is_ok() {
                break;
            }
            last = verify_audit_token(&token, requirement.as_deref());
        }
        last
    }

    /// `SecCodeCopyGuestWithAttributes(kSecGuestAttributeAudit)` then `SecCodeCheckValidity`
    /// against the configured requirement.
    fn verify_audit_token(
        token: &[u32; 8],
        requirement: Option<&str>,
    ) -> Result<(), SignatureError> {
        let Some(text) = requirement else {
            return Err(SignatureError::Unverifiable(
                "the configured macOS signing identity is malformed".into(),
            ));
        };
        let requirement: SecRequirement = text.parse().map_err(|error| {
            SignatureError::Unverifiable(format!("requirement did not compile: {error}"))
        })?;
        let mut bytes = [0u8; 32];
        for (i, word) in token.iter().enumerate() {
            bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_ne_bytes());
        }
        let data = CFData::from_buffer(&bytes);
        let mut attributes = GuestAttributes::new();
        attributes.set_audit_token(data.as_concrete_TypeRef());
        let guest = SecCode::copy_guest_with_attribues(None, &attributes, Flags::NONE).map_err(
            |error| {
                SignatureError::Unverifiable(format!(
                    "peer code object not found (OSStatus {}): {error}",
                    error.code()
                ))
            },
        )?;
        guest
            .check_validity(Flags::NONE, &requirement)
            .map_err(|error| {
                SignatureError::NotSigned(format!(
                    "code requirement not satisfied (OSStatus {}): {error}",
                    error.code()
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::mac_developer_id_requirement;

    #[test]
    fn developer_id_requirement_binds_team_identifier_and_certificate() {
        let requirement = mac_developer_id_requirement("ABC1234567", "com.example.app.cli");
        assert_eq!(
            requirement.as_deref(),
            Some(
                "anchor apple generic and certificate leaf[subject.OU] = \"ABC1234567\" \
                 and identifier \"com.example.app.cli\" \
                 and certificate 1[field.1.2.840.113635.100.6.2.6] \
                 and certificate leaf[field.1.2.840.113635.100.6.1.13]"
            )
        );
        assert!(mac_developer_id_requirement("bad", "com.example.app.cli").is_none());
        assert!(mac_developer_id_requirement("ABC1234567", "").is_none());
        assert!(mac_developer_id_requirement("ABC1234567", "x\" or true").is_none());
    }
}

#[cfg(windows)]
mod platform {
    use super::{
        signer_matches, AuthenticodeSigner, SignatureError, SigningIdentity, WindowsPublisherPolicy,
    };
    use crate::host_identity::HostProcess;
    use std::ffi::{c_void, CStr, OsString};
    use std::fs::File;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::{
        CERT_E_REVOCATION_FAILURE, CRYPT_E_NO_REVOCATION_CHECK, CRYPT_E_REVOCATION_OFFLINE, HANDLE,
        INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Security::Cryptography::{
        szOID_COMMON_NAME, CertGetEnhancedKeyUsage, CertGetNameStringW, CryptHashCertificate2,
        CERT_CONTEXT, CERT_FIND_EXT_ONLY_ENHKEY_USAGE_FLAG, CERT_NAME_ATTR_TYPE,
        CERT_TRUST_IS_OFFLINE_REVOCATION, CERT_TRUST_IS_REVOKED,
        CERT_TRUST_REVOCATION_STATUS_UNKNOWN, CTL_USAGE,
    };
    use windows_sys::Win32::Security::WinTrust::{
        WTHelperGetProvCertFromChain, WTHelperGetProvSignerFromChain,
        WTHelperProvDataFromStateData, WinVerifyTrust, CRYPT_PROVIDER_SGNR,
        WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO,
        WINTRUST_SIGNATURE_SETTINGS, WSS_VERIFY_SPECIFIC, WTD_CACHE_ONLY_URL_RETRIEVAL,
        WTD_CHOICE_FILE, WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT, WTD_REVOKE_NONE,
        WTD_REVOKE_WHOLECHAIN, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
    };

    pub(super) fn check(
        host: &HostProcess,
        signers: &[SigningIdentity],
    ) -> Result<(), SignatureError> {
        let names: Vec<&str> = signers
            .iter()
            .filter_map(|s| match s {
                SigningIdentity::WindowsCommonName(cn) => Some(cn.as_str()),
                _ => None,
            })
            .collect();
        if names.is_empty() {
            return Err(SignatureError::Unsupported(
                "the policy names no Windows signer",
            ));
        }
        let signer = authenticode(&host.executable, None)?;
        if names.iter().any(|n| signer_matches(&signer.common_name, n)) {
            Ok(())
        } else {
            Err(SignatureError::NotSigned(format!(
                "signed by another publisher ({})",
                signer.common_name
            )))
        }
    }

    pub(super) fn authenticode(
        path: &Path,
        expected: Option<&str>,
    ) -> Result<AuthenticodeSigner, SignatureError> {
        authenticode_with_handle(path, expected, None, None)
    }

    pub(super) fn authenticode_with_handle(
        path: &Path,
        expected: Option<&str>,
        held: Option<&File>,
        publisher: Option<&WindowsPublisherPolicy<'_>>,
    ) -> Result<AuthenticodeSigner, SignatureError> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: structures are zero-initialised then filled; every pointer outlives the
        // WinVerifyTrust calls; the state opened by VERIFY is released by the CLOSE call below
        // on every path.
        unsafe {
            let mut file = WINTRUST_FILE_INFO {
                cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
                pcwszFilePath: wide.as_ptr(),
                hFile: held.map_or(null_mut(), File::as_raw_handle),
                ..Default::default()
            };
            let mut signature = WINTRUST_SIGNATURE_SETTINGS {
                cbStruct: std::mem::size_of::<WINTRUST_SIGNATURE_SETTINGS>() as u32,
                dwIndex: 0,
                dwFlags: WSS_VERIFY_SPECIFIC,
                dwVerifiedSigIndex: u32::MAX,
                ..Default::default()
            };
            let mut data = WINTRUST_DATA {
                cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
                dwUIChoice: WTD_UI_NONE,
                fdwRevocationChecks: if publisher.is_some() {
                    WTD_REVOKE_WHOLECHAIN
                } else {
                    WTD_REVOKE_NONE
                },
                dwUnionChoice: WTD_CHOICE_FILE,
                Anonymous: WINTRUST_DATA_0 { pFile: &mut file },
                dwStateAction: WTD_STATEACTION_VERIFY,
                dwProvFlags: if publisher.is_some() {
                    WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT
                } else {
                    WTD_CACHE_ONLY_URL_RETRIEVAL
                },
                pSignatureSettings: if publisher.is_some() {
                    &mut signature
                } else {
                    null_mut()
                },
                ..Default::default()
            };
            let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
            let status = WinVerifyTrust(
                INVALID_HANDLE_VALUE,
                &mut action,
                (&mut data as *mut WINTRUST_DATA).cast::<c_void>(),
            );
            let outcome = if publisher.is_some()
                && matches!(
                    status,
                    CERT_E_REVOCATION_FAILURE
                        | CRYPT_E_NO_REVOCATION_CHECK
                        | CRYPT_E_REVOCATION_OFFLINE
                ) {
                Err(SignatureError::Unverifiable(format!(
                    "WinVerifyTrust revocation status 0x{:08X}",
                    status as u32
                )))
            } else if status != 0 {
                Err(SignatureError::NotSigned(format!(
                    "WinVerifyTrust 0x{:08X}",
                    status as u32
                )))
            } else if publisher.is_some() && signature.dwVerifiedSigIndex != 0 {
                Err(SignatureError::Unverifiable(
                    "WinVerifyTrust did not verify the primary signature".into(),
                ))
            } else if let Some(signer) = verified_signer(data.hWVTStateData) {
                match signer_common_name(signer) {
                    Some(name) => match expected {
                        Some(expected) if !signer_matches(&name, expected) => {
                            Err(SignatureError::NotSigned(format!(
                                "signed by another publisher ({name})"
                            )))
                        }
                        _ if publisher.is_some_and(|policy| !matches_publisher(signer, policy)) => {
                            Err(SignatureError::NotSigned(
                                "signer's verified certificate chain or identity EKU differs from policy".into(),
                            ))
                        }
                        _ => Ok(AuthenticodeSigner { common_name: name }),
                    },
                    None => Err(SignatureError::Unverifiable(
                        "signer certificate unreadable".into(),
                    )),
                }
            } else {
                Err(SignatureError::Unverifiable(
                    "signer certificate unreadable".into(),
                ))
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

    // The caller keeps this successful WinVerifyTrust state open for all signer reads.
    unsafe fn verified_signer(state: HANDLE) -> Option<*mut CRYPT_PROVIDER_SGNR> {
        let provider = WTHelperProvDataFromStateData(state);
        if provider.is_null() {
            return None;
        }
        let signer = WTHelperGetProvSignerFromChain(provider, 0, 0, 0);
        (!signer.is_null()).then_some(signer)
    }

    unsafe fn signer_common_name(signer: *mut CRYPT_PROVIDER_SGNR) -> Option<String> {
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

    /// The signer points into the currently open, successful WinVerifyTrust state.
    unsafe fn matches_publisher(
        signer: *mut CRYPT_PROVIDER_SGNR,
        policy: &WindowsPublisherPolicy<'_>,
    ) -> bool {
        const REVOCATION_ERROR: u32 = CERT_TRUST_IS_REVOKED
            | CERT_TRUST_REVOCATION_STATUS_UNKNOWN
            | CERT_TRUST_IS_OFFLINE_REVOCATION;
        let Some(chain) = (*signer).pChainContext.as_ref() else {
            return false;
        };
        if (*signer).dwError != 0
            || chain.cChain != 1
            || chain.rgpChain.is_null()
            || chain.TrustStatus.dwErrorStatus & REVOCATION_ERROR != 0
        {
            return false;
        }
        let Some(chain) = (*chain.rgpChain).as_ref() else {
            return false;
        };
        // Leaf, issuing CA, pinned parent CA, pinned root. Issuing certificates may rotate.
        if chain.cElement != 4
            || chain.rgpElement.is_null()
            || chain.TrustStatus.dwErrorStatus & REVOCATION_ERROR != 0
        {
            return false;
        }
        let revocation_failed = (0..3).any(|index| {
            (*chain.rgpElement.add(index))
                .as_ref()
                .is_none_or(|element| {
                    element.TrustStatus.dwErrorStatus & REVOCATION_ERROR != 0
                        || element
                            .pRevocationInfo
                            .as_ref()
                            .is_none_or(|info| info.dwRevocationResult != 0)
                })
        });
        if revocation_failed {
            return false;
        }
        let cert_at = |index| {
            (*chain.rgpElement.add(index))
                .as_ref()
                .map(|element| element.pCertContext)
                .filter(|context| !context.is_null())
        };
        let (Some(leaf), Some(parent), Some(root)) = (cert_at(0), cert_at(2), cert_at(3)) else {
            return false;
        };
        let verified_leaf = WTHelperGetProvCertFromChain(signer, 0);
        if verified_leaf.is_null() || (*verified_leaf).pCert.is_null() {
            return false;
        }
        certificate_sha256(leaf)
            .is_some_and(|digest| certificate_sha256((*verified_leaf).pCert) == Some(digest))
            && certificate_sha256(parent) == Some(policy.parent_ca_sha256)
            && certificate_sha256(root) == Some(policy.root_ca_sha256)
            && publisher_ekus_match(leaf, policy.identity_eku)
    }

    unsafe fn certificate_sha256(cert: *const CERT_CONTEXT) -> Option<[u8; 32]> {
        let mut digest = [0u8; 32];
        let mut size = digest.len() as u32;
        let ok = CryptHashCertificate2(
            windows_sys::core::w!("SHA256"),
            0,
            null(),
            (*cert).pbCertEncoded,
            (*cert).cbCertEncoded,
            digest.as_mut_ptr(),
            &mut size,
        );
        (ok != 0 && size as usize == digest.len()).then_some(digest)
    }

    unsafe fn publisher_ekus_match(cert: *const CERT_CONTEXT, identity: &str) -> bool {
        const CODE_SIGNING: &[u8] = b"1.3.6.1.5.5.7.3.3";
        if identity.is_empty() || identity.as_bytes() == CODE_SIGNING || identity == "2.5.29.37.0" {
            return false;
        }
        let flags = CERT_FIND_EXT_ONLY_ENHKEY_USAGE_FLAG;
        let mut size = 0;
        if CertGetEnhancedKeyUsage(cert, flags, null_mut(), &mut size) == 0
            || !(std::mem::size_of::<CTL_USAGE>() as u32..=65_536).contains(&size)
        {
            return false;
        }
        let mut buffer = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
        let usage = buffer.as_mut_ptr().cast::<CTL_USAGE>();
        if CertGetEnhancedKeyUsage(cert, flags, usage, &mut size) == 0
            || (*usage).cUsageIdentifier == 0
            || (*usage).rgpszUsageIdentifier.is_null()
        {
            return false;
        }
        let ids = std::slice::from_raw_parts(
            (*usage).rgpszUsageIdentifier,
            (*usage).cUsageIdentifier as usize,
        );
        let mut code_signing = false;
        let mut publisher = false;
        for &id in ids {
            if id.is_null() {
                return false;
            }
            let oid = CStr::from_ptr(id.cast()).to_bytes();
            code_signing |= oid == CODE_SIGNING;
            publisher |= oid == identity.as_bytes();
        }
        code_signing && publisher
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use super::{AuthenticodeSigner, SignatureError, SigningIdentity};
    use crate::host_identity::HostProcess;
    use std::path::Path;

    pub(super) fn authenticode(
        _path: &Path,
        _expected: Option<&str>,
    ) -> Result<AuthenticodeSigner, SignatureError> {
        Err(SignatureError::Unsupported(
            "Authenticode is a Windows signature",
        ))
    }

    /// No signing identity exists on this platform: every peer is unverifiable.
    pub(super) fn check(
        _host: &HostProcess,
        _signers: &[SigningIdentity],
    ) -> Result<(), SignatureError> {
        Err(SignatureError::Unsupported(
            "no peer code-signature verification on this platform",
        ))
    }
}
