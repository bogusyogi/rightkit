//! Metadata-only IPC to the installed app that owns its OS credential namespaces.
use super::*;
use crate::control::token;
use rightkit_child_process::{OwnedChild, OwnedCommand};
use rightkit_daemon::{Client, ClientConfig, HostExpectation, Suite};
use serde_json::json;
use std::process::Stdio;
use std::time::Instant;

const OWNER_SCHEMA: &str = "rightkit.qa.credential-owner/1";
const OWNER_SWITCH: &str = "--rightkit-qa-credentials";
const OWNER_MODE: &str = "credentials";
const TOKEN_FILE: &str = "owner-control.token";
const OWNER_STDERR_FILE: &str = "owner-stderr.log";
const OWNER_SERVICE_PREFIX: &str = "qa-credentials-";

/// A fresh owner service name the daemon host accepts: the prefix and 32 lowercase hex characters of a new id (47 bytes, within
/// rightkit_daemon::MAX_SERVICE_NAME_LEN = 48). The hyphenated id made 51 bytes and every host refused it (hr-r2-ultra-2, owner stderr:
/// "the control service name is missing or not a qa-credentials name").
fn owner_service_name() -> String {
    let id: String = new_id()
        .chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_lowercase())
        .take(32)
        .collect();
    format!("{OWNER_SERVICE_PREFIX}{id}")
}
/// At most this much of the owner's stderr is quoted into a failure (its own refusal line; the file keeps the rest).
const OWNER_STDERR_QUOTE: usize = 400;

/// `message`, with the owner's own first stderr bytes appended when it wrote any (the file stays in the private snapshot directory).
fn with_owner_stderr(message: String, file: &Path) -> String {
    let text = fs::read(file).unwrap_or_default();
    let text = String::from_utf8_lossy(&text[..text.len().min(OWNER_STDERR_QUOTE)])
        .trim()
        .replace(['\n', '\r'], " | ");
    if text.is_empty() {
        message
    } else {
        format!("{message} (owner stderr: {text}; {})", file.display())
    }
}
const START_TIMEOUT: Duration = Duration::from_secs(15);
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const EXIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Installed owner identity & current-user transport retained in the schema-1 restore marker.
/// Only [`Self::for_executable`] constructs a new identity; every launch rechecks its bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnerConfig {
    executable: PathBuf,
    executable_sha256: String,
    suite_root: PathBuf,
    service: String,
    token_file: Option<PathBuf>,
    process_names: Vec<String>,
}

impl OwnerConfig {
    pub fn for_executable(executable: &Path) -> Result<Self> {
        if !executable.is_absolute() {
            return err("credential owner executable must be absolute");
        }
        let executable = ctx(
            crate::util::canonical_plain(executable),
            "cannot resolve credential owner",
            executable,
        )?;
        let md = ctx(
            fs::symlink_metadata(&executable),
            "cannot stat credential owner",
            &executable,
        )?;
        refuse_reparse(&executable, &md)?;
        if !md.is_file() || md.file_type().is_symlink() {
            return err("credential owner executable is not a regular file");
        }
        let executable_sha256 = ctx(
            rightkit_fs::digest::sha256_file(&executable),
            "cannot hash credential owner",
            &executable,
        )?;
        let suite = Suite::per_user_default()
            .map_err(|_| Error("cannot resolve current-user credential owner transport".into()))?;
        Ok(Self {
            executable,
            executable_sha256,
            suite_root: suite.root().to_path_buf(),
            service: owner_service_name(),
            token_file: None,
            process_names: Vec::new(),
        })
    }

    pub(super) fn for_snapshot(mut self, dir: &Path, process_names: &[String]) -> Result<Self> {
        self.verify()?;
        self.token_file = Some(dir.join(TOKEN_FILE));
        self.process_names = process_names.to_vec();
        Ok(self)
    }

    pub(super) fn verify(&self) -> Result<()> {
        if !self.executable.is_absolute()
            || ctx(
                crate::util::canonical_plain(&self.executable),
                "cannot resolve credential owner",
                &self.executable,
            )? != self.executable
        {
            return err("credential owner path changed; restore marker retained");
        }
        let md = ctx(
            fs::symlink_metadata(&self.executable),
            "cannot stat credential owner",
            &self.executable,
        )?;
        refuse_reparse(&self.executable, &md)?;
        if !md.is_file()
            || md.file_type().is_symlink()
            || ctx(
                rightkit_fs::digest::sha256_file(&self.executable),
                "cannot hash credential owner",
                &self.executable,
            )? != self.executable_sha256
        {
            return err("credential owner executable changed; restore marker retained");
        }
        let suite = Suite::per_user_default()
            .map_err(|_| Error("cannot resolve current-user credential owner transport".into()))?;
        if self.suite_root != suite.root()
            // The binding rule stays the one older markers were written under: crash recovery must still read and discard a `snapshotting`
            // marker whose name an earlier tool made (51 bytes); only a name this tool creates must pass the host's own rule.
            || !self.service.starts_with(OWNER_SERVICE_PREFIX)
            || !self
                .service
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return err("credential owner transport differs from its current-user binding");
        }
        Ok(())
    }
}

/// Validate the exact snapshot layout, including every existing ancestor. No symlink aliases.
pub(super) fn verify_location(marker: &Marker) -> Result<()> {
    if !single_component(&marker.app) || !single_component(&marker.snapshot_id) {
        return err("credential owner snapshot app/id is invalid");
    }
    let dir = Path::new(&marker.snapshot_dir);
    if !dir.is_absolute()
        || dir
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return err("credential owner snapshot path is not absolute and normal");
    }
    if dir.file_name().and_then(|n| n.to_str()) != Some(marker.snapshot_id.as_str())
        || dir
            .parent()
            .and_then(Path::file_name)
            .and_then(|n| n.to_str())
            != Some(marker.app.as_str())
        || dir
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .and_then(|n| n.to_str())
            != Some(SNAPSHOTS_DIR)
    {
        return err("credential owner snapshot is outside its managed app layout");
    }
    let mut path = PathBuf::new();
    for component in dir.components() {
        path.push(component.as_os_str());
        // The volume prefix (`\\?\D:` on Windows) and the root are not directories this check can or needs to
        // stat: a bare prefix has no root and the OS refuses it. Every real directory below the root is checked.
        if matches!(component, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        let md = ctx(
            fs::symlink_metadata(&path),
            "cannot stat snapshot ancestor",
            &path,
        )?;
        refuse_reparse(&path, &md)?;
        if !md.is_dir() || md.file_type().is_symlink() {
            return err("credential owner snapshot ancestry is not a real directory");
        }
    }
    let owner = marker
        .credential_owner
        .as_ref()
        .ok_or_else(|| Error("credential owner binding is missing".into()))?;
    if owner.token_file.as_deref() != Some(dir.join(TOKEN_FILE).as_path())
        || owner.process_names != marker.process_names
    {
        return err("credential owner token/process binding differs from the marker");
    }
    for private in [
        dir.to_path_buf(),
        dir.parent().unwrap().to_path_buf(),
        dir.parent().unwrap().join(MARKER_FILE),
    ] {
        fs_ctx(
            rightkit_fs::private::verify_owner_only(&private),
            "credential owner snapshot is not private",
            &private,
        )?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Metadata {
    schema: String,
    operation: String,
    snapshot_id: String,
    item_count: usize,
    pub(super) credentials_sha256: String,
    verified: bool,
}

fn exchange(
    marker: &Marker,
    operation: &str,
    child: &mut OwnedChild,
    auth: &str,
) -> Result<Metadata> {
    let owner = marker.credential_owner.as_ref().unwrap();
    let suite = Suite::per_user_default()
        .map_err(|_| Error("cannot resolve credential owner transport".into()))?;
    let config = ClientConfig::new(&owner.service, "right-qa")
        .suite(suite)
        .ignore_env_overrides(true)
        .deadline(Some(Duration::from_millis(500)));
    let child_pid = child.id();
    let expected_hash = owner.executable_sha256.clone();
    let expected =
        HostExpectation::executable(owner.executable.clone()).with_verifier(move |host| {
            if host.pid != child_pid {
                return Err("credential owner is not this launch".into());
            }
            if rightkit_fs::digest::sha256_file(&host.executable)
                .ok()
                .as_deref()
                != Some(expected_hash.as_str())
            {
                return Err("credential owner executable hash differs".into());
            }
            Ok(())
        });
    let deadline = Instant::now() + START_TIMEOUT;
    let client = loop {
        if let Ok(client) = Client::connect_expecting(&config, &expected) {
            break client;
        }
        if child
            .process_handle()
            .and_then(|handle| handle.wait_exited_timeout(Duration::ZERO))
            .map_err(|_| Error("cannot observe credential owner identity".into()))?
        {
            return err("credential owner exited before authenticated health");
        }
        if Instant::now() >= deadline {
            return err("credential owner did not start within its deadline");
        }
        std::thread::sleep(Duration::from_millis(40));
    };
    client.set_deadline(Some(CALL_TIMEOUT));
    let health = client
        .call("health", token::with_token(json!({}), Some(auth)))
        .map_err(|_| Error("credential owner authenticated health failed".into()))?;
    if health.get("schema").and_then(serde_json::Value::as_str) != Some(OWNER_SCHEMA)
        || health.get("mode").and_then(serde_json::Value::as_str) != Some(OWNER_MODE)
    {
        return err("credential owner health schema/mode mismatch");
    }
    let mut params = json!({
        "schema": OWNER_SCHEMA,
        "snapshotId": marker.snapshot_id,
        "snapshotDir": marker.snapshot_dir,
        "credentials": marker.credentials,
    });
    if operation == "restore" {
        params["credentialsSha256"] = json!(marker
            .credentials_sha256
            .as_ref()
            .ok_or_else(|| Error("credential restore lacks a file digest".into()))?);
    }
    let response = client
        .call(
            &format!("qa_credentials_{operation}"),
            token::with_token(params, Some(auth)),
        )
        .map_err(|_| {
            Error(format!(
                "credential owner {operation} failed; marker retained"
            ))
        })?;
    let metadata: Metadata = serde_json::from_value(response)
        .map_err(|_| Error("credential owner returned invalid metadata".into()))?;
    if metadata.schema != OWNER_SCHEMA
        || metadata.operation != operation
        || metadata.snapshot_id != marker.snapshot_id
        || !metadata.verified
        || metadata.item_count
            != marker
                .credentials
                .iter()
                .map(|c| c.accounts.len())
                .sum::<usize>()
        || metadata.credentials_sha256.len() != 64
        || !metadata
            .credentials_sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return err("credential owner metadata does not prove this operation");
    }
    drop(client);
    // Observe natural exit before reaping, preserving ownership of any descendant identities.
    let handle = child
        .process_handle()
        .map_err(|_| Error("cannot hold credential owner process identity".into()))?;
    if !handle
        .wait_exited_timeout(EXIT_TIMEOUT)
        .map_err(|_| Error("cannot observe credential owner exit".into()))?
    {
        return err("credential owner failed to exit after its operation");
    }
    if !child
        .tree_members()
        .map_err(|_| Error("cannot verify credential owner process tree".into()))?
        .is_empty()
    {
        return err("credential owner left descendant processes");
    }
    let status = ctx(
        child.wait(),
        "cannot reap credential owner",
        &owner.executable,
    )?;
    if !status.success() {
        return err("credential owner exited unsuccessfully");
    }
    ensure_stopped(marker)?;
    owner.verify()?;
    Ok(metadata)
}

pub(super) fn operate(marker: &Marker, operation: &str) -> Result<Metadata> {
    let owner = marker
        .credential_owner
        .as_ref()
        .ok_or_else(|| Error("credential owner binding is missing".into()))?;
    owner.verify()?;
    verify_location(marker)?;
    ensure_stopped(marker)?;
    if !matches!(
        (operation, marker.state.as_str()),
        ("snapshot", STATE_SNAPSHOTTING) | ("restore", STATE_RESTORING)
    ) {
        return err("credential owner operation does not match the restore marker state");
    }
    let token_file = owner.token_file.as_ref().unwrap();
    let auth = token::write_file(token_file)?;
    // The owner's stderr goes to an owner-only file beside the token (the snapshot directory is private): the owner mode prints only its
    // own redacted refusal lines (no panic text, no payload), and a launch that ends before authenticated health is otherwise unexplained.
    let stderr_file = token_file.with_file_name(OWNER_STDERR_FILE);
    rightkit_fs::private::write_private_file(&stderr_file, b"")
        .map_err(|_| Error("cannot create credential owner stderr file".into()))?;
    let stderr = fs::OpenOptions::new()
        .append(true)
        .open(&stderr_file)
        .map_err(|_| Error("cannot open credential owner stderr file".into()))?;
    let mut command = OwnedCommand::new(&owner.executable);
    // The owner entry runs before app startup; inherit no credential-bearing environment.
    command.env_allowlist([
        "SystemRoot",
        "WINDIR",
        "TMPDIR",
        "TMP",
        "TEMP",
        "LANG",
        "LC_ALL",
    ]);
    command
        .command_mut()
        .arg(OWNER_SWITCH)
        .env("RIGHTKIT_CONTROL_SERVICE", &owner.service)
        .env(token::ENV_AUTH_FILE, token_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr));
    command.windows_hide();
    // macOS/Linux need an explicit parent-death tie; Windows already owns a kill-on-close Job Object.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    command.kill_on_parent_exit(true);
    let mut child = command
        .spawn()
        .map_err(|_| Error("cannot launch installed credential owner".into()))?;
    let result = exchange(marker, operation, &mut child, &auth);
    if result.is_err() {
        // The handle owns only this launch's process group / Job Object. Never kill by name.
        let _ = crate::process::terminate_bounded(&mut child);
    }
    let result = result.map_err(|Error(message)| Error(with_owner_stderr(message, &stderr_file)));
    let _ = fs::remove_file(token_file);
    result
}

#[cfg(test)]
mod owner_service_name_tests {
    use super::*;

    #[test]
    fn owner_service_names_are_fresh_and_hostable() {
        let a = owner_service_name();
        let b = owner_service_name();
        assert_ne!(a, b);
        for name in [&a, &b] {
            assert!(name.starts_with(OWNER_SERVICE_PREFIX), "{name}");
            assert_eq!(name.len(), OWNER_SERVICE_PREFIX.len() + 32, "{name}");
            assert!(
                name.len() <= rightkit_daemon::MAX_SERVICE_NAME_LEN,
                "{name}"
            );
            assert!(rightkit_daemon::valid_service_name(name), "{name}");
        }
    }
}
