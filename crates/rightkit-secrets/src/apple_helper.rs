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
    /// Decoded straight from the zeroised reply line into a zeroising buffer.
    value: Option<Zeroizing<String>>,
}

/// The reply was not the expected JSON object.
#[derive(Debug)]
struct Malformed;

/// A cursor over the reply line.
///
/// `read` replies carry the secret, and `serde_json` decodes a string that contains an escape
/// (`\n`, `\"`, `é`, ...) through a scratch buffer it owns and frees without wiping. So the
/// `value` field is decoded here instead: the unescaped bytes go into one `Zeroizing` buffer that
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
    fn secret_or_null(&mut self) -> std::result::Result<Option<Zeroizing<String>>, Malformed> {
        if self.peek() == Some(b'n') {
            self.literal(b"null")?;
            return Ok(None);
        }
        let mut out = Zeroizing::new(Vec::with_capacity(self.b.len().saturating_sub(self.i)));
        self.string_into(&mut out)?;
        // Moves the buffer into the String without copying it.
        let bytes = std::mem::take(&mut *out);
        String::from_utf8(bytes)
            .map(|text| Some(Zeroizing::new(text)))
            .map_err(|e| {
                let mut bytes = e.into_bytes();
                bytes.zeroize();
                Malformed
            })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn value_of(line: &str) -> Option<String> {
        parse_read_reply(line)
            .unwrap()
            .value
            .map(|text| text.as_str().to_string())
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
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;

    // ---- a heap that reports freed blocks that still hold a marker ----
    //
    // Reading a freed block is only sound if every byte of it is initialised, and a general heap
    // block is not (spare `Vec` capacity, padding). So the probe never reads a block it did not
    // initialise itself: while armed, every allocation is made ZEROED (`alloc_zeroed`) and its
    // address is recorded in a fixed table; only a recorded block is scanned when it is freed,
    // and every byte of it has been initialised (zero, or written by the program). Blocks
    // allocated while disarmed are never read.

    const MARKER: &[u8] = b"rk-wipe-marker-7f3a91";
    static ARMED: AtomicBool = AtomicBool::new(false);
    static UNWIPED_FREES: AtomicUsize = AtomicUsize::new(0);
    /// Only one test arms the probe at a time.
    static PROBE: Mutex<()> = Mutex::new(());

    const SLOTS: usize = 1 << 16;
    const EMPTY: usize = 0;
    const TOMBSTONE: usize = usize::MAX;
    /// Addresses of live blocks allocated (zeroed) while armed. Open addressing, no allocation.
    static TRACKED: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(EMPTY) }; SLOTS];

    fn slot_of(addr: usize) -> usize {
        (addr >> 4).wrapping_mul(0x9E37_79B9_7F4A_7C15_u64 as usize) % SLOTS
    }

    /// Record `addr`; `false` if the table is full (the block is then simply never scanned).
    fn track(addr: usize) -> bool {
        let start = slot_of(addr);
        for step in 0..SLOTS {
            let slot = &TRACKED[(start + step) % SLOTS];
            let seen = slot.load(Ordering::Acquire);
            if (seen == EMPTY || seen == TOMBSTONE)
                && slot
                    .compare_exchange(seen, addr, Ordering::AcqRel, Ordering::Relaxed)
                    .is_ok()
            {
                return true;
            }
        }
        false
    }

    /// Forget `addr`; `true` if it was recorded (so every byte of its block is initialised).
    fn untrack(addr: usize) -> bool {
        let start = slot_of(addr);
        for step in 0..SLOTS {
            let slot = &TRACKED[(start + step) % SLOTS];
            match slot.load(Ordering::Acquire) {
                EMPTY => return false,
                seen if seen == addr => {
                    return slot
                        .compare_exchange(addr, TOMBSTONE, Ordering::AcqRel, Ordering::Relaxed)
                        .is_ok();
                }
                _ => {}
            }
        }
        false
    }

    struct Probe;

    // SAFETY: every call is forwarded to the system allocator; the extra work is (a) zero-filling
    // blocks allocated while armed, and (b) reading, on free, only a block this allocator itself
    // zero-filled and recorded, which the caller owns and which is fully initialised.
    unsafe impl GlobalAlloc for Probe {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if ARMED.load(Ordering::Relaxed) {
                let ptr = System.alloc_zeroed(layout);
                if !ptr.is_null() && layout.size() >= MARKER.len() {
                    track(ptr as usize);
                }
                return ptr;
            }
            System.alloc(layout)
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            if layout.size() >= MARKER.len() && untrack(ptr as usize) {
                let block = std::slice::from_raw_parts(ptr, layout.size());
                if block.windows(MARKER.len()).any(|w| w == MARKER) {
                    UNWIPED_FREES.fetch_add(1, Ordering::Relaxed);
                }
            }
            System.dealloc(ptr, layout)
        }
    }

    #[global_allocator]
    static HEAP: Probe = Probe;

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

    /// Audit finding: a reply string with JSON escapes was decoded through serde_json's ordinary
    /// scratch buffer, which is freed without being wiped. The probe heap reports any freed block
    /// that still holds the decoded secret.
    #[test]
    fn an_escaped_secret_leaves_no_unwiped_copy_on_the_heap() {
        let _one_probe_at_a_time = PROBE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let reply_file = dir.path().join("reply.json");
        // The wire text has escapes, so the decoded value differs from it.
        let wire = format!(
            "{{\"ok\":true,\"value\":\"{}\\nsecond\\u0041line \\\"quoted\\\"\"}}\n",
            std::str::from_utf8(MARKER).unwrap()
        );
        std::fs::write(&reply_file, &wire).unwrap();
        let expected = format!(
            "{}\nsecondAline \"quoted\"",
            std::str::from_utf8(MARKER).unwrap()
        );
        // The script text and its arguments never contain the marker; only the pipe does.
        let helper = AppleHelper::new("/bin/sh")
            .args(["-c", "cat >/dev/null; cat \"$1\"", "sh"])
            .args([reply_file.as_os_str()]);

        UNWIPED_FREES.store(0, Ordering::SeqCst);
        ARMED.store(true, Ordering::SeqCst);
        let secret = helper.read(Scope::Device, "acct");
        ARMED.store(false, Ordering::SeqCst);

        let secret = secret.unwrap().expect("a value");
        assert_eq!(secret.expose_str().unwrap(), expected);
        assert_eq!(
            UNWIPED_FREES.load(Ordering::SeqCst),
            0,
            "a heap block holding the decoded secret was freed without being wiped"
        );
    }
}
