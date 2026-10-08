//! Rename-storm coalescing.
//!
//! Editors save by writing a temp file and renaming it over the original; build tools and VCS
//! checkouts create, rename, and delete thousands of paths in milliseconds. [`ChangeCoalescer`]
//! absorbs raw `notify` events and, on [`ChangeCoalescer::drain`], reports the *net* result by
//! consulting the filesystem: a path that exists is `changed`, a path that vanished is `removed`,
//! a path created and gone again inside one batch (an atomic-save temp file) is dropped, and a
//! rename chain `a -> b -> c` collapses to a single `a -> c`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind};

/// A rename whose source existed before the batch and whose destination exists now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rename {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// Net effect of one burst. Every list is sorted and duplicate-free.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeBatch {
    /// Paths that exist now and were created or modified (includes rename destinations).
    pub changed: Vec<PathBuf>,
    /// Paths that existed before the batch and are gone now.
    pub removed: Vec<PathBuf>,
    /// Collapsed rename pairs (only reported when the backend pairs the two halves).
    pub renamed: Vec<Rename>,
}

impl ChangeBatch {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.removed.is_empty() && self.renamed.is_empty()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum First {
    Created,
    Other,
}

struct Touch {
    first: First,
}

#[derive(Default)]
pub struct ChangeCoalescer {
    touched: BTreeMap<PathBuf, Touch>,
    renames: Vec<(PathBuf, PathBuf)>,
}

impl ChangeCoalescer {
    /// Absorb a typed event without constructing backend events. Gaps return an
    /// error: the caller must reconcile its snapshot before trusting this batch.
    /// No pending changes are cleared when an error is returned.
    pub fn push_watch_event(&mut self, event: &crate::WatchEvent) -> Result<(), crate::WatchError> {
        use crate::WatchEvent;
        let mut touch = |path: &PathBuf, first| {
            self.touched.entry(path.clone()).or_insert(Touch { first });
        };
        match event {
            WatchEvent::Create(path) => touch(path, First::Created),
            WatchEvent::Modify(path) | WatchEvent::Delete(path) => touch(path, First::Other),
            WatchEvent::Rename { from, to } => {
                touch(from, First::Other);
                touch(to, First::Other);
                self.push_rename(from.clone(), to.clone());
            }
            WatchEvent::Error(error) => return Err(error.clone()),
            WatchEvent::Overflow => return Err(crate::WatchError::Overflow),
        }
        Ok(())
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.touched.is_empty() && self.renames.is_empty()
    }

    /// Absorb one raw event. Access and "other" events are ignored.
    pub fn push(&mut self, event: &Event) {
        let created = matches!(event.kind, EventKind::Create(_));
        let relevant = matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
        );
        if !relevant {
            return;
        }
        for path in &event.paths {
            self.touched.entry(path.clone()).or_insert(Touch {
                first: if created {
                    First::Created
                } else {
                    First::Other
                },
            });
        }
        if let EventKind::Modify(ModifyKind::Name(RenameMode::Both)) = event.kind {
            if let [from, to] = event.paths.as_slice() {
                self.push_rename(from.clone(), to.clone());
            }
        }
    }

    fn push_rename(&mut self, from: PathBuf, to: PathBuf) {
        // Collapse chains: a->b then b->c becomes a->c.
        if let Some(entry) = self
            .renames
            .iter_mut()
            .find(|(_, existing_to)| *existing_to == from)
        {
            entry.1 = to;
        } else {
            self.renames.push((from, to));
        }
    }

    /// Resolve everything absorbed so far against the filesystem and reset.
    pub fn drain(&mut self) -> ChangeBatch {
        let touched = std::mem::take(&mut self.touched);
        let renames = std::mem::take(&mut self.renames);
        let mut batch = ChangeBatch::default();
        let ephemeral = |path: &Path, touched: &BTreeMap<PathBuf, Touch>| {
            touched.get(path).is_some_and(|t| t.first == First::Created)
        };
        for (path, touch) in &touched {
            if exists(path) {
                batch.changed.push(path.clone());
            } else if touch.first != First::Created {
                batch.removed.push(path.clone());
            }
        }
        for (from, to) in renames {
            if from == to || ephemeral(&from, &touched) || !exists(&to) || exists(&from) {
                continue;
            }
            batch.renamed.push(Rename { from, to });
        }
        batch.renamed.sort_by(|a, b| a.from.cmp(&b.from));
        batch.renamed.dedup();
        batch
    }
}

fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WatchError, WatchEvent};

    #[test]
    fn typed_adapter_feeds_all_kinds_and_collapses_rename_chain() {
        let mut coalescer = ChangeCoalescer::new();
        for event in [
            WatchEvent::Create("new".into()),
            WatchEvent::Modify("new".into()),
            WatchEvent::Modify("changed".into()),
            WatchEvent::Delete("gone".into()),
            WatchEvent::Rename {
                from: "a".into(),
                to: "b".into(),
            },
            WatchEvent::Rename {
                from: "b".into(),
                to: "c".into(),
            },
        ] {
            coalescer.push_watch_event(&event).unwrap();
        }
        assert_eq!(coalescer.touched.len(), 6);
        assert!(coalescer.touched[Path::new("new")].first == First::Created);
        for path in ["changed", "gone", "a", "b", "c"] {
            assert!(coalescer.touched[Path::new(path)].first == First::Other);
        }
        assert_eq!(
            coalescer.renames,
            vec![(PathBuf::from("a"), PathBuf::from("c"))]
        );
    }

    #[test]
    fn typed_adapter_returns_gaps_without_losing_pending_changes() {
        let mut coalescer = ChangeCoalescer::new();
        coalescer
            .push_watch_event(&WatchEvent::Create("pending".into()))
            .unwrap();
        let error = WatchError::Backend {
            message: "unavailable".into(),
            paths: vec!["root".into()],
        };
        assert_eq!(
            coalescer.push_watch_event(&WatchEvent::Error(error.clone())),
            Err(error)
        );
        assert_eq!(
            coalescer.push_watch_event(&WatchEvent::Overflow),
            Err(WatchError::Overflow)
        );
        assert_eq!(coalescer.touched.len(), 1);
        assert!(coalescer.touched.contains_key(Path::new("pending")));
    }

    #[test]
    fn typed_adapter_drain_reports_net_state_without_backend_events() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let from = root.join("from");
        let to = root.join("to");
        let new = root.join("new");
        let temporary = root.join("temporary");
        std::fs::write(&to, "renamed payload").unwrap();
        std::fs::write(&new, "new payload").unwrap();
        let mut coalescer = ChangeCoalescer::new();
        for event in [
            WatchEvent::Rename {
                from: from.clone(),
                to: to.clone(),
            },
            WatchEvent::Create(new.clone()),
            WatchEvent::Create(temporary.clone()),
            WatchEvent::Delete(temporary),
        ] {
            coalescer.push_watch_event(&event).unwrap();
        }
        let batch = coalescer.drain();
        assert_eq!(batch.changed, vec![new, to.clone()]);
        assert_eq!(batch.removed, vec![from.clone()]);
        assert_eq!(batch.renamed, vec![Rename { from, to }]);
        assert!(coalescer.is_empty());
    }
}
