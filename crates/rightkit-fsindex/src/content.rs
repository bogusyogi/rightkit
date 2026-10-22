//! Opt-in content search: a trigram index over allowlisted text files.
//!
//! Compiled only with the `content` cargo feature and inert until
//! `ContentConfig::enabled` is true. This is the only module that opens file
//! contents.

use crate::config::Config;
use std::collections::HashMap;
use std::path::Path;

pub struct ContentIndex {
    enabled: bool,
    allow: Vec<Vec<u8>>,
    excl: Vec<Vec<u8>>,
    exts: Vec<Vec<u8>>,
    max_size: u64,
    postings: HashMap<u32, Vec<u32>>,
    per_file: HashMap<u32, Vec<u32>>,
}

fn under(path: &[u8], root: &[u8]) -> bool {
    path.starts_with(root) && (path.len() == root.len() || path[root.len()] == b'/' || root == b"/")
}

fn canon_bytes(p: &Path) -> Vec<u8> {
    crate::index::path_bytes(&std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()))
}

impl ContentIndex {
    pub fn new(cfg: &Config) -> ContentIndex {
        let c = &cfg.content;
        ContentIndex {
            enabled: c.enabled,
            allow: c.allowlist.iter().map(|p| canon_bytes(p)).collect(),
            excl: c
                .exclude
                .iter()
                .chain(cfg.exclude.iter())
                .map(|p| canon_bytes(p))
                .collect(),
            exts: c
                .text_extensions
                .iter()
                .map(|e| e.trim_start_matches('.').to_ascii_lowercase().into_bytes())
                .collect(),
            max_size: c.max_file_size,
            postings: HashMap::new(),
            per_file: HashMap::new(),
        }
    }

    /// Publish a new slot namespace without reopening surviving file contents.
    pub fn remap(&mut self, map: impl Fn(u32) -> Option<u32>) {
        self.per_file = std::mem::take(&mut self.per_file)
            .into_iter()
            .filter_map(|(old, ts)| map(old).map(|new| (new, ts)))
            .collect();
        self.postings.clear();
        for (&slot, ts) in &self.per_file {
            for &t in ts {
                self.postings.entry(t).or_default().push(slot);
            }
        }
        for slots in self.postings.values_mut() {
            slots.sort_unstable();
            slots.dedup();
        }
    }

    pub fn clear(&mut self) {
        self.postings.clear();
        self.per_file.clear();
    }

    pub fn eligible(&self, path: &[u8], size: u64) -> bool {
        if !self.enabled || size == 0 || size > self.max_size {
            return false;
        }
        if !self.allow.iter().any(|r| under(path, r)) || self.excl.iter().any(|r| under(path, r)) {
            return false;
        }
        let name = path.rsplit(|&b| b == b'/').next().unwrap_or(path);
        match name.iter().rposition(|&b| b == b'.') {
            Some(p) => {
                let ext = &name[p + 1..];
                self.exts.iter().any(|e| e.eq_ignore_ascii_case(ext))
            }
            None => false,
        }
    }

    pub fn remove(&mut self, slot: u32) {
        if let Some(ts) = self.per_file.remove(&slot) {
            for t in ts {
                if let Some(v) = self.postings.get_mut(&t) {
                    if let Ok(i) = v.binary_search(&slot) {
                        v.remove(i);
                    }
                    if v.is_empty() {
                        self.postings.remove(&t);
                    }
                }
            }
        }
    }

    pub fn index_file(&mut self, slot: u32, path: &Path) {
        self.remove(slot);
        let Ok(data) = std::fs::read(path) else {
            return;
        };
        if data.len() as u64 > self.max_size || data[..data.len().min(8192)].contains(&0) {
            return;
        }
        let lower = data.to_ascii_lowercase();
        let mut ts: Vec<u32> = lower
            .windows(3)
            .map(|w| (w[0] as u32) << 16 | (w[1] as u32) << 8 | w[2] as u32)
            .collect();
        ts.sort_unstable();
        ts.dedup();
        for &t in &ts {
            let v = self.postings.entry(t).or_default();
            if let Err(i) = v.binary_search(&slot) {
                v.insert(i, slot);
            }
        }
        self.per_file.insert(slot, ts);
    }

    /// Slots whose files contain every trigram of `needle` (lowercase, at least 3 bytes). Verify afterwards.
    pub fn candidates(&self, needle: &[u8]) -> Vec<u32> {
        let mut lists: Vec<&Vec<u32>> = Vec::new();
        for w in needle.windows(3) {
            match self
                .postings
                .get(&((w[0] as u32) << 16 | (w[1] as u32) << 8 | w[2] as u32))
            {
                Some(v) => lists.push(v),
                None => return Vec::new(),
            }
        }
        lists.sort_by_key(|v| v.len());
        let Some((first, rest)) = lists.split_first() else {
            return Vec::new();
        };
        first
            .iter()
            .copied()
            .filter(|s| rest.iter().all(|v| v.binary_search(s).is_ok()))
            .collect()
    }
}

/// Count case-insensitive occurrences of `needle_lower` in the file, or 0 if unreadable.
pub fn verify(path: &Path, needle_lower: &[u8]) -> u32 {
    let Ok(data) = std::fs::read(path) else {
        return 0;
    };
    let lower = data.to_ascii_lowercase();
    memchr::memmem::find_iter(&lower, needle_lower).count() as u32
}
#[cfg(test)]
mod rewrite_tests {
    use super::*;
    use crate::backend::*;
    use crate::store::Store;

    #[test]
    fn compaction_remaps_content_postings_to_surviving_slots() {
        let temp = tempfile::tempdir().unwrap();
        let text = temp.path().join("kept.txt");
        std::fs::write(&text, b"unique needle content").unwrap();
        let mut cfg = Config::new(vec![temp.path().into()]);
        cfg.content.enabled = true;
        let mut content = ContentIndex::new(&cfg);
        let mut old = Store::create(None, Vec::new(), 9).unwrap();
        let root = old
            .add_root(
                b"/r",
                EntryData {
                    kind: KIND_DIR,
                    dev: 1,
                    ..EntryData::default()
                },
            )
            .unwrap();
        let mount = old
            .add_child(
                root,
                b"mnt",
                EntryData {
                    kind: KIND_DIR,
                    flags: FLAG_MOUNT,
                    dev: 1,
                    ..EntryData::default()
                },
            )
            .unwrap();
        let foreign = old
            .add_child(
                mount,
                b"gone.txt",
                EntryData {
                    kind: KIND_FILE,
                    dev: 2,
                    nlink: 1,
                    ..EntryData::default()
                },
            )
            .unwrap();
        let kept = old
            .add_child(
                root,
                b"kept.txt",
                EntryData {
                    kind: KIND_FILE,
                    dev: 1,
                    nlink: 1,
                    ..EntryData::default()
                },
            )
            .unwrap();
        content.index_file(foreign, &text);
        content.index_file(kept, &text);
        old.remove_subtree(foreign);
        let mut fresh = Store::create_sized(None, Vec::new(), 9, old.copy_hint()).unwrap();
        old.copy_into(&mut fresh).unwrap();
        let mapped = fresh.lookup(b"/r/kept.txt").unwrap();
        assert_ne!(mapped, kept);
        content.remap(|s| {
            old.is_live(s)
                .then(|| fresh.lookup(&old.path_of(s)))
                .flatten()
        });
        assert_eq!(content.candidates(b"needle"), vec![mapped]);
        assert_eq!(fresh.path_of(mapped), b"/r/kept.txt");
        assert_eq!(content.per_file.len(), 1);
    }
}
