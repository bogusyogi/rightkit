//! Linked computers: how this computer reaches another one's `<cli>` over ssh.
//!
//! A link is `{device, ssh, pulse, shell, deviceId?, chatKey?}`: the name chats are shown under
//! (`"<chat> on <device>"`), the ssh destination (a host alias from
//! `~/.ssh/config`, or `user@host`), the path of `<cli>` on that computer, the
//! family of shell ssh lands in there (`posix` or `powershell`, found when the link
//! is made), the peer's stable device id and its chat public key (both learned from its chat
//! listing, which this computer read over its own ssh connection to that host, and then
//! pinned: a listing that reports another id or key is refused, see `poll`). The key is what
//! the origin of a message posted here is checked against (`envelope` module docs): ssh
//! itself does not tell `<cli> chat post` which computer ran it. Links live in
//! `<state>/chat/links.json` and are made with
//! `<cli> chat link <device> <ssh-host> [--cli PATH]`.
//!
//! Three remote commands exist: `<cli> chat peers --local --json` (that
//! computer's chats), `<cli> chat post <base64 envelope>` (deliver one
//! message there) and `<cli> chat said --local <chat> --json` (what the user typed in one
//! chat there). The remote command is one string, every word quoted for the
//! remote shell family, and the destination follows `--` so it cannot be read as
//! an ssh option. ssh must already work without a prompt (keys, BatchMode);
//! nothing here sets ssh up. A release build runs the system ssh at its fixed path
//! (`/usr/bin/ssh`, or `System32\OpenSSH\ssh.exe` on Windows), never one found on the
//! search path. Only a build with the `test-overrides` feature reads `RIGHTKIT_CHAT_SSH`,
//! which names another program to run instead (tests); it gets the destination and the
//! words as separate arguments, unquoted. Each poll of a link is recorded in `links-status.json`, and the
//! last listing a link answered with is kept in `links-listings.json` for 24 hours, so its
//! chats stay listed (as "unreachable") while the computer cannot be reached.

use super::envelope::{Envelope, now_ms};
use super::receipts::Update;
use super::roster::RosterEntry;
use super::store::Store;
use rightkit_child_process::RunOptions;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// How long one remote command may take, ssh connection included.
///
/// A loaded linked computer answers slowly (#438: a Mac at load 12 took 7 s to list peers and
/// more than 25 s to take a post), so the wait covers a slow remote that does answer. A link
/// that is down fails at ssh's own 10 s connect timeout, long before this.
pub const REMOTE_TIMEOUT: Duration = Duration::from_secs(60);
/// Most output kept from one remote command, per stream.
const OUTPUT_CAP: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub device: String,
    pub ssh: String,
    /// The app's command there; the brand's command name means on PATH. Stored under `<cli>` in
    /// `links.json`, a name existing files already use.
    #[serde(default = "default_cli", rename = "pulse", alias = "cli")]
    pub cli: String,
    /// The shell ssh lands in there: "posix" (default) or "powershell".
    #[serde(default = "default_shell")]
    pub shell: String,
    /// The peer's stable device id, learned from its chat listing and pinned.
    #[serde(default, rename = "deviceId", skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    /// The peer's chat public key (Ed25519, hex), learned from its chat listing and pinned
    /// like `device_id`. Envelopes posted here in that computer's name are believed only when
    /// signed with it (`chat::origin_of`). `None` for a peer whose the host app reports no key.
    #[serde(default, rename = "chatKey", skip_serializing_if = "Option::is_none")]
    pub chat_key: Option<String>,
}

impl Default for Link {
    fn default() -> Link {
        Link {
            device: String::new(),
            ssh: String::new(),
            cli: default_cli(),
            shell: default_shell(),
            device_id: None,
            chat_key: None,
        }
    }
}

/// What a linked computer's chat listing says about the computer itself: its stable device
/// id and its chat public key, each `None` when it reports none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeerIds {
    pub device_id: Option<String>,
    /// The chat device id the computer reported in its listing, for a device link too (where
    /// `device_id` stays the link's own id): what pairs a device link with its ssh twin
    /// (`resolved`, `remember_identity`).
    pub chat_device_id: Option<String>,
    pub chat_key: Option<String>,
    /// The computer's `claudeAccountNames` (`claude_names`), merged into this computer's own
    /// once the listing has passed the device-id and chat-key checks.
    pub claude_names: Option<Value>,
}

fn default_cli() -> String {
    super::brand::brand().cli.clone()
}

fn default_shell() -> String {
    "posix".to_string()
}

/// What `<cli> chat peers --local --json` prints on a linked computer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RemoteListing {
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub chats: Vec<RosterEntry>,
}

/// A linked computer's chats, or why they could not be listed.
#[derive(Debug, Clone)]
pub struct RemoteChats {
    pub link: Link,
    pub listing: Result<RemoteListing, String>,
    /// When `listing` failed: the last good listing of this computer, if one is kept and
    /// not older than `LISTING_TTL_MS`. Its chats are listed as unreachable.
    pub cached: Option<CachedChats>,
}

/// The last listing a linked computer answered with, kept while it is unreachable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedChats {
    pub listing: RemoteListing,
    /// When that listing was taken (ms since the epoch).
    pub last_seen_ms: u64,
    /// The computer's device id then, when it reported one; a listing from another
    /// computer under the same name is not used.
    #[serde(default)]
    pub device_id: Option<String>,
}

/// How long a computer's last listing is kept for when it cannot be reached: 24 hours.
pub const LISTING_TTL_MS: u64 = 24 * 60 * 60 * 1000;
const LISTINGS_FILE: &str = "links-listings.json";

/// How the last polls of a linked computer went.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkStatus {
    #[serde(default)]
    pub device: String,
    /// When a poll last succeeded (ms); none when it never has.
    #[serde(default)]
    pub last_ok_ms: Option<u64>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_error_ms: Option<u64>,
    /// When the link was last polled (ms); 0 when never.
    #[serde(default)]
    pub last_attempt_ms: u64,
}

impl LinkStatus {
    /// Whether the most recent poll succeeded.
    pub fn online(&self) -> bool {
        match (self.last_ok_ms, self.last_error_ms) {
            (Some(ok), Some(err)) => ok >= err,
            (Some(_), None) => true,
            _ => false,
        }
    }

    /// How long ago (ms) the link last answered, when it ever did.
    pub fn age_ms(&self, now: u64) -> Option<u64> {
        self.last_ok_ms.map(|t| now.saturating_sub(t))
    }
}

/// Test builds only (`test-overrides`): the program `RIGHTKIT_CHAT_SSH` names. Not compiled
/// into a release build, so the variable cannot choose what a release build runs.
#[cfg(feature = "test-overrides")]
fn ssh_override() -> Option<String> {
    std::env::var("RIGHTKIT_CHAT_SSH")
        .ok()
        .filter(|v| !v.trim().is_empty())
}

#[cfg(not(feature = "test-overrides"))]
fn ssh_override() -> Option<String> {
    None
}

/// The system ssh at its fixed location, never found through the search path.
fn system_ssh() -> std::path::PathBuf {
    #[cfg(windows)]
    {
        let root = std::env::var_os("SystemRoot")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"));
        root.join(r"System32\OpenSSH\ssh.exe")
    }
    #[cfg(not(windows))]
    {
        std::path::PathBuf::from("/usr/bin/ssh")
    }
}

/// An ssh destination is a host alias or `user@host`: never an option, never a command.
fn validate_ssh(ssh: &str) -> Result<(), String> {
    let ok = !ssh.is_empty()
        && !ssh.starts_with('-')
        && ssh.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '@' | '.' | '_' | '-' | ':' | '[' | ']' | '+' | '~')
        });
    if ok {
        Ok(())
    } else {
        Err("the ssh host must be a host alias or user@host (no spaces, ';' or leading '-')".into())
    }
}

fn validate_cli(cli: &str) -> Result<(), String> {
    if cli.trim().is_empty() || cli.contains(['\n', '\r', '\0']) {
        Err("the command path is empty or has a line break".to_string())
    } else {
        Ok(())
    }
}

/// `text` as one POSIX shell word.
fn posix_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// `text` as one PowerShell single-quoted string.
fn powershell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// The one thing PowerShell may expand: the installed-app candidate below.
fn powershell_cli(cli: &str) -> String {
    if cli == cli_candidates()[2] {
        format!("& \"{cli}\"")
    } else {
        format!("& {}", powershell_quote(cli))
    }
}

/// `<cli> chat <args…>` as one command string for the remote shell family.
fn remote_command(link: &Link, args: &[&str]) -> String {
    let powershell = link.shell == "powershell";
    let mut command = if powershell {
        powershell_cli(&link.cli)
    } else {
        posix_quote(&link.cli)
    };
    command.push_str(" chat");
    for arg in args {
        command.push(' ');
        command.push_str(&if powershell {
            powershell_quote(arg)
        } else {
            posix_quote(arg)
        });
    }
    command
}

/// Why a remote command failed. A timeout is kept apart: the remote may have run the command
/// (and accepted a post) even though nothing came back.
enum RunFailure {
    TimedOut(String),
    Other(String),
}

impl RunFailure {
    fn into_message(self) -> String {
        match self {
            RunFailure::TimedOut(m) | RunFailure::Other(m) => m,
        }
    }
}

/// Run `<cli> chat <args…>` on the linked computer; stdout on exit 0.
fn run(link: &Link, args: &[&str], timeout: Duration) -> Result<String, String> {
    run_checked(link, args, timeout).map_err(RunFailure::into_message)
}

fn run_checked(link: &Link, args: &[&str], timeout: Duration) -> Result<String, RunFailure> {
    validate_ssh(&link.ssh).map_err(RunFailure::Other)?;
    validate_cli(&link.cli).map_err(RunFailure::Other)?;
    let override_program = ssh_override();
    let program: std::ffi::OsString = match &override_program {
        Some(program) => program.into(),
        None => system_ssh().into(),
    };
    let mut command_args: Vec<std::ffi::OsString> = Vec::new();
    if override_program.is_none() {
        command_args.extend(
            [
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "LogLevel=ERROR",
                // Never a pty. The child runs with stdin on NUL; Windows ssh.exe takes NUL for a
                // console, wants a tty, and then never exits after the remote command ends
                // (#541: every poll from the CLI ran to the 60 s timeout).
                "-o",
                "RequestTTY=no",
                "--",
            ]
            .map(std::ffi::OsString::from),
        );
        command_args.push((&link.ssh).into());
        command_args.push(remote_command(link, args).into());
    } else {
        command_args.push((&link.ssh).into());
        command_args.push((&link.cli).into());
        command_args.push("chat".into());
        command_args.extend(args.iter().map(|a| std::ffi::OsString::from(*a)));
    }
    let options = RunOptions::new(timeout)
        .stdout_cap(OUTPUT_CAP)
        .stderr_cap(OUTPUT_CAP);
    let done = match rightkit_child_process::run(&program, &command_args, &options) {
        Ok(done) if done.timed_out => {
            return Err(RunFailure::TimedOut(format!(
                "{} did not answer within {} s",
                link.device,
                timeout.as_secs()
            )));
        }
        Ok(done) => done,
        Err(e) => return Err(RunFailure::Other(format!("couldn't run ssh: {e}"))),
    };
    let status = done.status;
    let out = String::from_utf8_lossy(&done.stdout.bytes).into_owned();
    let err = String::from_utf8_lossy(&done.stderr.bytes).into_owned();
    if status.success() {
        Ok(out)
    } else {
        let detail = err.trim();
        let detail = if detail.is_empty() {
            out.trim().to_string()
        } else {
            detail.to_string()
        };
        Err(RunFailure::Other(format!(
            "{} answered {}: {}",
            link.device,
            status.code().unwrap_or(-1),
            detail
        )))
    }
}

/// Decode what `b64` made (plain base64, `=` padding), and what a person pasting it may have
/// changed: white space and line breaks, the URL-safe alphabet, missing padding, text after the
/// first `=`, a dangling character that holds fewer than 8 bits, and unused trailing bits that are
/// not zero. The text is made canonical here and the decoding is `rightkit_crypto::base64_decode`
/// (the strict owner), the same normalisation Ember's depth reader uses.
pub fn unb64(text: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut compact: String = text
        .chars()
        .filter(|c| !matches!(c, ' ' | '\t' | '\r' | '\n'))
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            other => other,
        })
        .collect();
    // Anything after the first `=` is padding and is ignored.
    if let Some(end) = compact.find('=') {
        compact.truncate(end);
    }
    let mut bytes = compact.into_bytes();
    // A last character holding fewer than 8 bits carries nothing; two or three characters leave 4
    // or 2 unused bits to clear.
    let unused = match bytes.len() % 4 {
        1 => {
            bytes.pop();
            0
        }
        2 => 0x0f,
        3 => 0x03,
        _ => 0,
    };
    if let Some(last) = bytes.last_mut().filter(|_| unused != 0)
        && let Some(index) = ALPHABET.iter().position(|letter| letter == last)
    {
        *last = ALPHABET[index & !unused];
    }
    while !bytes.len().is_multiple_of(4) {
        bytes.push(b'=');
    }
    let canonical = String::from_utf8(bytes).ok()?;
    rightkit_crypto::base64_decode(&canonical).ok()
}

/// The envelope as the `post` argument.
pub fn encode_envelope(env: &Envelope) -> String {
    rightkit_crypto::base64_encode(env.to_json().as_bytes())
}

/// An envelope from outside this computer's own processes (`<cli> chat post`, the mesh): the
/// device-link marks (`mesh`, `mesh_owner`) are cleared, only `mesh::serve` sets them.
pub fn decode_envelope(text: &str) -> Result<Envelope, String> {
    let mut env = decode_envelope_local(text)?;
    env.mesh.clear();
    env.mesh_owner = false;
    env.mesh_role = "computer".to_string();
    Ok(env)
}

/// An envelope this computer's own CLI handed the hub, or the hub's control files: keeps the
/// device-link marks `mesh::serve` stamped.
pub fn decode_envelope_local(text: &str) -> Result<Envelope, String> {
    let bytes = unb64(text.trim()).ok_or_else(|| "not base64".to_string())?;
    let json = String::from_utf8(bytes).map_err(|_| "not UTF-8".to_string())?;
    Envelope::from_json(&json).map_err(|e| e.to_string())
}

/// This computer's links: the paired computers reached over the device link (`mesh`) and the
/// saved ssh links. A computer that is both is one link, the device link, with its ssh link kept
/// as the fallback (`twin_of`).
pub fn all(store: &Store) -> Vec<Link> {
    resolved(store).into_iter().map(|(link, _)| link).collect()
}

/// The links with the ssh link each device link falls back to. A device link and an ssh link are
/// the same computer when the chat device id the device link's computer reported (kept in
/// `links-identity.json`, never expired) is the id the ssh link pinned, which is stable across
/// renames and the 24 h listing cache. Until that id is known they are the same computer
/// when they share a name (ignoring case) or the ssh link's computer reports the device link's
/// name for itself. Sorted by name, as saved links are.
fn resolved(store: &Store) -> Vec<(Link, Option<Link>)> {
    let ssh = store.links();
    let meshed = super::mesh::links();
    if meshed.is_empty() {
        return ssh.into_iter().map(|link| (link, None)).collect();
    }
    let mut taken = vec![false; ssh.len()];
    let mut out: Vec<(Link, Option<Link>)> = Vec::new();
    for link in meshed {
        let wanted = same_computer_key(&link.device);
        let chat_id = identity_of(store, &link).and_then(|i| i.chat_device_id);
        let by_id = chat_id.as_deref().and_then(|id| {
            ssh.iter().enumerate().position(|(i, saved)| {
                !taken[i] && ssh_chat_id(store, saved).as_deref() == Some(id)
            })
        });
        let twin = by_id.or_else(|| {
            ssh.iter().enumerate().position(|(i, saved)| {
                !taken[i]
                    && (same_computer_key(&saved.device) == wanted
                        || reported_name(store, saved)
                            .is_some_and(|n| same_computer_key(&n) == wanted))
            })
        });
        let twin = twin.map(|i| {
            taken[i] = true;
            ssh[i].clone()
        });
        out.push((link, twin));
    }
    for (i, link) in ssh.into_iter().enumerate() {
        if !taken[i] {
            out.push((link, None));
        }
    }
    out.sort_by_key(|(link, _)| link.device.to_lowercase());
    out
}

/// The chat device id an ssh link's computer has: the one pinned in `links.json`, else the one
/// its last listing reported.
fn ssh_chat_id(store: &Store, ssh: &Link) -> Option<String> {
    ssh.device_id
        .clone()
        .or_else(|| identity_of(store, ssh).and_then(|i| i.chat_device_id))
}

/// A computer's name as both links know it: the device link names the Mac by its host name
/// ("Alices-Mac-Mini.local"), an ssh link by what the owner typed or the computer reports
/// ("the owner's Mac Mini"). Letters and digits only, lower case, without `.local`.
pub(super) fn same_computer_key(name: &str) -> String {
    let name = name.trim();
    let name = name
        .strip_suffix(".local")
        .or_else(|| name.strip_suffix(".LOCAL"))
        .unwrap_or(name);
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// The ssh link named `device` and the chat device id its computer is known by (pinned, or from
/// its last listing), or `None` when it is not an ssh link or no identity is known (R17).
pub(super) fn ssh_link_identity(store: &Store, device: &str) -> Option<(Link, String)> {
    let link = all(store).into_iter().find(|l| {
        super::mesh::mesh_id(l).is_none()
            && same_computer_key(&l.device) == same_computer_key(device)
    })?;
    let id = ssh_chat_id(store, &link)?;
    Some((link, id))
}

/// The ssh link a device link falls back to, when the same computer is also linked over ssh.
fn twin_of(store: &Store, link: &Link) -> Option<Link> {
    super::mesh::mesh_id(link)?;
    resolved(store)
        .into_iter()
        .find(|(l, _)| l.device == link.device)
        .and_then(|(_, twin)| twin)
}

/// The link a computer is named by: the link's own name, else the name the linked computer
/// reports for itself (its host name: "BOGUS-DELL" for a link made as "Dell"), and the latter
/// only when exactly one link reports it. The ssh name of a computer that is also on the
/// device link names the device link.
pub fn find(store: &Store, device: &str) -> Option<Link> {
    let wanted = device.trim().to_lowercase();
    let all = resolved(store);
    if let Some((link, _)) = all.iter().find(|(l, _)| l.device.to_lowercase() == wanted) {
        return Some(link.clone());
    }
    if let Some((link, _)) = all.iter().find(|(_, twin)| {
        twin.as_ref()
            .is_some_and(|t| t.device.to_lowercase() == wanted)
    }) {
        return Some(link.clone());
    }
    let mut reporting = all
        .into_iter()
        .map(|(l, _)| l)
        .filter(|l| reported_name(store, l).is_some_and(|name| name.to_lowercase() == wanted));
    let first = reporting.next()?;
    reporting.next().is_none().then_some(first)
}

/// The link whose peer reported this stable device id (a device link's own id, or the chat
/// device id its ssh twin pinned).
pub fn find_by_device_id(store: &Store, device_id: &str) -> Option<Link> {
    resolved(store)
        .into_iter()
        .find(|(l, twin)| {
            l.device_id.as_deref() == Some(device_id)
                || identity_of(store, l)
                    .and_then(|i| i.chat_device_id)
                    .as_deref()
                    == Some(device_id)
                || twin
                    .as_ref()
                    .is_some_and(|t| t.device_id.as_deref() == Some(device_id))
        })
        .map(|(link, _)| link)
}

/// Where `<cli>` lives when the link does not say: PATH, then the installed app on a Mac,
/// then the installed app on Windows (PowerShell expands `$env:`; ssh to Windows lands in
/// PowerShell by default).
fn cli_candidates() -> [String; 3] {
    let brand = super::brand::brand();
    [
        brand.cli.clone(),
        brand.cli_mac_path.clone(),
        brand.cli_windows_path.clone(),
    ]
}

/// Add or replace a link, then check it by listing that computer's chats. The remote
/// shell family is found by trying POSIX quoting first, then PowerShell, and the one
/// that answered is stored; with no explicit `<cli>` path, so is the first candidate
/// that answers. The peer's device id and chat key, when it reports them, are pinned.
pub fn add(store: &Store, mut link: Link) -> Result<RemoteListing, String> {
    if link.device.trim().is_empty() || link.ssh.trim().is_empty() {
        return Err("a link needs a device name and an ssh host".to_string());
    }
    if link.device.contains(':') {
        return Err("the device name can't contain ':'".to_string());
    }
    validate_ssh(&link.ssh)?;
    validate_cli(&link.cli)?;
    link.device_id = None;
    link.chat_key = None;
    let explicit = link.cli != super::brand::brand().cli;
    let first_shell = if link.shell == "powershell" {
        "powershell"
    } else {
        "posix"
    };
    let other_shell = if first_shell == "posix" {
        "powershell"
    } else {
        "posix"
    };
    // The test hook ignores quoting, so one shell family is enough there.
    let shells: Vec<&str> = if ssh_override().is_none() {
        vec![first_shell, other_shell]
    } else {
        vec![first_shell]
    };
    let requested = link.cli.clone();
    let known = cli_candidates();
    let mut found = None;
    let mut first_error = None;
    'search: for shell in shells {
        link.shell = shell.to_string();
        let candidates: Vec<&str> = if explicit {
            vec![requested.as_str()]
        } else if shell == "powershell" {
            vec![known[0].as_str(), known[2].as_str()]
        } else {
            vec![known[0].as_str(), known[1].as_str()]
        };
        for candidate in candidates {
            link.cli = candidate.to_string();
            match list_chats_identified(&link, None) {
                Ok(answer) => {
                    found = Some(answer);
                    break 'search;
                }
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
    }
    let (listing, ids) = found.ok_or_else(|| {
        let last = first_error.unwrap_or_default();
        if explicit {
            last
        } else {
            format!("{last} (pass the command's path for {})", link.device)
        }
    })?;
    link.device_id = ids.device_id;
    link.chat_key = ids.chat_key;
    let mut links: Vec<Link> = store
        .links()
        .into_iter()
        .filter(|l| !l.device.eq_ignore_ascii_case(&link.device))
        .collect();
    links.push(link);
    links.sort_by_key(|a| a.device.to_lowercase());
    store.save_links(&links).map_err(|e| e.to_string())?;
    Ok(listing)
}

pub fn remove(store: &Store, device: &str) -> Result<bool, String> {
    let before = store.links();
    let after: Vec<Link> = before
        .iter()
        .filter(|l| !l.device.eq_ignore_ascii_case(device.trim()))
        .cloned()
        .collect();
    let removed = after.len() != before.len();
    if removed {
        store.save_links(&after).map_err(|e| e.to_string())?;
    }
    Ok(removed)
}

fn last_json_line(out: &str) -> Option<&str> {
    out.lines().rev().find(|l| l.trim_start().starts_with('{'))
}

/// The chats open on a linked computer.
pub fn list_chats(link: &Link) -> Result<RemoteListing, String> {
    list_chats_identified(link, None).map(|(listing, _)| listing)
}

/// What is left of a `status`/`peers` listing's deadline; an error once it has passed, so no
/// further route (the ssh twin, the other shell family) is tried past the bound.
fn listing_wait(deadline: Instant) -> Result<Duration, String> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left < Duration::from_millis(500) {
        return Err("the listing wait ran out".to_string());
    }
    Ok(left)
}

thread_local! {
    /// Whether this thread's last `list_chats_identified` over a device link failed with a
    /// failure that may not fall back to ssh (`MeshFailure::may_fall_back`).
    static MESH_REFUSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The chats open on a linked computer and what it reported about itself: its stable device
/// id (`"deviceId"` in the listing) and its chat public key (`"chatKey"`, 64 hex characters;
/// anything else is ignored as no key).
///
/// `deadline` is `None` for a call that decides a send (target resolution, discovery): it keeps
/// the full device-link wait (`CHAT_CALL_WAIT`) and its route. `Some` is a `status`/`peers`
/// listing: what is left of the deadline bounds the call, over the device link and over ssh (C-D4).
pub fn list_chats_identified(
    link: &Link,
    deadline: Option<Instant>,
) -> Result<(RemoteListing, PeerIds), String> {
    let meshed = super::mesh::mesh_id(link).is_some();
    MESH_REFUSED.with(|flag| flag.set(false));
    let value: Value = if meshed {
        // The device link proves who answered (the mesh's authenticated stream): no id or chat
        // key is pinned from the listing, and the link's own id stands for the computer.
        let answer = match deadline {
            None => super::mesh::call(link, super::mesh::PEERS, serde_json::json!({})),
            Some(deadline) => super::mesh::call_within(
                link,
                super::mesh::PEERS,
                serde_json::json!({}),
                listing_wait(deadline)?,
            ),
        };
        answer.map_err(|e| {
            // A refusal is terminal: the ssh twin is never asked after it (R05). A listing that
            // got no answer in time (`may_have_arrived`) is not a refusal and changes nothing on
            // the other computer, so the ssh twin may be asked for the same read. A
            // send with no answer is retried once over ssh with the same id (C-D19, `post_traced`).
            // Only a deadline-bound listing may be answered by the ssh twin after no answer in time;
            // a send-deciding call (`deadline` None) keeps the old rule (C-D4).
            MESH_REFUSED.with(|flag| {
                flag.set(!e.may_fall_back() && !(deadline.is_some() && e.may_have_arrived))
            });
            e.to_string()
        })?
    } else {
        let wait = match deadline {
            None => REMOTE_TIMEOUT,
            Some(deadline) => listing_wait(deadline)?,
        };
        let out = run(link, &["peers", "--local", "--json"], wait)?;
        let line =
            last_json_line(&out).ok_or_else(|| format!("{} printed no chat list", link.device))?;
        serde_json::from_str(line)
            .map_err(|e| format!("{} printed something else: {e}", link.device))?
    };
    let device_id = if meshed {
        link.device_id.clone()
    } else {
        value["deviceId"]
            .as_str()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
    };
    let chat_device_id = value["deviceId"]
        .as_str()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    let chat_key = value["chatKey"]
        .as_str()
        .map(|key| key.trim().to_ascii_lowercase())
        .filter(|key| key.len() == 64 && key.chars().all(|c| c.is_ascii_hexdigit()))
        .filter(|_| !meshed);
    let claude_names = value
        .get("claudeAccountNames")
        .filter(|names| names.is_object())
        .cloned();
    let listing = serde_json::from_value(value)
        .map_err(|e| format!("{} printed something else: {e}", link.device))?;
    Ok((
        listing,
        PeerIds {
            device_id,
            chat_device_id,
            chat_key,
            claude_names,
        },
    ))
}

const STATUS_FILE: &str = "links-status.json";

/// An unchanged poll result is not rewritten until its timestamp is this old, so a link that
/// keeps answering costs a write every few minutes instead of every poll.
const STATUS_REFRESH_MS: u64 = 5 * 60 * 1000;

fn record_status(store: &Store, device: &str, error: Option<&str>) {
    let now = now_ms();
    let recorded: Option<BTreeMap<String, LinkStatus>> = store.read_state(STATUS_FILE);
    if let Some(entry) = recorded
        .as_ref()
        .and_then(|all| all.get(&device.to_lowercase()))
    {
        let unchanged = entry.device == device
            && match error {
                None => {
                    entry.last_error.is_none()
                        && entry
                            .last_ok_ms
                            .is_some_and(|t| now.saturating_sub(t) < STATUS_REFRESH_MS)
                }
                Some(text) => {
                    entry.last_error.as_deref() == Some(text)
                        && entry
                            .last_error_ms
                            .is_some_and(|t| now.saturating_sub(t) < STATUS_REFRESH_MS)
                }
            };
        if unchanged {
            return;
        }
    }
    let _ = store.update_state(STATUS_FILE, |all: &mut BTreeMap<String, LinkStatus>| {
        let entry = all.entry(device.to_lowercase()).or_default();
        entry.device = device.to_string();
        entry.last_attempt_ms = now;
        match error {
            None => {
                entry.last_ok_ms = Some(now);
                entry.last_error = None;
            }
            Some(text) => {
                entry.last_error = Some(text.to_string());
                entry.last_error_ms = Some(now);
            }
        }
    });
}

/// Poll one link: list its chats, pin or check its device id, record how it went.
/// Whether a remote answer means the command reached the wrong shell family: PowerShell
/// choking on POSIX quoting, or a POSIX shell / cmd on PowerShell syntax.
fn wrong_shell(error: &str) -> bool {
    error.contains("ParserError")
        || error.contains("is not recognized as")
        || error.contains("syntax error")
        || error.contains("command not found")
}

/// Poll `link`; when the answer says the stored shell family is wrong (a link made
/// before families were recorded, or a computer whose login shell changed), try the
/// other family once and keep it when it answers.
fn list_with_shell_fallback(
    store: &Store,
    link: &mut Link,
    deadline: Option<Instant>,
) -> Result<(RemoteListing, PeerIds), String> {
    match list_chats_identified(link, deadline) {
        Err(error)
            if wrong_shell(&error)
                && ssh_override().is_none()
                && super::mesh::mesh_id(link).is_none() =>
        {
            let other = if link.shell == "powershell" {
                "posix"
            } else {
                "powershell"
            };
            let mut retry = link.clone();
            retry.shell = other.to_string();
            match list_chats_identified(&retry, deadline) {
                Ok(answer) => {
                    let device = link.device.clone();
                    let shell = other.to_string();
                    let _ = store.update_state("links.json", move |links: &mut Vec<Link>| {
                        for l in links.iter_mut() {
                            if l.device.eq_ignore_ascii_case(&device) {
                                l.shell = shell.clone();
                            }
                        }
                    });
                    link.shell = other.to_string();
                    Ok(answer)
                }
                Err(_) => Err(error),
            }
        }
        other => other,
    }
}

/// Keep `listing` as this computer's last good one (and drop kept listings past their TTL).
fn remember_listing(store: &Store, link: &Link, listing: &RemoteListing) {
    let now = now_ms();
    let device = link.device.to_lowercase();
    let cache = CachedChats {
        listing: listing.clone(),
        last_seen_ms: now,
        device_id: link.device_id.clone(),
    };
    let _ = store.update_state(
        LISTINGS_FILE,
        move |all: &mut BTreeMap<String, CachedChats>| {
            all.retain(|_, c| now.saturating_sub(c.last_seen_ms) <= LISTING_TTL_MS);
            all.insert(device, cache);
        },
    );
}

/// What a linked computer reported about itself, kept without an expiry (`links-identity.json`):
/// the listing cache drops entries after 24 hours, and the pairing of a device link with its ssh
/// link and the name each answers to must not.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PeerIdentity {
    /// The name the computer gave itself in its listing.
    #[serde(default)]
    name: String,
    /// Its chat device id (the listing's `deviceId`).
    #[serde(default)]
    chat_device_id: Option<String>,
    #[serde(default)]
    seen_ms: u64,
}

const IDENTITY_FILE: &str = "links-identity.json";

/// The key of a link's identity: a device link by its id, an ssh link by its name.
fn identity_key(link: &Link) -> String {
    match super::mesh::mesh_id(link) {
        Some(_) => link.ssh.clone(),
        None => format!("ssh:{}", link.device.to_lowercase()),
    }
}

fn identity_of(store: &Store, link: &Link) -> Option<PeerIdentity> {
    let all: BTreeMap<String, PeerIdentity> = store.read_state(IDENTITY_FILE)?;
    all.get(&identity_key(link)).cloned()
}

/// Keep what `link`'s computer reported in `listing` (and `chat_id`), when it reported anything.
fn remember_identity(store: &Store, link: &Link, listing: &RemoteListing, chat_id: Option<String>) {
    let name = listing.device.trim().to_string();
    if name.is_empty() && chat_id.is_none() {
        return;
    }
    let key = identity_key(link);
    let now = now_ms();
    // A listing that says what is already kept costs no write (a poll runs on every command).
    if let Some(kept) = identity_of(store, link)
        && (name.is_empty() || kept.name == name)
        && (chat_id.is_none() || kept.chat_device_id == chat_id)
    {
        return;
    }
    let _ = store.update_state(
        IDENTITY_FILE,
        move |all: &mut BTreeMap<String, PeerIdentity>| {
            let entry = all.entry(key).or_default();
            if !name.is_empty() {
                entry.name = name;
            }
            if chat_id.is_some() {
                entry.chat_device_id = chat_id;
            }
            entry.seen_ms = now;
        },
    );
}

/// The name a linked computer last reported for itself in its chat listing (its own
/// computer name, the host name unless it was given another). Kept without an expiry in
/// `links-identity.json` (the listing cache's own copy is only the fallback for a computer read
/// before that file existed): the name changes rarely. `None` until a listing has been read.
pub fn reported_name(store: &Store, link: &Link) -> Option<String> {
    if let Some(identity) = identity_of(store, link)
        && !identity.name.is_empty()
    {
        return Some(identity.name);
    }
    let all: BTreeMap<String, CachedChats> = store.read_state(LISTINGS_FILE).unwrap_or_default();
    all.get(&link.device.to_lowercase())
        .map(|cached| cached.listing.device.trim().to_string())
        .filter(|name| !name.is_empty())
}

/// Claude sessions that a linked computer lists live, from the last good listing of each linked
/// computer (the kept listing, not older than `LISTING_TTL_MS`). A Claude Desktop chat whose
/// process runs on one of those computers is listed there; this computer's own non-live row for it
/// is stale. Read from the kept file only: it asks no computer.
pub fn live_sessions_elsewhere(store: &Store) -> std::collections::BTreeSet<String> {
    let now = now_ms();
    let all: BTreeMap<String, CachedChats> = store.read_state(LISTINGS_FILE).unwrap_or_default();
    all.values()
        .filter(|cached| now.saturating_sub(cached.last_seen_ms) <= LISTING_TTL_MS)
        .flat_map(|cached| cached.listing.chats.iter())
        .filter(|chat| chat.kind == "claude" && chat.liveness == "live")
        .map(|chat| chat.session.clone())
        .collect()
}

/// The kept listing of this computer, when it is not expired and is the same computer.
fn cached_listing(store: &Store, link: &Link) -> Option<CachedChats> {
    let all: BTreeMap<String, CachedChats> = store.read_state(LISTINGS_FILE).unwrap_or_default();
    let cached = all.get(&link.device.to_lowercase())?;
    let fresh = now_ms().saturating_sub(cached.last_seen_ms) <= LISTING_TTL_MS;
    let same_computer = match (&link.device_id, &cached.device_id) {
        (Some(pinned), Some(seen)) => pinned == seen,
        _ => true,
    };
    (fresh && same_computer).then(|| cached.clone())
}

/// Poll `link`; a device link that cannot be asked falls back to the ssh link of the same
/// computer, and the answer then carries that ssh link, so what follows (a send, a `said`) goes
/// the way the listing came.
///
/// `deadline` is `Some` for `status`/`peers` (C-D4): the whole poll, the twin included, ends by
/// then. `None` keeps the full waits (`CHAT_CALL_WAIT`, `REMOTE_TIMEOUT`).
fn poll(store: &Store, link: Link, twin: Option<Link>, deadline: Option<Instant>) -> RemoteChats {
    let first = poll_link(store, link, deadline);
    match twin {
        Some(twin) if first.listing.is_err() && !MESH_REFUSED.with(std::cell::Cell::get) => {
            if deadline.is_some_and(|d| listing_wait(d).is_err()) {
                super::sendlog::event(
                    rightkit_logs::Level::Info,
                    "chat.listing.twin_skipped",
                    serde_json::json!({"reason": "deadline"}),
                );
                return first;
            }
            let fallback = poll_link(store, twin, deadline);
            if fallback.listing.is_ok() {
                fallback
            } else {
                first
            }
        }
        _ => first,
    }
}

fn poll_link(store: &Store, mut link: Link, deadline: Option<Instant>) -> RemoteChats {
    let pinned = link.device_id.clone();
    let pinned_key = link.chat_key.clone();
    let polled = list_with_shell_fallback(store, &mut link, deadline);
    let changed = |what: &str| {
        format!(
            "{} is not the computer this link was made with ({what} changed); \
             link it again if it was reinstalled",
            link.device
        )
    };
    let listing = polled.and_then(|(listing, seen)| {
        let id = match (pinned, seen.device_id) {
            (Some(pinned), Some(seen)) if pinned != seen => return Err(changed("device id")),
            (None, Some(seen)) => Some(seen),
            _ => None,
        };
        // A pinned key is never replaced from a listing: only a new link pins another one.
        let key = match (pinned_key, seen.chat_key) {
            (Some(pinned), Some(seen)) if pinned != seen => return Err(changed("chat key")),
            (None, Some(seen)) => Some(seen),
            _ => None,
        };
        // The other computer's account names fold into this one's: the newest rename wins.
        if let Some(names) = &seen.claude_names {
            rightkit_ai_accounts::names::merge_remote(names);
        }
        Ok((listing, id, key, seen.chat_device_id))
    });
    let listing = listing.map(|(listing, learned, learned_key, chat_id)| {
        remember_identity(store, &link, &listing, chat_id);
        if learned.is_some() || learned_key.is_some() {
            let device = link.device.clone();
            let id = learned.clone();
            let key = learned_key.clone();
            let _ = store.update_state("links.json", move |links: &mut Vec<Link>| {
                for l in links.iter_mut() {
                    if !l.device.eq_ignore_ascii_case(&device) {
                        continue;
                    }
                    if l.device_id.is_none() {
                        l.device_id = id.clone();
                    }
                    if l.chat_key.is_none() {
                        l.chat_key = key.clone();
                    }
                }
            });
        }
        if let Some(id) = learned {
            link.device_id = Some(id);
        }
        if let Some(key) = learned_key {
            link.chat_key = Some(key);
        }
        listing
    });
    record_status(
        store,
        &link.device,
        listing.as_ref().err().map(String::as_str),
    );
    let cached = match &listing {
        Ok(listing) => {
            remember_listing(store, &link, listing);
            None
        }
        Err(_) => cached_listing(store, &link),
    };
    RemoteChats {
        link,
        listing,
        cached,
    }
}

/// The chats open on `link`. A device link that cannot list them (the engine busy, the path down)
/// is asked again over the ssh link of the same computer, when there is one; the error is
/// the device link's when both fail.
pub fn list_chats_or_twin(store: &Store, link: &Link) -> Result<RemoteListing, String> {
    match list_chats(link) {
        Ok(listing) => Ok(listing),
        Err(first) if MESH_REFUSED.with(std::cell::Cell::get) => Err(first),
        Err(first) => match twin_of(store, link) {
            Some(twin) => list_chats(&twin).map_err(|second| format!("{first}; ssh: {second}")),
            None => Err(first),
        },
    }
}

/// One linked computer's chats (`None` when no such link), asked now.
pub fn list_chats_for(store: &Store, device: &str) -> Option<RemoteChats> {
    find(store, device).map(|link| {
        let twin = twin_of(store, &link);
        poll(store, link, twin, None)
    })
}

/// Every link's chats, asked in parallel. A link that cannot be asked carries its last good
/// listing in `cached` (see `CachedChats`).
pub fn list_all(store: &Store) -> Vec<RemoteChats> {
    list_all_within(store, None)
}

/// [`list_all`] for `status` and `peers`: with `Some(wait)` every link's poll, ssh twin included,
/// ends within `wait` (C-D4). Sends never use it.
pub fn list_all_within(store: &Store, wait: Option<Duration>) -> Vec<RemoteChats> {
    let deadline = wait.map(|w| Instant::now() + w);
    let handles: Vec<_> = resolved(store)
        .into_iter()
        .map(|(link, twin)| {
            let store = store.clone();
            std::thread::spawn(move || poll(&store, link, twin, deadline))
        })
        .collect();
    handles.into_iter().filter_map(|h| h.join().ok()).collect()
}

/// How each link's last poll went: a link that is down shows as offline, with when
/// it last answered, instead of being missing. Links never polled report no success.
pub fn link_status(store: &Store) -> Vec<LinkStatus> {
    let recorded: BTreeMap<String, LinkStatus> = store.read_state(STATUS_FILE).unwrap_or_default();
    all(store)
        .into_iter()
        .map(|link| {
            recorded
                .get(&link.device.to_lowercase())
                .cloned()
                .unwrap_or_else(|| LinkStatus {
                    device: link.device.clone(),
                    ..LinkStatus::default()
                })
        })
        .collect()
}

/// Deliver `env` to the chat it names on the linked computer. The result is
/// the receipt `<cli> chat post` printed there (`{status, detail}`). `Err` means the post
/// was not sent (ssh could not run or connect, or the remote refused to run the command). Once
/// the command was started there and no receipt came back (a timeout, a cut connection, output
/// that is not a receipt) the remote may have accepted the message, so the answer is an
/// `unknown` receipt, never an error that reads as "not sent".
pub fn post(link: &Link, env: &Envelope) -> Result<Value, String> {
    post_traced(link, env).0
}

/// How a post travelled, for the `chat.send` log line (`sendlog`).
#[derive(Debug, Clone, Default)]
pub struct PostTrace {
    /// "device link" or "ssh": the transport that produced the answer (or the last one tried).
    pub transport: &'static str,
    /// The device link failed with nothing delivered and the post went over the ssh twin.
    pub fallback: bool,
    /// The device link's failure code, when it failed (whether or not ssh then took over).
    pub mesh_error: Option<String>,
}

/// `post`, with how it travelled.
pub fn post_traced(link: &Link, env: &Envelope) -> (Result<Value, String>, PostTrace) {
    if super::mesh::mesh_id(link).is_none() {
        let trace = PostTrace {
            transport: "ssh",
            ..PostTrace::default()
        };
        if let Ok(store) = Store::open_default() {
            super::receipts::note_outbound(&store, env, link);
        }
        return (post_ssh(link, env), trace);
    }
    let mut trace = PostTrace {
        transport: "device link",
        ..PostTrace::default()
    };
    // Over the device link first. A transport-unreachable failure that certainly delivered
    // nothing (no dial, no engine, a call that ran out of time before it wrote) falls back to
    // ssh; a refusal is terminal; a request that may have arrived is retried once (below).
    if let Ok(store) = Store::open_default() {
        super::receipts::note_outbound(&store, env, link);
    }
    let params = serde_json::json!({ "envelope": encode_envelope(env) });
    match super::mesh::call(link, super::mesh::DELIVER, params) {
        Ok(receipt) => (Ok(receipt), trace),
        // A request that may have arrived (the engine did not answer, C-D19): it is asked again
        // once over the ssh twin with the SAME envelope id. The receiving computer keeps a
        // per-chat delivery record by id on disk (`Store::begin_delivery`), checked on the
        // device-link receive and on `chat post` alike, so the message reaches the chat at most
        // once whichever attempt arrived first. Anything but a definite receipt stays `unknown`.
        // A refusal that may have arrived (the redial was refused after a first attempt was
        // written) is terminal for the ssh twin (R05): `unknown`, never retried over ssh.
        Err(failure)
            if failure.may_have_arrived
                && !SAME_ID_RETRY_CODES.contains(&failure.code.as_str()) =>
        {
            trace.mesh_error = Some(failure.code.clone());
            (
                Ok(serde_json::json!({
                    "status": "unknown",
                    "detail": format!(
                        "{failure}; the message may have been delivered. Check `{cli} chat inbox` or ask \
                         the chat on {} before resending (a resend is a new message)",
                        link.device,
                        cli = super::brand::brand().cli
                    ),
                })),
                trace,
            )
        }
        Err(failure) if failure.may_have_arrived => {
            trace.mesh_error = Some(failure.code.clone());
            let unknown = |extra: &str| {
                serde_json::json!({
                    "status": "unknown",
                    "detail": format!(
                        "{failure}; the message may have been delivered{extra}. Check `{cli} chat inbox` or ask \
                         the chat on {} before resending (a resend is a new message)",
                        link.device,
                        cli = super::brand::brand().cli
                    ),
                })
            };
            let twin = Store::open_default().ok().and_then(|store| {
                twin_of(&store, link).map(|twin| {
                    let sealed = super::seal(&store, &twin, env.clone());
                    (twin, sealed)
                })
            });
            let Some((twin, sealed)) = twin else {
                return (Ok(unknown("")), trace);
            };
            trace.transport = "ssh";
            trace.fallback = true;
            let answer = match post_ssh(&twin, &sealed) {
                Ok(receipt) => {
                    match super::ReceiptState::parse(receipt["status"].as_str().unwrap_or("")) {
                        // A receipt that says the message is in the chat (or its inbox or queue)
                        // is definite; the receiver's record answers a repeat with the first
                        // attempt's receipt, so it is the same message either way.
                        Some(
                            super::ReceiptState::Delivered
                            | super::ReceiptState::Queued
                            | super::ReceiptState::Held
                            | super::ReceiptState::Sent,
                        ) => receipt,
                        // `unknown`, or a refusal that cannot rule out the first attempt: the
                        // outcome stays `unknown` (no third attempt).
                        _ => unknown("; the ssh retry with the same id gave no definite answer"),
                    }
                }
                Err(_) => unknown("; the ssh retry with the same id did not run"),
            };
            (Ok(answer), trace)
        }
        Err(failure) if !failure.may_fall_back() => {
            // A refusal (authorization, revoked, expired lease, unknown method, policy,
            // unsupported version) is terminal: the ssh twin is a separate, weaker credential and
            // never a way around it (R05). `fallback` stays false and the code is recorded.
            trace.mesh_error = Some(failure.code.clone());
            (
                Err(format!("{} (device link): {failure}", link.device)),
                trace,
            )
        }
        Err(failure) => {
            trace.mesh_error = Some(failure.code.clone());
            match Store::open_default().ok().and_then(|store| {
                twin_of(&store, link).map(|twin| {
                    // The ssh link's own receiver id and chat key sign the envelope.
                    let sealed = super::seal(&store, &twin, env.clone());
                    (twin, sealed)
                })
            }) {
                Some((twin, sealed)) => {
                    trace.transport = "ssh";
                    trace.fallback = true;
                    (post_ssh(&twin, &sealed), trace)
                }
                None => (
                    Err(format!("{} (device link): {failure}", link.device)),
                    trace,
                ),
            }
        }
    }
}

/// The device-link failures that mean the engine did not answer (C-D19): only these are asked
/// again over the ssh twin with the same id. A refusal is an answer and never goes to ssh (R05).
const SAME_ID_RETRY_CODES: [&str; 4] = [
    "request_failed",
    "answer_timeout",
    "engine_unknown",
    "engine_unresponsive",
];

fn post_ssh(link: &Link, env: &Envelope) -> Result<Value, String> {
    let encoded = encode_envelope(env);
    let unknown = |why: String| {
        serde_json::json!({
            "status": "unknown",
            "detail": format!(
                "{why}; the message may have been delivered. Check `{cli} chat inbox` or ask the \
                 chat on {} before resending (a resend is a new message)",
                link.device,
                cli = super::brand::brand().cli
            ),
        })
    };
    let out = match run_checked(link, &["post", &encoded], REMOTE_TIMEOUT) {
        Ok(out) => out,
        Err(RunFailure::TimedOut(why)) => return Ok(unknown(why)),
        Err(RunFailure::Other(why)) => return Err(why),
    };
    let Some(line) = last_json_line(&out) else {
        return Ok(unknown(format!("{} printed no receipt", link.device)));
    };
    match serde_json::from_str(line) {
        Ok(receipt) => Ok(receipt),
        Err(e) => Ok(unknown(format!(
            "{} printed something else: {e}",
            link.device
        ))),
    }
}

/// Post one receipt update to the sending computer over `link`: `<cli> chat receipt-post
/// <base64 json>`. Ok when that computer stored it.
pub fn post_receipt(link: &Link, update: &Update) -> Result<(), String> {
    let json = serde_json::to_string(update).map_err(|e| e.to_string())?;
    let encoded = rightkit_crypto::base64_encode(json.as_bytes());
    let value: Value = if super::mesh::mesh_id(link).is_some() {
        let params = serde_json::json!({ "update": encoded });
        match super::mesh::call(link, super::mesh::RECEIPT, params) {
            Ok(value) => value,
            Err(failure) => {
                if !failure.may_fall_back() {
                    return Err(format!("{} (device link): {failure}", link.device));
                }
                return match Store::open_default()
                    .ok()
                    .and_then(|store| twin_of(&store, link))
                {
                    Some(twin) => post_receipt(&twin, update),
                    None => Err(format!("{} (device link): {failure}", link.device)),
                };
            }
        }
    } else {
        let store = Store::open_default().map_err(|e| e.to_string())?;
        let signed = super::receipts::SignedUpdate::seal(&store, link, update)?;
        let bytes = serde_json::to_vec(&signed).map_err(|e| e.to_string())?;
        let encoded = rightkit_crypto::base64_encode(&bytes);
        let out = run(link, &["receipt-post", &encoded], REMOTE_TIMEOUT)?;
        let line =
            last_json_line(&out).ok_or_else(|| format!("{} printed no answer", link.device))?;
        serde_json::from_str(line)
            .map_err(|e| format!("{} printed something else: {e}", link.device))?
    };
    if value["ok"].as_bool() == Some(true) {
        Ok(())
    } else {
        let code = value["code"].as_str().unwrap_or("refused");
        let why = value["error"].as_str().unwrap_or("");
        Err(format!(
            "{} did not store the receipt ({code}): {why}",
            link.device
        ))
    }
}

/// What the user typed in one chat on the linked computer: runs
/// `<cli> chat said --local <target> --limit <n> --json` there (`target` is
/// `<kind>:<session-id>` or a bare session id) and returns its one JSON line
/// (`{ok, messages, ...}` or `{ok: false, refused, error}`).
pub fn said(link: &Link, target: &str, limit: usize) -> Result<Value, String> {
    if super::mesh::mesh_id(link).is_some() {
        let params = serde_json::json!({ "target": target, "limit": limit });
        return match super::mesh::call(link, super::mesh::SAID, params) {
            Ok(value) => Ok(value),
            Err(failure) if !failure.may_fall_back() => {
                Err(format!("{} (device link): {failure}", link.device))
            }
            Err(failure) => match Store::open_default()
                .ok()
                .and_then(|store| twin_of(&store, link))
            {
                Some(twin) => said(&twin, target, limit),
                None => Err(format!("{} (device link): {failure}", link.device)),
            },
        };
    }
    let limit = limit.to_string();
    let out = run(
        link,
        &["said", "--local", target, "--limit", &limit, "--json"],
        REMOTE_TIMEOUT,
    )?;
    let line = last_json_line(&out).ok_or_else(|| format!("{} printed no answer", link.device))?;
    serde_json::from_str(line).map_err(|e| format!("{} printed something else: {e}", link.device))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for text in ["", "a", "ab", "abc", "hello world", "ünïcödé ✓"] {
            let encoded = rightkit_crypto::base64_encode(text.as_bytes());
            assert_eq!(unb64(&encoded).unwrap(), text.as_bytes(), "{text}");
        }
        assert!(unb64("!!").is_none());
    }
}
