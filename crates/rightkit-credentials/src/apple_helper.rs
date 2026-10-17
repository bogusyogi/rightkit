//! Client for the RightKitAppleCredentials Swift helper (the synchronizable iCloud Keychain
//! cannot be reached from Rust). One JSON request line on stdin, one JSON response line on
//! stdout, then the helper exits.
//! Secret values appear only on the stdin pipe and in the returned [`Secret`]; they are never
//! placed in an error.
//!
//! One deadline (`timeout`, default 10 s) covers the whole exchange: writing the request,
//! reading the reply and the helper exiting. The helper is killed and reaped on every path,
//! including a write that blocks because the helper never reads its stdin.

use crate::error::{Error, Result};
use crate::secret::Secret;
use serde::Serialize;
use serde_json::{json, Value};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
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
    #[cfg(test)]
    reserved: usize,
}

impl SecretBuf {
    fn with_capacity(capacity: usize) -> Self {
        let bytes = Vec::with_capacity(capacity);
        Self {
            #[cfg(test)]
            reserved: bytes.capacity(),
            bytes,
        }
    }
    fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl Drop for SecretBuf {
    fn drop(&mut self) {
        #[cfg(test)]
        let held_data = self.bytes.iter().any(|b| *b != 0);
        // Wipe the initialised bytes in place first, so they can be inspected afterwards.
        self.bytes.as_mut_slice().zeroize();
        #[cfg(test)]
        wipe_probe::record(wipe_probe::Wiped {
            len: self.bytes.len(),
            held_data,
            reserved: self.reserved,
            capacity: self.bytes.capacity(),
            zeroed: self.bytes.iter().all(|b| *b == 0),
        });
        // Then the spare capacity, and clear.
        self.bytes.zeroize();
    }
}

/// What each dropped [`SecretBuf`] looked like after its wipe: a controlled, per-buffer hook in
/// place of scanning the allocator.
#[cfg(test)]
mod wipe_probe {
    use std::cell::RefCell;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Wiped {
        /// Initialised bytes the buffer held.
        pub len: usize,
        /// Some initialised byte was non-zero before the wipe.
        pub held_data: bool,
        /// Capacity at creation, and at drop (equal: it never reallocated, so no unwiped copy
        /// was left behind by a growth).
        pub reserved: usize,
        pub capacity: usize,
        /// Every initialised byte was zero after the wipe.
        pub zeroed: bool,
    }

    thread_local! {
        static LOG: RefCell<Vec<Wiped>> = const { RefCell::new(Vec::new()) };
    }

    pub fn record(entry: Wiped) {
        LOG.with(|log| log.borrow_mut().push(entry));
    }

    /// Everything dropped on this thread since the last call.
    pub fn take() -> Vec<Wiped> {
        LOG.with(|log| std::mem::take(&mut *log.borrow_mut()))
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

#[derive(Debug, Clone)]
pub struct AppleHelper {
    path: PathBuf,
    args: Vec<OsString>,
    timeout: Duration,
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
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            args: Vec::new(),
            timeout: Duration::from_secs(10),
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
            Command::new(&self.path)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_in_bundle_finds_contains_and_refuses_escapes() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("Demo.app").join("Contents");
        std::fs::create_dir_all(app.join("MacOS")).unwrap();
        std::fs::create_dir_all(app.join("Helpers")).unwrap();
        let exe = app.join("MacOS").join("demo");
        std::fs::write(&exe, b"x").unwrap();
        std::fs::write(app.join("Helpers").join("helper"), b"x").unwrap();
        let found = resolve_in_bundle(&exe, "helper").unwrap();
        assert!(found.starts_with(tmp.path().canonicalize().unwrap().join("Demo.app")));
        assert!(resolve_in_bundle(&exe, "missing").is_none());
        for bad in ["", ".", "..", "../MacOS/demo", "a/b", "/etc/passwd"] {
            assert!(resolve_in_bundle(&exe, bad).is_none(), "{bad}");
        }
        assert!(resolve_in_bundle(&tmp.path().join("plain"), "helper").is_none());
        #[cfg(unix)]
        {
            let outside = tmp.path().join("outside");
            std::fs::write(&outside, b"x").unwrap();
            std::os::unix::fs::symlink(&outside, app.join("Helpers").join("escape")).unwrap();
            assert!(resolve_in_bundle(&exe, "escape").is_none());
        }
    }

    fn value_of(line: &str) -> Option<String> {
        parse_read_reply(line)
            .unwrap()
            .value
            .map(|text| String::from_utf8(text.as_bytes().to_vec()).unwrap())
    }

    #[test]
    fn the_hand_decoder_agrees_with_serde_json_on_every_escape() {
        let samples = [
            "plain",
            "",
            "with \"quotes\" and \\ backslash and / slash",
            "tab\tnew\nline\rreturn\u{8}backspace\u{c}formfeed",
            "caf\u{e9} \u{65e5}\u{672c} \u{1f511} emoji",
            "\u{0}\u{1}\u{1f}control",
            "mixed \u{7f} del and \u{2028} separators",
        ];
        for sample in samples {
            let token = serde_json::to_string(sample).unwrap();
            let line = format!(r#"{{"ok":true,"value":{token}}}"#);
            assert_eq!(value_of(&line).as_deref(), Some(sample), "{sample:?}");
        }
        // Escapes serde_json never emits but accepts.
        let line = r#"{"ok":true,"value":"éé😀\/\b"}"#;
        let expected: String = serde_json::from_str(r#""éé😀\/\b""#).unwrap();
        assert_eq!(value_of(line).as_deref(), Some(expected.as_str()));
    }

    /// Audit finding 6: the earlier test scanned every allocation made while it ran, which reads
    /// bytes this program never initialised. This one observes the one buffer that holds the
    /// decoded secret, through its own drop hook, and reads only the bytes written to it.
    #[test]
    fn an_escaped_secret_is_decoded_into_one_buffer_that_never_grew_and_is_wiped() {
        let _ = wipe_probe::take();
        let wire = r#"{"ok":true,"value":"rk-wipe-marker\nsecond\u0041line \"quoted\""}"#;
        let expected = "rk-wipe-marker\nsecondAline \"quoted\"";
        let reply = parse_read_reply(wire).unwrap();
        assert_eq!(
            reply.value.as_ref().unwrap().as_bytes(),
            expected.as_bytes()
        );
        assert!(
            wipe_probe::take().is_empty(),
            "the buffer was dropped while still in use"
        );
        drop(reply);
        let log = wipe_probe::take();
        assert_eq!(log.len(), 1, "exactly one buffer holds the secret: {log:?}");
        let wiped = &log[0];
        assert_eq!(wiped.len, expected.len());
        assert!(wiped.held_data, "the probe saw no secret before the wipe");
        assert!(wiped.zeroed, "initialised bytes survived the wipe");
        assert_eq!(
            wiped.capacity, wiped.reserved,
            "decoding reallocated, which would leave an unwiped copy behind"
        );
    }

    #[test]
    fn a_reply_rejected_after_part_of_the_secret_was_decoded_wipes_that_part() {
        for bad in [
            r#"{"value":"partial-secret\q"}"#,
            r#"{"value":"partial-secret\ud800 lone"}"#,
            r#"{"value":"partial-secret\u12g4"}"#,
            r#"{"value":"partial-secret"} trailing"#,
        ] {
            let _ = wipe_probe::take();
            assert!(parse_read_reply(bad).is_err(), "{bad:?}");
            let log = wipe_probe::take();
            assert_eq!(log.len(), 1, "{bad:?}: {log:?}");
            assert!(log[0].held_data && log[0].zeroed, "{bad:?}: {log:?}");
            assert_eq!(log[0].capacity, log[0].reserved, "{bad:?}");
        }
    }

    #[test]
    fn a_null_value_allocates_no_secret_buffer() {
        let _ = wipe_probe::take();
        let reply = parse_read_reply(r#"{"ok":true,"value":null}"#).unwrap();
        assert!(reply.value.is_none());
        drop(reply);
        assert!(wipe_probe::take().is_empty());
    }

    #[test]
    fn ok_error_null_and_unknown_keys_are_read() {
        let reply = parse_read_reply(
            r#" { "extra" : {"a":[1,-2.5e3,{"b":"x\"}"}],"c":null,"d":true} , "ok" : false , "error":"no \"item\"", "value": null } "#,
        )
        .unwrap();
        assert_eq!(reply.ok, Some(false));
        assert_eq!(reply.error.as_deref(), Some("no \"item\""));
        assert!(reply.value.is_none());
        let empty = parse_read_reply("{}").unwrap();
        assert!(empty.ok.is_none() && empty.error.is_none() && empty.value.is_none());
        assert_eq!(parse_read_reply(r#"{"ok":null}"#).unwrap().ok, None);
    }

    #[test]
    fn malformed_replies_are_refused() {
        for bad in [
            "",
            "null",
            "[]",
            "{",
            r#"{"ok":true"#,
            r#"{"ok":true,}"#,
            r#"{"ok":true} trailing"#,
            r#"{"ok":1}"#,
            r#"{"value":"unterminated}"#,
            r#"{"value":"bad \q escape"}"#,
            r#"{"value":"raw
newline"}"#,
            r#"{"value":"\ud800 lone high"}"#,
            r#"{"value":"\udc00 lone low"}"#,
            r#"{"value":"\ud800A"}"#,
            r#"{"value":"\u12g4"}"#,
            r#"{"value":5}"#,
            r#"{"ok":true,"ok":true}"#,
            r#"{"value":"a","value":"b"}"#,
            r#"{"error":"a","error":"b"}"#,
        ] {
            assert!(parse_read_reply(bad).is_err(), "{bad:?}");
        }
    }
}

#[cfg(test)]
#[cfg(unix)]
mod process_tests {
    use super::*;
    fn sh(script: &str, timeout: Duration) -> AppleHelper {
        AppleHelper::new("/bin/sh")
            .args(["-c", script])
            .timeout(timeout)
    }

    /// Run `f` on its own thread and fail the test, rather than hang it, if it outlives `limit`.
    fn within<T: Send + 'static>(limit: Duration, f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(limit)
            .expect("the helper call outlived every deadline it was given")
    }

    #[test]
    fn a_helper_that_never_reads_stdin_cannot_block_a_large_request() {
        // 1 MiB is far beyond a pipe's buffer, so the write blocks until the helper reads.
        let secret = Secret::from_string("x".repeat(1 << 20));
        let outcome = within(Duration::from_secs(20), move || {
            sh("exec sleep 30", Duration::from_millis(300)).write(Scope::Device, "acct", &secret)
        });
        assert!(
            matches!(
                outcome,
                Err(Error::Timeout {
                    op: "apple_helper",
                    ..
                })
            ),
            "{outcome:?}"
        );
    }

    #[test]
    fn a_helper_that_closes_stdout_but_stays_alive_cannot_block_the_final_wait() {
        let started = Instant::now();
        let outcome = within(Duration::from_secs(20), || {
            // Reads the request, closes stdout (no reply), then lingers.
            sh(
                "cat >/dev/null; exec 1>&-; sleep 20",
                Duration::from_secs(10),
            )
            .status()
        });
        assert!(
            matches!(outcome, Err(Error::Helper(_))),
            "an empty reply is an invalid response, not a timeout: {outcome:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "the call waited for the lingering helper: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_well_behaved_helper_still_answers() {
        let answer = within(Duration::from_secs(20), || {
            sh(
                "cat >/dev/null; printf '%s\\n' '{\"ok\":true,\"exists\":true}'",
                Duration::from_secs(10),
            )
            .exists(Scope::Device, "acct")
        });
        assert_eq!(answer, Ok(true));
    }

    #[test]
    fn a_failed_call_kills_and_reaps_the_helper() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let helper = AppleHelper::new("/bin/sh")
            .args(["-c", "echo $$ > \"$1\"; exec sleep 30", "sh"])
            .args([pid_file.as_os_str()])
            .timeout(Duration::from_millis(500));
        let outcome = within(Duration::from_secs(20), move || {
            helper.exists(Scope::Device, "acct")
        });
        assert!(matches!(outcome, Err(Error::Timeout { .. })), "{outcome:?}");
        let pid = std::fs::read_to_string(&pid_file).unwrap();
        let alive = Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(!alive, "helper {} is still running", pid.trim());
    }

    /// M11-O1: the wire requests for the two scope-changing calls, and the reading of `copied`.
    /// The Swift side of the same semantics is `AppleCredentialTests` in RightKitSwift.
    #[test]
    fn scope_changing_calls_send_the_documented_command_and_read_copied() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("req");
        let script = "cat > \"$1\"; printf '%s\\n' \"$2\"";
        let run = |reply: &str, f: &dyn Fn(&AppleHelper) -> Result<bool>| {
            let helper = AppleHelper::new("/bin/sh")
                .args(["-c", script, "sh"])
                .args([log.as_os_str(), std::ffi::OsStr::new(reply)])
                .timeout(Duration::from_secs(10));
            let out = f(&helper);
            (out, std::fs::read_to_string(&log).unwrap())
        };
        let (out, req) = run("{\"ok\":true,\"copied\":true}", &|h| {
            h.copy_synced_to_device_only("acct")
        });
        assert_eq!(out, Ok(true));
        let v: Value = serde_json::from_str(req.lines().next().unwrap()).unwrap();
        assert_eq!(v["command"], "copy_synced_to_device_only");
        assert_eq!(v["account"], "acct");
        let (out, _) = run("{\"ok\":true,\"copied\":false}", &|h| {
            h.copy_synced_to_device_only("acct")
        });
        assert_eq!(out, Ok(false));
        let (out, req) = run("{\"ok\":true}", &|h| {
            h.delete_everywhere("acct").map(|()| true)
        });
        assert_eq!(out, Ok(true));
        let v: Value = serde_json::from_str(req.lines().next().unwrap()).unwrap();
        assert_eq!(v["command"], "delete_everywhere");
        assert_eq!(v["account"], "acct");
    }
}
