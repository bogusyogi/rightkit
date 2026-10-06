use crate::toolbox::{Journal, ToolEvent};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// Appends one JSON object per event to a file (content-free; never uploaded).
pub struct JsonlJournal {
    path: PathBuf,
    lock: Mutex<()>,
}

impl JsonlJournal {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), lock: Mutex::new(()) }
    }
}

impl Journal for JsonlJournal {
    fn record(&self, event: &ToolEvent) {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = writeln!(f, "{}", event.to_value());
        }
    }
}
