//! Protected external trust. Port of the trust-loading half of `ownership-approval.mjs`:
//! the platform-owned trust file, its location audit (POSIX owner/mode/ACL, Windows
//! owner/ACE audit through PowerShell), and enrolment detection.
//!
//! This module only reads. Nothing here writes a trust file or an exception record.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fancy_regex::Regex;
use serde_json::{json, Value};

use crate::approval::{key_id_for_public_key, validate_trust_registry, REQUIRED_HOSTS};
use crate::error::{Res, ScanError};
use crate::gitio;
use crate::json::{field, is_hex64, items, str_of, truthy};
use crate::keyauth::AuthorityEnv;
use crate::keylog::{parse_public_key, PublicKey};
use crate::scan::TrustLoaderFn;

const WINDOWS_TRUSTED_SIDS: [&str; 2] = ["S-1-5-18", "S-1-5-32-544"];
const TRUSTED_INSTALLER: &str = "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";
const WINDOWS_READ_MASK: u64 = 0x0012_00a9;
const WINDOWS_ANCESTOR_CREATE_MASK: u64 = 0x0000_0116;
const WINDOWS_GENERIC_WRITE: u64 = 0x4000_0000;
const WINDOWS_GENERIC_ALL: u64 = 0x1000_0000;

/// Canonical trust file for this platform.
pub fn trusted_path() -> Res<PathBuf> {
    if cfg!(windows) {
        Ok(PathBuf::from(
            r"C:\ProgramData\Orthic Labs\RightKit\ownership\trust.json",
        ))
    } else if cfg!(target_os = "macos") {
        Ok(PathBuf::from(
            "/Library/Application Support/Orthic Labs/RightKit/ownership/trust.json",
        ))
    } else {
        Err(ScanError::new(format!(
            "ownership approval unsupported platform: {}",
            std::env::consts::OS
        )))
    }
}

fn sid_ok(value: &str) -> bool {
    value.strip_prefix("S-1-").is_some_and(|rest| {
        let parts: Vec<&str> = rest.split('-').collect();
        parts.len() >= 2
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// Evaluate one native Windows security descriptor using numeric SIDs and masks.
pub fn evaluate_windows_acl(info: &Value, target: &str, require_non_elevated: bool) -> Vec<String> {
    let mut problems = Vec::new();
    let owner_sid = str_of(field(info, "ownerSID"));
    let installer_owner = owner_sid == TRUSTED_INSTALLER;
    let trusted_owner = WINDOWS_TRUSTED_SIDS.contains(&owner_sid) || installer_owner;
    if !sid_ok(owner_sid) || !trusted_owner {
        problems.push(
            "owner is not SYSTEM, Administrators, or the native TrustedInstaller service owner"
                .into(),
        );
    }
    if str_of(field(info, "attributes"))
        .to_lowercase()
        .contains("reparsepoint")
    {
        problems.push("path object is a reparse point".into());
    }
    if require_non_elevated && field(info, "elevated").as_bool() != Some(false) {
        problems.push("current process is elevated or elevation state is unknown".into());
    }
    let normalized = target
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase();
    let b = normalized.as_bytes();
    let is_drive_root = b.len() == 2 && b[0].is_ascii_lowercase() && b[1] == b':';
    let is_program_data =
        b.len() == 12 && b[0].is_ascii_lowercase() && &normalized[1..] == ":\\programdata";
    let permit_creation = (is_drive_root || is_program_data)
        && field(info, "directory").as_bool() == Some(true)
        && field(info, "nonempty").as_bool() == Some(true);
    if (is_drive_root || is_program_data) && !permit_creation {
        problems.push("system ancestor must be an existing nonempty directory before create rights are accepted".into());
    }
    let access = field(info, "access").as_array();
    if access.is_none_or(|a| a.is_empty()) {
        problems.push("ACL access entries are missing, empty, or malformed".into());
    }
    for ace in access.map(Vec::as_slice).unwrap_or(&[]) {
        let sid = str_of(field(ace, "identitySID"));
        let mask = field(ace, "mask").as_i64();
        let propagation = field(ace, "propagationFlags").as_i64();
        let kind = str_of(field(ace, "type"));
        let valid = sid_ok(sid)
            && mask.is_some_and(|m| (-2_147_483_648..=4_294_967_295).contains(&m))
            && propagation.is_some_and(|p| (0..=3).contains(&p))
            && (kind == "Allow" || kind == "Deny");
        if !valid {
            problems.push("ACL contains a malformed ACE".into());
            continue;
        }
        if propagation.unwrap_or(0) & 2 != 0 || kind != "Allow" {
            continue;
        }
        if WINDOWS_TRUSTED_SIDS.contains(&sid) || (installer_owner && sid == owner_sid) {
            continue;
        }
        let unsigned = (mask.unwrap_or(0) as u32) as u64;
        if unsigned & (WINDOWS_GENERIC_WRITE | WINDOWS_GENERIC_ALL) != 0 {
            problems.push(format!(
                "untrusted principal {sid} has generic write or all access"
            ));
            continue;
        }
        let allowed = if permit_creation {
            WINDOWS_READ_MASK | WINDOWS_ANCESTOR_CREATE_MASK
        } else {
            WINDOWS_READ_MASK
        };
        if unsigned & !allowed != 0 {
            problems.push(format!("untrusted principal {sid} has write, delete, control, or unknown access mask 0x{unsigned:x}"));
        }
    }
    problems
}

const WINDOWS_ACL_SCRIPT: &str = "$ErrorActionPreference='Stop'; $a=Get-Acl -LiteralPath $env:RK_TRUST_CHECK_TARGET; $item=Get-Item -Force -LiteralPath $env:RK_TRUST_CHECK_TARGET; $id=[Security.Principal.WindowsIdentity]::GetCurrent(); $p=New-Object Security.Principal.WindowsPrincipal($id); $ownerSid=$a.GetOwner([Security.Principal.SecurityIdentifier]).Value; $ownerName=$a.Owner; $out=[ordered]@{ownerSID=$ownerSid;ownerName=$ownerName;attributes=$item.Attributes.ToString();directory=[bool]$item.PSIsContainer;nonempty=(!$item.PSIsContainer -or [System.IO.Directory]::EnumerateFileSystemEntries($item.FullName).GetEnumerator().MoveNext());elevated=$p.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator);access=@($a.Access | ForEach-Object { [ordered]@{identitySID=$_.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value;type=$_.AccessControlType.ToString();mask=[int64]$_.FileSystemRights.value__;propagationFlags=[int]$_.PropagationFlags} })}; $out | ConvertTo-Json -Compress -Depth 5";

/// Audits a protected path: every ancestor and the path itself must be owned and
/// writable only by trusted principals. Returns the problems found.
pub fn protected_location(file: &Path, require_non_elevated: bool) -> Vec<String> {
    if cfg!(windows) {
        windows_location(file, require_non_elevated)
    } else {
        posix_location(file)
    }
}

fn windows_location(file: &Path, require_non_elevated: bool) -> Vec<String> {
    let mut problems = Vec::new();
    let mut targets: Vec<PathBuf> = Vec::new();
    let mut current = file.parent().map(Path::to_path_buf);
    while let Some(dir) = current {
        targets.push(dir.clone());
        current = dir.parent().map(Path::to_path_buf);
    }
    targets.push(file.to_path_buf());
    for target in targets {
        let shown = target.to_string_lossy().into_owned();
        match std::fs::symlink_metadata(&target) {
            Ok(meta) if meta.file_type().is_symlink() => {
                problems.push(format!("trust path contains a reparse/symlink: {shown}"))
            }
            Ok(_) => {}
            Err(_) => {
                problems.push(format!("cannot inspect trust path: {shown}"));
                continue;
            }
        }
        let mut env: Vec<(String, String)> = std::env::vars()
            .filter(|(k, _)| !k.eq_ignore_ascii_case("psmodulepath"))
            .collect();
        env.push(("RK_TRUST_CHECK_TARGET".into(), shown.clone()));
        let args = [
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            WINDOWS_ACL_SCRIPT,
        ];
        let output = gitio::run(
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
            &args,
            Some(&env),
            None,
            Duration::from_secs(60),
            4 * 1024 * 1024,
        );
        let Ok(output) = output.and_then(|o| {
            if o.success() {
                Ok(o)
            } else {
                Err(gitio::SpawnError { code: "EXIT" })
            }
        }) else {
            problems.push(format!("cannot inspect trust ACL: {shown}"));
            continue;
        };
        match serde_json::from_str::<Value>(output.text().trim()) {
            Ok(acl) => problems.extend(
                evaluate_windows_acl(&acl, &shown, require_non_elevated)
                    .into_iter()
                    .map(|p| format!("{p}: {shown}")),
            ),
            Err(_) => problems.push(format!("malformed ACL result: {shown}")),
        }
    }
    problems
}

#[cfg(unix)]
fn posix_location(file: &Path) -> Vec<String> {
    use std::os::unix::fs::MetadataExt;
    let write_re = Regex::new(r"(?i)\b(?:writesecurity|chown|write|delete|append|writeattr|writeextattr|delete_child|add_file|add_subdirectory)\b").expect("static regex");
    let root_re = Regex::new(r"(?i)^\s*\d+:\s+user:root\s+allow\b").expect("static regex");
    let mut problems = Vec::new();
    let mut current = file.to_path_buf();
    loop {
        let shown = current.to_string_lossy().into_owned();
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                problems.push(format!("trust path contains a symlink: {shown}"))
            }
            Ok(_) => {}
            Err(_) => problems.push(format!("cannot inspect trust path: {shown}")),
        }
        match std::fs::metadata(&current) {
            Ok(st) => {
                if st.uid() != 0 || st.mode() & 0o022 != 0 {
                    problems.push(format!("trust location is not root-owned/private: {shown}"));
                }
            }
            Err(_) => problems.push(format!("cannot inspect trust location: {shown}")),
        }
        let acl = gitio::run(
            "/bin/ls",
            &["-lde", shown.as_str()],
            None,
            None,
            Duration::from_secs(10),
            1024 * 1024,
        );
        match acl {
            Ok(out) if out.success() => {
                for line in out
                    .text()
                    .split('\n')
                    .map(|l| l.trim_end_matches('\r'))
                    .skip(1)
                {
                    if !line.to_lowercase().contains("allow")
                        || !write_re.is_match(line).unwrap_or(false)
                    {
                        continue;
                    }
                    if !root_re.is_match(line).unwrap_or(false) {
                        problems.push(format!(
                            "trust ACL grants write authority to a non-root principal: {shown}"
                        ));
                    }
                }
            }
            _ => problems.push(format!("cannot inspect trust ACL: {shown}")),
        }
        match current.parent().map(Path::to_path_buf) {
            Some(parent) if parent != current && current != Path::new("/") => current = parent,
            _ => break,
        }
    }
    problems
}

#[cfg(not(unix))]
fn posix_location(_file: &Path) -> Vec<String> {
    vec!["POSIX trust audit is unavailable on this platform".into()]
}

fn normalize_id(id: &Value) -> String {
    id.as_str()
        .map(|s| {
            let lower = s.to_lowercase();
            lower
                .strip_prefix("win-")
                .or_else(|| lower.strip_prefix("mac-"))
                .unwrap_or(&lower)
                .to_string()
        })
        .unwrap_or_default()
}

/// Port of `loadTrustedExternal`: reads and validates the platform trust file. Returns
/// `{enrolled, pending, protection, trust, path}`.
pub fn load_trusted_external() -> Res<Value> {
    let file = trusted_path()?;
    if !file.exists() {
        return Ok(
            json!({"enrolled": false, "pending": true, "protection": {"ok": false, "reason": "external trust file is absent"}, "trust": null, "path": file.to_string_lossy()}),
        );
    }
    let meta = std::fs::symlink_metadata(&file).map_err(|e| ScanError::new(e.to_string()))?;
    if meta.file_type().is_symlink() {
        return Err(ScanError::new("ownership trust file may not be a symlink"));
    }
    if !std::fs::metadata(&file)
        .map_err(|e| ScanError::new(e.to_string()))?
        .is_file()
    {
        return Err(ScanError::new(
            "ownership trust file must be a regular protected file",
        ));
    }
    let problems = protected_location(&file, true);
    if !problems.is_empty() {
        return Err(ScanError::new(problems.join("; ")));
    }
    let text = std::fs::read_to_string(&file).map_err(|e| ScanError::new(e.to_string()))?;
    let trust: Value = serde_json::from_str(&text).map_err(|e| ScanError::new(e.to_string()))?;
    if field(&trust, "schema").as_f64() != Some(1.0) {
        return Err(ScanError::new("ownership trust file schema is unsupported"));
    }
    let check = validate_trust_registry(&trust);
    if !check.is_empty() {
        return Err(ScanError::new(check.join("; ")));
    }
    let revoked: HashSet<String> = items(field(&trust, "revokedKeyIds"))
        .iter()
        .map(normalize_id)
        .collect();
    for host in REQUIRED_HOSTS {
        let entry = field(field(&trust, "hosts"), host);
        if !truthy(field(entry, "keyId")) && !truthy(field(entry, "publicKeyPem")) {
            continue;
        }
        let pem = str_of(field(entry, "publicKeyPem"));
        let derived = key_id_for_public_key(host, pem)
            .ok()
            .map(|d| normalize_id(&json!(d)));
        if !truthy(field(entry, "keyId"))
            || !truthy(field(entry, "publicKeyPem"))
            || derived != Some(normalize_id(field(entry, "keyId")))
        {
            return Err(ScanError::new(format!(
                "invalid or mismatched {host} approval key enrollment"
            )));
        }
        if !matches!(parse_public_key(pem), Some(PublicKey::Ec(_))) {
            return Err(ScanError::new(format!("{host} approval key must be P-256")));
        }
    }
    let host_keys: Vec<&Value> = REQUIRED_HOSTS
        .iter()
        .map(|h| field(field(&trust, "hosts"), h))
        .collect();
    let enrolled = if truthy(field(&trust, "rootFingerprint")) {
        let checkpoint = field(&trust, "keyCheckpoint");
        is_hex64(str_of(field(&trust, "rootFingerprint")))
            && truthy(checkpoint)
            && crate::json::safe_ordinal(field(checkpoint, "sequence"))
            && is_hex64(str_of(field(checkpoint, "head")))
    } else {
        host_keys.iter().all(|h| {
            truthy(field(h, "keyId"))
                && truthy(field(h, "publicKeyPem"))
                && !revoked.contains(&normalize_id(field(h, "keyId")))
        }) && normalize_id(field(host_keys[0], "keyId"))
            != normalize_id(field(host_keys[1], "keyId"))
            && field(host_keys[0], "publicKeyPem") != field(host_keys[1], "publicKeyPem")
    };
    let mechanism = if cfg!(windows) {
        "PowerShell ACL owner/ACE audit"
    } else {
        "owner/mode/ACL audit"
    };
    Ok(
        json!({"enrolled": enrolled, "pending": !enrolled, "protection": {"ok": true, "mechanism": mechanism}, "trust": trust, "path": file.to_string_lossy()}),
    )
}

/// A trust loader closure over [`load_trusted_external`].
pub fn production_trust_loader() -> TrustLoaderFn {
    std::sync::Arc::new(load_trusted_external)
}

/// Checkpoint location and audit for key-authority reads on this host.
pub fn production_authority_env() -> AuthorityEnv {
    AuthorityEnv {
        trust_file: trusted_path().ok(),
        audit: Box::new(|path| protected_location(path, true)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_acl_accepts_trusted_owner_and_read_only_users() {
        let info = json!({"ownerSID": "S-1-5-18", "attributes": "Directory", "elevated": false, "directory": true, "nonempty": true,
            "access": [{"identitySID": "S-1-5-32-545", "type": "Allow", "mask": 0x0012_00a9u32, "propagationFlags": 0}]});
        assert!(evaluate_windows_acl(&info, r"C:\ProgramData\Orthic Labs", true).is_empty());
    }

    #[test]
    fn windows_acl_refuses_user_owner_write_and_elevation() {
        let info = json!({"ownerSID": "S-1-5-21-1-2-3-1001", "attributes": "Directory", "elevated": true,
            "access": [{"identitySID": "S-1-5-32-545", "type": "Allow", "mask": 0x4000_0000u32, "propagationFlags": 0}]});
        let problems = evaluate_windows_acl(&info, r"C:\x", true);
        assert_eq!(problems.len(), 3, "{problems:?}");
    }
}
