//! Client for the RightKitAppleCredentials Swift helper (the synchronizable iCloud Keychain
//! cannot be reached from Rust). One JSON request line on stdin, one JSON response line on
//! stdout, then the helper exits; wire-compatible with HeardRight/ScrapeRight helpers.
//! Secret values appear only on the stdin pipe and in the returned [`Secret`]; they are never
//! placed in an error.

use crate::error::{Error, Result};
use crate::secret::Secret;
use serde_json::{json, Value};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

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

    fn call(&self, request: Value) -> Result<Value> {
        let line = Zeroizing::new(format!("{request}\n"));
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
            .write_all(line.as_bytes())
            .map_err(|e| Error::Helper(format!("write failed: {}", e.kind())))?;
        drop(stdin);
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Helper("no stdout".into()))?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Zeroizing::new(String::new());
            let _ = stdout.by_ref().take(1 << 20).read_to_string(&mut buf);
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
        let first = out.lines().next().unwrap_or("");
        let v: Value = serde_json::from_str(first)
            .map_err(|_| Error::Helper("invalid helper response".into()))?;
        if v.get("ok").and_then(Value::as_bool) != Some(true) {
            let msg = v
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("helper reported failure");
            return Err(Error::Helper(msg.to_string()));
        }
        Ok(v)
    }

    pub fn status(&self) -> Result<HelperStatus> {
        let v = self.call(json!({"command": "status"}))?;
        Ok(HelperStatus {
            ready: v.get("status").and_then(Value::as_str) == Some("ready"),
            detail: v.get("detail").and_then(Value::as_str).map(str::to_string),
        })
    }
    pub fn read(&self, scope: Scope, account: &str) -> Result<Option<Secret>> {
        let v = self.call(json!({"command": "read", "scope": scope.wire(), "account": account}))?;
        Ok(v.get("value")
            .and_then(Value::as_str)
            .map(Secret::from_string))
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
        self.call(
            json!({"command": "write", "scope": scope.wire(), "account": account, "value": value}),
        )?;
        Ok(())
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
