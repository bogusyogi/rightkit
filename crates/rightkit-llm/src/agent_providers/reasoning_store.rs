//! Native reasoning blocks (signed thinking, encrypted reasoning items) kept for replay.
//!
//! The normalized transcript carries only plain reasoning text, so a provider's signed or
//! encrypted blocks are looked up again by key when the next request is built. Entries live in
//! memory and, when the host sets `persist_reasoning`, in an append-only file under the state
//! root, so a daemon restart or a resumed session still finds them. Nothing here is authoritative: a miss means the block is simply not
//! replayed, as before.

use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

struct State {
    entries: Vec<(String, Vec<Value>)>,
    loaded: bool,
}

pub(crate) struct ReasoningStore {
    file_name: &'static str,
    cap: usize,
    state: Mutex<State>,
}

impl ReasoningStore {
    pub(crate) const fn new(file_name: &'static str, cap: usize) -> Self {
        Self {
            file_name,
            cap,
            state: Mutex::new(State {
                entries: Vec::new(),
                loaded: false,
            }),
        }
    }

    fn path(&self) -> Option<PathBuf> {
        if !super::host::host_options().persist_reasoning {
            return None;
        }
        let dir = rightkit_agent_config::state_root()?;
        Some(dir.join(self.file_name))
    }

    fn load(&self, state: &mut State) {
        if state.loaded {
            return;
        }
        state.loaded = true;
        let Some(text) = self
            .path()
            .and_then(|path| std::fs::read_to_string(path).ok())
        else {
            return;
        };
        for line in text.lines() {
            let Ok(Value::Object(mut row)) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let (Some(Value::String(key)), Some(Value::Array(blocks))) =
                (row.remove("k"), row.remove("b"))
            else {
                continue;
            };
            state.entries.retain(|(existing, _)| existing != &key);
            state.entries.push((key, blocks));
        }
        let over = state.entries.len().saturating_sub(self.cap);
        state.entries.drain(..over);
    }

    pub(crate) fn remember(&self, key: String, blocks: Vec<Value>) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        self.load(&mut state);
        state.entries.retain(|(existing, _)| existing != &key);
        let row = serde_json::json!({"k": key, "b": blocks});
        state.entries.push((key, blocks));
        let trimmed = state.entries.len() > self.cap;
        if trimmed {
            state.entries.remove(0);
        }
        let Some(path) = self.path() else {
            return;
        };
        // Best effort: a failed write only loses replay after a restart.
        if trimmed {
            let mut body = String::new();
            for (key, blocks) in &state.entries {
                body.push_str(&serde_json::json!({"k": key, "b": blocks}).to_string());
                body.push('\n');
            }
            let _ = write_private(&path, body.as_bytes(), false);
        } else {
            let _ = write_private(&path, format!("{row}\n").as_bytes(), true);
        }
    }

    pub(crate) fn recall(&self, key: &str) -> Option<Vec<Value>> {
        let mut state = self.state.lock().ok()?;
        self.load(&mut state);
        state
            .entries
            .iter()
            .find(|(existing, _)| existing == key)
            .map(|(_, blocks)| blocks.clone())
    }
}

/// Write `bytes` to `path`, owner-only on Unix: reasoning text is session content.
fn write_private(path: &std::path::Path, bytes: &[u8], append: bool) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true);
    if append {
        options.append(true);
    } else {
        options.truncate(true);
    }
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?.write_all(bytes)
}
