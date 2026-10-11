//! Client for the RightKitAppleCredentials Swift helper (the synchronizable iCloud Keychain
//! cannot be reached from Rust). One JSON request line on stdin, one JSON response line on
//! stdout, then the helper exits.
//! Secret values appear only on the stdin pipe and in the returned [`Secret`]; they are never
//! placed in an error.

use crate::error::{Error, Result};
use crate::secret::Secret;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

/// Response cap (and read buffer size) for one helper reply.
const RESPONSE_CAP: usize = 1 << 20;

#[derive(Serialize)]
struct WriteRequest<'a> {
    command: &'static str,
    scope: &'static str,
    account: &'a str,
    value: &'a str,
}

/// A string that is zeroed when dropped, deserialized without an intermediate `Value`.
struct WipedString(Zeroizing<String>);

impl<'de> Deserialize<'de> for WipedString {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = WipedString;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a string")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<WipedString, E> {
                Ok(WipedString(Zeroizing::new(v.to_owned())))
            }
        }
        d.deserialize_str(V)
    }
}

#[derive(Deserialize)]
struct ReadResponse {
    ok: Option<bool>,
    error: Option<String>,
    value: Option<WipedString>,
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
    fn exchange(&self, line: Zeroizing<Vec<u8>>) -> Result<Zeroizing<String>> {
        let mut child = Command::new(&self.path)
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Error::Helper(format!("cannot start helper: {}", e.kind())))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Helper("no stdin".into()))?;
        stdin
            .write_all(&line)
            .map_err(|e| Error::Helper(format!("write failed: {}", e.kind())))?;
        drop(stdin);
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Helper("no stdout".into()))?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Zeroizing::new(String::with_capacity(RESPONSE_CAP));
            let _ = stdout
                .by_ref()
                .take(RESPONSE_CAP as u64)
                .read_to_string(&mut buf);
            let _ = tx.send(buf);
        });
        let deadline = Instant::now() + self.timeout;
        let out = match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(s) => s,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Timeout {
                    op: "apple_helper",
                    secs: self.timeout.as_secs(),
                });
            }
        };
        let _ = child.wait();
        Ok(out)
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
            serde_json::to_vec(&json!({"command": "read", "scope": scope.wire(), "account": account}))
                .map_err(|_| Error::Helper("cannot encode request".into()))?,
        );
        line.push(b'\n');
        let out = self.exchange(line)?;
        let first = out.lines().next().unwrap_or("");
        // Typed parse: the value goes straight into a zeroed string, never a `Value::String`.
        let resp: ReadResponse = serde_json::from_str(first)
            .map_err(|_| Error::Helper("invalid helper response".into()))?;
        if resp.ok != Some(true) {
            return Err(Error::Helper(
                resp.error
                    .unwrap_or_else(|| "helper reported failure".to_string()),
            ));
        }
        Ok(resp
            .value
            .map(|w| Secret::from_bytes(w.0.as_bytes())))
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
