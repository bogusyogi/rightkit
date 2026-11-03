//! Client for the RightKitAppleCredentials Swift helper (the synchronizable iCloud Keychain
//! cannot be reached from Rust). One JSON request line on stdin, one JSON response line on
//! stdout, then the helper exits.
//! Secret values appear only on the stdin pipe and in the returned [`Secret`]; they are never
//! placed in an error.
//!
//! One deadline (`timeout`, default 10 s) covers the whole exchange: writing the request,
//! reading the reply and the helper exiting. The helper is killed and reaped on every path,
//! including a write that blocks because the helper never reads its stdin.
//!
//! # Trust
//!
//! [`AppleHelper::new`] runs whatever is at the path, so it sends secrets to an unverified
//! program. Production code must build the client with [`AppleHelper::new_trusted`] and a
//! [`HelperTrust`], which checks the helper's code signature against a team requirement
//! immediately before every spawn, and resolve a nested login-item helper with
//! `resolve_bundled_login_item` (macOS).

use crate::error::{Error, Result};
use crate::secret::Secret;
use serde::Serialize;
use serde_json::{json, Value};
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use zeroize::{Zeroize, Zeroizing};

/// Response cap (and read buffer size) for one helper reply.
const RESPONSE_CAP: usize = 1 << 20;

#[derive(Serialize)]
struct WriteRequest<'a> {
    command: &'static str,
    scope: &'static str,
    account: &'a str,
    value: &'a str,
}

/// The reply to a `read` request.
struct ReadReply {
    ok: Option<bool>,
    error: Option<String>,
    /// Decoded straight from the zeroised reply line into a wiping buffer.
    value: Option<SecretBuf>,
}

/// The bytes of a decoded secret. It is allocated once at its final size and never grows, and
/// it wipes itself on every path that drops it (success, a malformed reply, a failed UTF-8
/// check). It is the ONLY buffer the decoder puts secret bytes in, so a wipe of it is a wipe of
/// every decoded copy.
///
/// Under test, `Drop` reports what it wiped to a thread-local log (see `wipe_probe`). The
/// report reads only the `len` bytes this program initialised, never the allocation at large,
/// so it makes no claim about uninitialised memory and reads none.
struct SecretBuf {
    bytes: Vec<u8>,
}

impl SecretBuf {
    fn with_capacity(capacity: usize) -> Self {
        let bytes = Vec::with_capacity(capacity);
        Self { bytes }
    }
    fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl Drop for SecretBuf {
    fn drop(&mut self) {
        // Wipe the initialised bytes in place first, so they can be inspected afterwards.
        self.bytes.as_mut_slice().zeroize();
        // Then the spare capacity, and clear.
        self.bytes.zeroize();
    }
}

/// The reply was not the expected JSON object.
#[derive(Debug)]
struct Malformed;

/// A cursor over the reply line.
///
/// `read` replies carry the secret, and `serde_json` decodes a string that contains an escape
/// (`\n`, `\"`, `é`, ...) through a scratch buffer it owns and frees without wiping. So the
/// `value` field is decoded here instead: the unescaped bytes go into one [`SecretBuf`] that
/// is allocated at its final size (an unescaped string is never longer than its source), so it
/// never reallocates and no smaller copy is left behind, and every error path drops it
/// zeroised. The rest of the object is parsed only as far as needed to find the three fields.
struct Scan<'a> {
    b: &'a [u8],
    i: usize,
}

impl Scan<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: u8) -> std::result::Result<(), Malformed> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(Malformed)
        }
    }

    fn literal(&mut self, word: &[u8]) -> std::result::Result<(), Malformed> {
        if self
            .b
            .get(self.i..)
            .is_some_and(|rest| rest.starts_with(word))
        {
            self.i += word.len();
            Ok(())
        } else {
            Err(Malformed)
        }
    }

    fn hex4(&mut self) -> std::result::Result<u32, Malformed> {
        let mut value = 0u32;
        for _ in 0..4 {
            let digit = match self.peek().ok_or(Malformed)? {
                c @ b'0'..=b'9' => c - b'0',
                c @ b'a'..=b'f' => c - b'a' + 10,
                c @ b'A'..=b'F' => c - b'A' + 10,
                _ => return Err(Malformed),
            };
            self.i += 1;
            value = (value << 4) | u32::from(digit);
        }
        Ok(value)
    }

    /// Decode the JSON string at the cursor (its opening quote is next) into `out`, with the same
    /// rules `serde_json` applies: the standard escapes, `\u` escapes with surrogate pairs,
    /// no raw control characters, no lone surrogates. `out` must have room for the rest of the
    /// input so that it never reallocates.
    fn string_into(&mut self, out: &mut Vec<u8>) -> std::result::Result<(), Malformed> {
        self.expect(b'"')?;
        loop {
            let c = self.peek().ok_or(Malformed)?;
            self.i += 1;
            match c {
                b'"' => return Ok(()),
                b'\\' => {
                    let escape = self.peek().ok_or(Malformed)?;
                    self.i += 1;
                    match escape {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0c),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let first = self.hex4()?;
                            let scalar = match first {
                                0xD800..=0xDBFF => {
                                    if self.b.get(self.i) != Some(&b'\\')
                                        || self.b.get(self.i + 1) != Some(&b'u')
                                    {
                                        return Err(Malformed);
                                    }
                                    self.i += 2;
                                    let second = self.hex4()?;
                                    if !(0xDC00..=0xDFFF).contains(&second) {
                                        return Err(Malformed);
                                    }
                                    0x10000 + (((first - 0xD800) << 10) | (second - 0xDC00))
                                }
                                0xDC00..=0xDFFF => return Err(Malformed),
                                other => other,
                            };
                            let ch = char::from_u32(scalar).ok_or(Malformed)?;
                            let mut utf8 = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut utf8).as_bytes());
                            utf8.zeroize();
                        }
                        _ => return Err(Malformed),
                    }
                }
                0x00..=0x1f => return Err(Malformed),
                _ => out.push(c),
            }
        }
    }

    /// A string that is not a secret (a key, an error message).
    fn plain_string(&mut self) -> std::result::Result<String, Malformed> {
        let mut out = Vec::new();
        self.string_into(&mut out)?;
        String::from_utf8(out).map_err(|_| Malformed)
    }

    /// The `value` string, or `null`.
    fn secret_or_null(&mut self) -> std::result::Result<Option<SecretBuf>, Malformed> {
        if self.peek() == Some(b'n') {
            self.literal(b"null")?;
            return Ok(None);
        }
        // Room for the rest of the input, so decoding never reallocates.
        let mut out = SecretBuf::with_capacity(self.b.len().saturating_sub(self.i));
        self.string_into(&mut out.bytes)?;
        // Validated in place; `out` wipes itself on this error and on every earlier one.
        std::str::from_utf8(out.as_bytes()).map_err(|_| Malformed)?;
        Ok(Some(out))
    }

    fn string_or_null(&mut self) -> std::result::Result<Option<String>, Malformed> {
        if self.peek() == Some(b'n') {
            self.literal(b"null")?;
            return Ok(None);
        }
        self.plain_string().map(Some)
    }

    fn bool_or_null(&mut self) -> std::result::Result<Option<bool>, Malformed> {
        match self.peek().ok_or(Malformed)? {
            b't' => self.literal(b"true").map(|()| Some(true)),
            b'f' => self.literal(b"false").map(|()| Some(false)),
            b'n' => self.literal(b"null").map(|()| None),
            _ => Err(Malformed),
        }
    }

    fn skip_string(&mut self) -> std::result::Result<(), Malformed> {
        self.expect(b'"')?;
        loop {
            let c = self.peek().ok_or(Malformed)?;
            self.i += 1;
            match c {
                b'"' => return Ok(()),
                b'\\' => {
                    self.peek().ok_or(Malformed)?;
                    self.i += 1;
                }
                _ => {}
            }
        }
    }

    /// Step over a value this client does not use.
    fn skip_value(&mut self) -> std::result::Result<(), Malformed> {
        match self.peek().ok_or(Malformed)? {
            b'"' => self.skip_string(),
            b'{' | b'[' => {
                let mut depth = 0usize;
                loop {
                    match self.peek().ok_or(Malformed)? {
                        b'"' => self.skip_string()?,
                        b'{' | b'[' => {
                            depth += 1;
                            self.i += 1;
                        }
                        b'}' | b']' => {
                            depth = depth.checked_sub(1).ok_or(Malformed)?;
                            self.i += 1;
                            if depth == 0 {
                                return Ok(());
                            }
                        }
                        _ => self.i += 1,
                    }
                }
            }
            b't' => self.literal(b"true"),
            b'f' => self.literal(b"false"),
            b'n' => self.literal(b"null"),
            b'-' | b'0'..=b'9' => {
                while matches!(
                    self.peek(),
                    Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                ) {
                    self.i += 1;
                }
                Ok(())
            }
            _ => Err(Malformed),
        }
    }
}

/// Parse a `read` reply: an object with optional `ok`, `error` and `value`. Other keys are
/// ignored; a repeated `ok`, `error` or `value` is malformed, as it is for the derived parser
/// this replaces.
fn parse_read_reply(line: &str) -> std::result::Result<ReadReply, Malformed> {
    let mut scan = Scan {
        b: line.as_bytes(),
        i: 0,
    };
    let mut reply = ReadReply {
        ok: None,
        error: None,
        value: None,
    };
    let (mut seen_ok, mut seen_error, mut seen_value) = (false, false, false);
    scan.ws();
    scan.expect(b'{')?;
    scan.ws();
    if !scan.eat(b'}') {
        loop {
            scan.ws();
            let key = scan.plain_string()?;
            scan.ws();
            scan.expect(b':')?;
            scan.ws();
            match key.as_str() {
                "ok" if !seen_ok => {
                    seen_ok = true;
                    reply.ok = scan.bool_or_null()?;
                }
                "error" if !seen_error => {
                    seen_error = true;
                    reply.error = scan.string_or_null()?;
                }
                "value" if !seen_value => {
                    seen_value = true;
                    reply.value = scan.secret_or_null()?;
                }
                "ok" | "error" | "value" => return Err(Malformed),
                _ => scan.skip_value()?,
            }
            scan.ws();
            if scan.eat(b',') {
                continue;
            }
            scan.expect(b'}')?;
            break;
        }
    }
    scan.ws();
    if scan.peek().is_some() {
        return Err(Malformed);
    }
    Ok(reply)
}

fn check_ok(v: &Value) -> Result<()> {
    if v.get("ok").and_then(Value::as_bool) != Some(true) {
        let msg = v
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("helper reported failure");
        return Err(Error::Helper(msg.to_string()));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Synchronizable iCloud Keychain item (visible on every device of the Apple Account).
    Synced,
    /// This device only.
    Device,
}

impl Scope {
    fn wire(self) -> &'static str {
        match self {
            Scope::Synced => "synced",
            Scope::Device => "device",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperStatus {
    pub ready: bool,
    pub detail: Option<String>,
}

/// The signing identity a helper must have before [`AppleHelper::new_trusted`] will run it: a
/// Developer ID chain (`anchor apple generic`) whose leaf certificate belongs to one team.
///
/// The check is the macOS `codesign` code-requirement check; see [`AppleHelper::new_trusted`]
/// for exactly when it runs and what it does not cover.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HelperTrust {
    team: String,
}

impl HelperTrust {
    /// Require a helper signed by the Developer ID team `team_id`: exactly 10 ASCII uppercase
    /// letters or digits (an Apple team identifier). Anything else is refused, so the value can
    /// never alter the requirement text.
    pub fn team(team_id: &str) -> Result<Self> {
        let valid = team_id.len() == 10
            && team_id
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
        if !valid {
            return Err(Error::Invalid {
                what: "team id",
                reason: "must be 10 uppercase ASCII letters or digits".into(),
            });
        }
        Ok(Self {
            team: team_id.to_string(),
        })
    }

    /// The team identifier.
    pub fn team_id(&self) -> &str {
        &self.team
    }

    /// The code requirement:
    /// `anchor apple generic and certificate leaf[subject.OU] = "<TEAM>"`.
    pub fn requirement(&self) -> String {
        format!(
            "anchor apple generic and certificate leaf[subject.OU] = \"{}\"",
            self.team
        )
    }
}

#[derive(Debug, Clone)]
pub struct AppleHelper {
    path: PathBuf,
    args: Vec<OsString>,
    timeout: Duration,
    /// `Some` for a client built by [`AppleHelper::new_trusted`]: verified before every spawn.
    trust: Option<HelperTrust>,
}

/// Owns the helper process and kills and reaps it when dropped, so no path out of an exchange
/// (a timeout, a failed write, an early return, a panic) leaves it running or unreaped.
struct Reaped(Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        // Killing a process that has already exited is not an error.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl AppleHelper {
    /// `path` must be the helper inside the app bundle (it carries the keychain-access-groups
    /// entitlement); resolve and contain it under the bundle before calling.
    ///
    /// This client does NOT check the helper's code signature: it executes whatever is at
    /// `path` and writes secret values to it. Production code must use
    /// [`AppleHelper::new_trusted`]; keep `new` for tests and tools that handle no real secret.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            args: Vec::new(),
            timeout: Duration::from_secs(10),
            trust: None,
        }
    }

    /// Like [`AppleHelper::new`], but every exchange first verifies the helper's code signature
    /// and refuses to run it otherwise, so a swapped, unsigned or foreign-signed helper never
    /// receives a secret.
    ///
    /// IMMEDIATELY before EVERY spawn (each `status`, `read`, `exists`, `write`,
    /// `delete_everywhere` and `copy_synced_to_device_only` call) the helper path is
    /// canonicalised and `/usr/bin/codesign --verify --strict -R "=<requirement>" <target>` is run
    /// (absolute path, no `PATH` lookup, no shell), where `<target>` is the enclosing `.app` when
    /// the path has the form `<X>.app/Contents/MacOS/<file>` (the signed unit) and the file
    /// itself otherwise. The check is bounded to 20 s; a timeout kills the `codesign` process.
    /// A non-zero exit, a timeout, a missing or unrunnable `codesign` (including every non-macOS
    /// platform), or a path that is not a regular file refuses the call with
    /// [`Error::Helper`] (`"untrusted helper: ..."`), before the helper is started and before
    /// any secret is written anywhere. There is no skip, environment override or debug fallback.
    ///
    /// Residual TOCTOU: the check and the spawn are two steps on the same canonical path. A
    /// process that can replace files inside the signed bundle between them could still run a
    /// different program, because `codesign` verifies a path, not the process later created from
    /// it. Such a process can also replace the app itself, so this defends against a swapped,
    /// unsigned or foreign-signed helper, not against a compromised install directory. The
    /// requirement proves who signed the helper (the team), not that it is the helper you meant;
    /// resolve the path with `resolve_bundled_login_item` (macOS) so placement is checked as well.
    pub fn new_trusted(path: impl Into<PathBuf>, trust: HelperTrust) -> Self {
        Self {
            trust: Some(trust),
            ..Self::new(path)
        }
    }
    /// One deadline for the whole exchange: writing the request, reading the reply and the helper
    /// exiting.
    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = d;
        self
    }
    /// Extra helper arguments (e.g. `--service`, `--isolated-keychain` for tests).
    pub fn args<I: IntoIterator<Item = S>, S: Into<OsString>>(mut self, args: I) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// A request line (JSON + newline) in a buffer that is zeroed on drop. Used for requests that
    /// carry no secret; `write` builds its own line so the secret is never copied into a
    /// `serde_json::Value`.
    fn call(&self, request: Value) -> Result<Value> {
        let mut line = Zeroizing::new(
            serde_json::to_vec(&request)
                .map_err(|_| Error::Helper("cannot encode request".into()))?,
        );
        line.push(b'\n');
        let out = self.exchange(line)?;
        let first = out.lines().next().unwrap_or("");
        let v: Value = serde_json::from_str(first)
            .map_err(|_| Error::Helper("invalid helper response".into()))?;
        check_ok(&v)?;
        Ok(v)
    }

    /// Send one request line, return the raw response text (zeroed on drop). The response buffer
    /// is allocated at its 1 MiB cap up front so it never reallocates while the helper's reply
    /// (which may hold a secret) is read, leaving no smaller unwiped copy behind.
    ///
    /// The request is written and the reply read on their own threads so that a helper that
    /// never reads its stdin, or never closes its stdout, cannot block this call: both are
    /// bounded by the one deadline, and the helper is killed and reaped when the call ends,
    /// whatever the outcome (a helper that has closed stdout but is still alive is killed
    /// rather than waited for).
    fn exchange(&self, line: Zeroizing<Vec<u8>>) -> Result<Zeroizing<String>> {
        // A trusted client verifies the helper first and then runs exactly the path it
        // verified. This happens before the exchange deadline starts and before anything is
        // spawned or written, so a failed check cannot reach the secret in `line`.
        let exec_path = match &self.trust {
            Some(trust) => verify_helper(&self.path, trust)?,
            None => self.path.clone(),
        };
        let started = Instant::now();
        // A timeout too large to add to the clock is treated as "a year".
        let deadline = started
            .checked_add(self.timeout)
            .unwrap_or_else(|| started + Duration::from_secs(365 * 24 * 60 * 60));
        let timed_out = || Error::Timeout {
            op: "apple_helper",
            secs: self.timeout.as_secs(),
        };
        let remaining = || deadline.saturating_duration_since(Instant::now());

        let mut child = Reaped(
            Command::new(&exec_path)
                .args(&self.args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| Error::Helper(format!("cannot start helper: {}", e.kind())))?,
        );
        let mut stdin = child
            .0
            .stdin
            .take()
            .ok_or_else(|| Error::Helper("no stdin".into()))?;
        let mut stdout = child
            .0
            .stdout
            .take()
            .ok_or_else(|| Error::Helper("no stdout".into()))?;

        let (written_tx, written_rx) = mpsc::channel();
        std::thread::spawn(move || {
            // Ends when the write completes or fails; killing the helper breaks the pipe, so a
            // write that is blocked on a helper that never reads stops when the call returns.
            let result = stdin.write_all(&line).map_err(|e| e.kind());
            drop(stdin);
            drop(line);
            let _ = written_tx.send(result);
        });
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Zeroizing::new(String::with_capacity(RESPONSE_CAP));
            let _ = stdout
                .by_ref()
                .take(RESPONSE_CAP as u64)
                .read_to_string(&mut buf);
            let _ = tx.send(buf);
        });

        match written_rx.recv_timeout(remaining()) {
            Ok(Ok(())) => {}
            Ok(Err(kind)) => return Err(Error::Helper(format!("write failed: {kind}"))),
            Err(_) => return Err(timed_out()),
        }
        rx.recv_timeout(remaining()).map_err(|_| timed_out())
        // `child` is dropped here: the helper is killed and reaped, so it cannot outlive the
        // deadline by staying alive after it closed stdout.
    }

    pub fn status(&self) -> Result<HelperStatus> {
        let v = self.call(json!({"command": "status"}))?;
        Ok(HelperStatus {
            ready: v.get("status").and_then(Value::as_str) == Some("ready"),
            detail: v.get("detail").and_then(Value::as_str).map(str::to_string),
        })
    }
    pub fn read(&self, scope: Scope, account: &str) -> Result<Option<Secret>> {
        let mut line = Zeroizing::new(
            serde_json::to_vec(
                &json!({"command": "read", "scope": scope.wire(), "account": account}),
            )
            .map_err(|_| Error::Helper("cannot encode request".into()))?,
        );
        line.push(b'\n');
        let out = self.exchange(line)?;
        let first = out.lines().next().unwrap_or("");
        // Hand-decoded: the value goes from the zeroised reply into a zeroised string with no
        // intermediate copy (neither a `Value::String` nor serde_json's scratch buffer).
        let resp =
            parse_read_reply(first).map_err(|_| Error::Helper("invalid helper response".into()))?;
        if resp.ok != Some(true) {
            return Err(Error::Helper(
                resp.error
                    .unwrap_or_else(|| "helper reported failure".to_string()),
            ));
        }
        Ok(resp.value.map(|w| Secret::from_bytes(w.as_bytes())))
    }
    /// Presence only; the helper does not request the item's data.
    pub fn exists(&self, scope: Scope, account: &str) -> Result<bool> {
        let v =
            self.call(json!({"command": "exists", "scope": scope.wire(), "account": account}))?;
        v.get("exists")
            .and_then(Value::as_bool)
            .ok_or_else(|| Error::Helper("helper lacks `exists` support".into()))
    }
    pub fn write(&self, scope: Scope, account: &str, secret: &Secret) -> Result<()> {
        let value = secret.expose_str()?;
        // Serialize straight from the borrowed secret into a zeroed buffer. JSON escaping grows a
        // byte to at most six (`\u00XX`), so reserving that much up front means the buffer never
        // reallocates and no smaller copy of the secret is left behind.
        let mut line = Zeroizing::new(Vec::with_capacity(
            value
                .len()
                .saturating_add(account.len())
                .saturating_mul(6)
                .saturating_add(128),
        ));
        serde_json::to_writer(
            &mut *line,
            &WriteRequest {
                command: "write",
                scope: scope.wire(),
                account,
                value,
            },
        )
        .map_err(|_| Error::Helper("cannot encode request".into()))?;
        line.push(b'\n');
        let out = self.exchange(line)?;
        let first = out.lines().next().unwrap_or("");
        let v: Value = serde_json::from_str(first)
            .map_err(|_| Error::Helper("invalid helper response".into()))?;
        check_ok(&v)
    }
    /// Removes the synced AND device copy; Apple propagates the deletion to every device of the
    /// account, so call only for an explicit "forget everywhere" user action.
    pub fn delete_everywhere(&self, account: &str) -> Result<()> {
        self.call(json!({"command": "delete_everywhere", "account": account}))?;
        Ok(())
    }
    /// Turning sync off: keep working locally without touching the synced copy.
    pub fn copy_synced_to_device_only(&self, account: &str) -> Result<bool> {
        let v = self.call(json!({"command": "copy_synced_to_device_only", "account": account}))?;
        Ok(v.get("copied").and_then(Value::as_bool).unwrap_or(false))
    }
}

/// Bundle subdirectories (under `Contents/`) searched by [`resolve_bundled`], in order.
const BUNDLE_HELPER_DIRS: [&str; 3] = ["MacOS", "Helpers", "Library/LoginItems"];

/// Resolve the helper `name` inside the running app's bundle, or `None`. `name` is a single file
/// name (no separator, no `.`/`..`), looked up as
/// `Contents/{MacOS,Helpers,Library/LoginItems}/<name>` of the `.app` that contains the
/// running executable. The result is canonical, a regular file (not a symlink out of the
/// bundle) and verified to lie under the canonical bundle root; any doubt is `None`.
/// Pass the result to [`AppleHelper::new`]. This checks placement only; it does not check the
/// code signature (the helper's entitlement is enforced by the OS at run time).
pub fn resolve_bundled(name: &str) -> Option<PathBuf> {
    resolve_in_bundle(&std::env::current_exe().ok()?, name)
}

fn resolve_in_bundle(exe: &std::path::Path, name: &str) -> Option<PathBuf> {
    use std::path::Component;
    let mut parts = std::path::Path::new(name).components();
    match (parts.next(), parts.next()) {
        (Some(Component::Normal(n)), None) if n == std::ffi::OsStr::new(name) => {}
        _ => return None,
    }
    let exe = exe.canonicalize().ok()?;
    let bundle = exe.ancestors().find(|a| {
        a.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")) && a.join("Contents").is_dir()
    })?;
    let bundle = bundle.canonicalize().ok()?;
    BUNDLE_HELPER_DIRS.iter().find_map(|dir| {
        let found = bundle
            .join("Contents")
            .join(dir)
            .join(name)
            .canonicalize()
            .ok()?;
        (found.starts_with(&bundle) && found.is_file()).then_some(found)
    })
}

/// The nested login-item location, relative to the bundle root: the helper `<name>` is the
/// executable of its own `<name>.app` (which is the unit that is signed and carries the
/// provisioning profile).
const LOGIN_ITEMS_DIR: &str = "Contents/Library/LoginItems";

/// Resolve the nested login-item helper `name` of the running app:
/// `<main bundle>/Contents/Library/LoginItems/<name>.app/Contents/MacOS/<name>`.
///
/// `name` must be a single path component (not empty, not `.` or `..`, no separator). The result
/// is canonical, a regular file, and must lie inside the nested `<name>.app` that itself lies
/// inside the canonical main bundle, so a symlink that leaves the helper app or the bundle is
/// refused (and so is one that points at another file of the same bundle). Any doubt is an
/// error: [`Error::Invalid`] for a bad `name`, [`Error::Helper`] for everything else.
///
/// This checks placement only; pass the result to [`AppleHelper::new_trusted`] so the code
/// signature is checked as well. [`resolve_bundled`] cannot find this layout.
#[cfg(target_os = "macos")]
pub fn resolve_bundled_login_item(name: &str) -> Result<PathBuf> {
    let exe = std::env::current_exe().map_err(|e| {
        Error::Helper(format!(
            "cannot locate the running executable: {}",
            e.kind()
        ))
    })?;
    login_item_in_bundle(&exe, name)
}

/// The path logic of `resolve_bundled_login_item`, independent of the OS and of the running
/// executable (`exe` is any file inside the main `.app`).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn login_item_in_bundle(exe: &Path, name: &str) -> Result<PathBuf> {
    use std::path::Component;
    let bad_name = |reason: &str| Error::Invalid {
        what: "login item helper name",
        reason: reason.to_string(),
    };
    if name.is_empty() {
        return Err(bad_name("empty"));
    }
    let mut parts = Path::new(name).components();
    match (parts.next(), parts.next()) {
        (Some(Component::Normal(n)), None) if n == OsStr::new(name) => {}
        _ => return Err(bad_name("must be a single path component")),
    }
    // Refuse both separator spellings on every OS, so a name that is one component here cannot
    // be several on another.
    if name.contains(['/', '\\']) {
        return Err(bad_name("must be a single path component"));
    }
    let missing = |what: &str| Error::Helper(format!("login item helper {what}"));
    let exe = exe
        .canonicalize()
        .map_err(|_| missing("not found: the running executable cannot be resolved"))?;
    let bundle = exe
        .ancestors()
        .find(|a| {
            a.extension().is_some_and(|e| e.eq_ignore_ascii_case("app"))
                && a.join("Contents").is_dir()
        })
        .ok_or_else(|| missing("not found: the running executable is not inside an .app bundle"))?;
    let bundle = bundle
        .canonicalize()
        .map_err(|_| missing("not found: the app bundle cannot be resolved"))?;
    let app = bundle.join(LOGIN_ITEMS_DIR).join(format!("{name}.app"));
    let app = app
        .canonicalize()
        .map_err(|_| missing("not found in Contents/Library/LoginItems"))?;
    if !app.starts_with(&bundle) {
        return Err(missing("resolves outside the app bundle"));
    }
    let binary = app
        .join("Contents")
        .join("MacOS")
        .join(name)
        .canonicalize()
        .map_err(|_| missing("executable not found in its .app"))?;
    if !binary.starts_with(&app) {
        return Err(missing("executable resolves outside its .app"));
    }
    if !binary.is_file() {
        return Err(missing("executable is not a regular file"));
    }
    Ok(binary)
}

const CODESIGN: &str = "/usr/bin/codesign";
const CODESIGN_TIMEOUT: Duration = Duration::from_secs(20);

fn untrusted(reason: impl std::fmt::Display) -> Error {
    Error::Helper(format!("untrusted helper: {reason}"))
}

/// What `codesign` should be pointed at: the enclosing `.app` when `binary` is
/// `<X>.app/Contents/MacOS/<file>` (the sealed, signed unit), else the file itself.
fn signed_unit(binary: &Path) -> &Path {
    let named = |p: &Path, n: &str| p.file_name().is_some_and(|f| f == OsStr::new(n));
    let layout = binary.parent().and_then(|macos| {
        let contents = macos.parent()?;
        let app = contents.parent()?;
        (named(macos, "MacOS")
            && named(contents, "Contents")
            && app
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("app")))
        .then_some(app)
    });
    layout.unwrap_or(binary)
}

/// Verify `path` against `trust` and return the canonical path that was verified, which the
/// caller must execute (not `path`). Fails closed on every doubt; see
/// [`AppleHelper::new_trusted`].
fn verify_helper(path: &Path, trust: &HelperTrust) -> Result<PathBuf> {
    let canonical = path
        .canonicalize()
        .map_err(|e| untrusted(format!("cannot resolve the helper path ({})", e.kind())))?;
    if !canonical.is_file() {
        return Err(untrusted("the helper is not a regular file"));
    }
    // `-R` takes a requirement file path unless the text starts with `=`.
    let requirement = OsString::from(format!("={}", trust.requirement()));
    let target = signed_unit(&canonical);
    let args: [&OsStr; 5] = [
        OsStr::new("--verify"),
        OsStr::new("--strict"),
        OsStr::new("-R"),
        &requirement,
        target.as_os_str(),
    ];
    match run_bounded(Path::new(CODESIGN), &args, CODESIGN_TIMEOUT) {
        Ok(Bounded::Exited(status)) if status.success() => Ok(canonical),
        Ok(Bounded::Exited(status)) => Err(untrusted(format!(
            "the code signature check failed ({status})"
        ))),
        Ok(Bounded::TimedOut) => Err(untrusted(format!(
            "the code signature check did not finish within {} s",
            CODESIGN_TIMEOUT.as_secs()
        ))),
        Err(e) => Err(untrusted(format!(
            "the code signature check could not run ({})",
            e.kind()
        ))),
    }
}

enum Bounded {
    Exited(std::process::ExitStatus),
    TimedOut,
}

/// Run `program` with no stdin and no output, wait at most `limit`, and kill and reap it if it
/// is still running then (or if waiting fails). Output goes to the null device so there is no
/// pipe that could fill and stall the child.
///
/// Only the child itself is killed, not a process tree: `codesign` starts no children that
/// outlive it. The poll sleeps at most 10 ms at a time and the call ends by `limit`.
fn run_bounded(program: &Path, args: &[&OsStr], limit: Duration) -> std::io::Result<Bounded> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let now = Instant::now();
    let deadline = now
        .checked_add(limit)
        .unwrap_or_else(|| now + Duration::from_secs(365 * 24 * 60 * 60));
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Bounded::Exited(status)),
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Bounded::TimedOut);
        }
        std::thread::sleep(left.min(Duration::from_millis(10)));
    }
}
