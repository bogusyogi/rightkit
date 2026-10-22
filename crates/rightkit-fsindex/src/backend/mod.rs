//! Platform backend: enumeration plus change feed.
//!
//! macOS: getattrlistbulk crawl + FSEvents (`macos`). Windows: Windows Search
//! SystemIndex over OLE DB (`windows_search`, the default backend there) and the NTFS USN
//! journal reader (`windows_usn`; MFT path resolution and the change-feed hookup are still
//! TODO). `UnsupportedBackend` remains for other platforms. Everything above this trait is
//! platform-neutral.

use crate::error::Result;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::sync::Arc;

#[cfg(target_os = "macos")]
pub mod macos;
pub mod windows_search;
pub mod windows_stub;
pub mod windows_usn;

pub const NONE: u32 = u32::MAX;

pub const KIND_FILE: u8 = 0;
pub const KIND_DIR: u8 = 1;
pub const KIND_LINK: u8 = 2;
pub const KIND_OTHER: u8 = 3;

pub const FLAG_PACKAGE: u8 = 1;
/// Hard link whose size is owned (counted) by another link of the same inode.
pub const FLAG_SECONDARY_LINK: u8 = 2;
/// Directory deliberately not indexed (protected store, consent-gated, configured exclusion).
pub const FLAG_EXCLUDED: u8 = 4;
pub const FLAG_HIDDEN: u8 = 8;
/// Mount point not descended into.
pub const FLAG_MOUNT: u8 = 16;
/// Directory that could not be read (permissions, Full Disk Access).
pub const FLAG_UNREADABLE: u8 = 32;

/// Per-entry metadata as reported by the platform.
#[derive(Clone, Copy, Debug, Default)]
pub struct EntryData {
    pub kind: u8,
    pub flags: u8,
    pub nlink: u32,
    pub mtime: i64,
    pub dev: u32,
    pub fileid: u64,
    /// data fork + resource fork length
    pub logical: u64,
    /// total allocated bytes (all forks)
    pub alloc: u64,
    /// bytes freed if only this entry were deleted (APFS PRIVATESIZE); equals `alloc` when unknown
    pub private: u64,
    /// APFS clone id; 0 when none
    pub cloneid: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct RawEnt {
    pub name_off: u32,
    pub name_len: u16,
    pub data: EntryData,
    /// Id of the listing of this directory in the same crawl, or NONE.
    pub child: u32,
}

#[derive(Debug, Default)]
pub struct Listing {
    pub id: u32,
    pub names: Vec<u8>,
    pub ents: Vec<RawEnt>,
    pub error: Option<std::io::ErrorKind>,
}

impl Listing {
    pub fn name(&self, e: &RawEnt) -> &[u8] {
        &self.names[e.name_off as usize..e.name_off as usize + e.name_len as usize]
    }
}

/// Directories never descended into: exact path prefixes, plus glob patterns over path
/// components. Pattern matching is relative to the root that contains the path, so a root
/// placed under a pattern's name is still indexed.
#[derive(Clone, Debug, Default)]
pub struct Excludes {
    pub prefixes: Vec<Vec<u8>>,
    /// Each pattern is split on `/`; each segment matches one path component with `*` and `?`.
    /// A pattern matches wherever its segments appear as consecutive components.
    pub patterns: Vec<Vec<Vec<u8>>>,
    /// Roots, without a trailing slash (`/` is stored as the empty string).
    pub roots: Vec<Vec<u8>>,
    /// Absolute prefixes compared ASCII case-insensitively, with `/` and `\` equal (Windows
    /// paths). Same component-boundary rule as `prefixes`.
    pub ci_prefixes: Vec<Vec<u8>>,
    /// Skip directories that contain a `CACHEDIR.TAG` file with the standard signature.
    /// The check reads at most 43 bytes of that one file and happens in the platform crawl
    /// (`matches` does no I/O).
    pub honor_cachedir_tag: bool,
}

/// First bytes of a valid `CACHEDIR.TAG` (<https://bford.info/cachedir/>).
pub const CACHEDIR_TAG_SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// Whether `content` (the start of a `CACHEDIR.TAG` file) carries the standard signature.
pub fn cachedir_tag_matches(content: &[u8]) -> bool {
    content.starts_with(CACHEDIR_TAG_SIGNATURE)
}

/// Whether `dir` holds a regular `CACHEDIR.TAG` file with the standard signature.
pub fn dir_has_cachedir_tag(dir: &Path) -> bool {
    crate::names::read_file_head(&dir.join("CACHEDIR.TAG"), CACHEDIR_TAG_SIGNATURE.len())
        .is_some_and(|head| cachedir_tag_matches(&head))
}

fn is_sep(b: u8) -> bool {
    b == b'/' || b == b'\\'
}

fn ci_prefix_match(path: &[u8], prefix: &[u8]) -> bool {
    if path.len() < prefix.len() {
        return false;
    }
    let same = path
        .iter()
        .zip(prefix)
        .all(|(&a, &b)| (is_sep(a) && is_sep(b)) || a.eq_ignore_ascii_case(&b));
    same && (path.len() == prefix.len() || is_sep(path[prefix.len()]))
}

impl Excludes {
    pub fn matches(&self, path: &[u8]) -> bool {
        if self
            .prefixes
            .iter()
            .any(|s| path.starts_with(s) && (path.len() == s.len() || path[s.len()] == b'/'))
        {
            return true;
        }
        if self.ci_prefixes.iter().any(|s| ci_prefix_match(path, s)) {
            return true;
        }
        if self.patterns.is_empty() {
            return false;
        }
        let rel = self.relative(path);
        let comps: Vec<&[u8]> = rel
            .split(|&b| b == b'/')
            .filter(|c| !c.is_empty())
            .collect();
        self.patterns.iter().any(|segs| {
            segs.len() <= comps.len()
                && (0..=comps.len() - segs.len()).any(|i| {
                    segs.iter()
                        .zip(&comps[i..])
                        .all(|(seg, comp)| glob_match(seg, comp))
                })
        })
    }

    /// The part of `path` below the longest root that contains it, or the whole path.
    fn relative<'a>(&self, path: &'a [u8]) -> &'a [u8] {
        let mut best: Option<usize> = None;
        for r in &self.roots {
            let inside = path.starts_with(r) && (path.len() == r.len() || path[r.len()] == b'/');
            if inside && best.is_none_or(|b| r.len() > b) {
                best = Some(r.len());
            }
        }
        match best {
            Some(n) => &path[n..],
            None => path,
        }
    }
}

/// `*` matches any run of bytes and `?` one byte; everything else matches itself.
pub fn glob_match(pat: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while t < text.len() {
        if p < pat.len() && (pat[p] == b'?' || pat[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == b'*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(sp) = star {
            p = sp + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

pub const EV_MUST_SCAN_SUBDIRS: u32 = 0x1;
pub const EV_USER_DROPPED: u32 = 0x2;
pub const EV_KERNEL_DROPPED: u32 = 0x4;
pub const EV_HISTORY_DONE: u32 = 0x10;
pub const EV_ROOT_CHANGED: u32 = 0x20;
pub const EV_MOUNT: u32 = 0x40;
pub const EV_UNMOUNT: u32 = 0x80;

#[derive(Clone, Debug)]
pub struct Event {
    pub path: Vec<u8>,
    pub flags: u32,
    pub id: u64,
}

pub enum WatchMsg {
    Events(Vec<Event>),
    /// Worker acknowledges after everything queued before it was applied.
    Flush(Sender<()>),
    Stop,
    /// Rebuild the column store (test hook); acknowledged when done.
    Compact(Sender<()>),
    /// Drop clean mapped pages from the resident set (throttled by the worker).
    Trim,
}

pub trait Watcher: Send {
    /// Push out events the platform has not delivered yet (blocking).
    fn flush(&self);
}

pub trait Backend: Send + Sync {
    fn name(&self) -> &'static str;
    /// Err(Unsupported) on platforms without an implementation.
    fn check_supported(&self) -> Result<()>;
    /// Metadata for one path without following symlinks.
    fn stat(&self, path: &Path) -> std::io::Result<EntryData>;
    /// Recursive crawl streamed to `sink`; listing id 0 is `root` itself. A directory's listing
    /// is always delivered before the listings of its subdirectories.
    fn crawl(
        &self,
        root: &Path,
        excludes: &Excludes,
        threads: usize,
        sink: &mut dyn FnMut(Listing),
    );
    /// One directory, no recursion; subdirectories have `child == NONE`.
    fn list_dir(&self, dir: &Path, excludes: &Excludes) -> std::io::Result<Listing>;
    /// Start the change feed. `since`: resume from this event id (None: from now).
    fn watch(
        &self,
        roots: &[std::path::PathBuf],
        since: Option<u64>,
        latency: std::time::Duration,
        tx: Sender<WatchMsg>,
    ) -> Result<Box<dyn Watcher>>;
    /// Current change-feed position.
    fn current_event_id(&self) -> u64;
    /// Whether the process can read protected stores (Full Disk Access on macOS).
    fn has_full_disk_access(&self) -> bool;
}

pub fn default_backend() -> Arc<dyn Backend> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(macos::MacBackend)
    }
    #[cfg(windows)]
    {
        Arc::new(windows_search::WindowsSearchBackend)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Arc::new(windows_stub::UnsupportedBackend)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_star_question_and_literals() {
        assert!(glob_match(b"target", b"target"));
        assert!(glob_match(b"*.app", b"Foo.app"));
        assert!(!glob_match(b"*.app", b"Foo.apps"));
        assert!(glob_match(b"a?c", b"abc"));
        assert!(!glob_match(b"a?c", b"ac"));
        assert!(glob_match(b"*", b""));
        assert!(glob_match(b"a*b*c", b"aXXbYYc"));
        assert!(!glob_match(b"a*b", b"ba"));
        assert!(
            !glob_match(b"Target", b"target"),
            "matching is case-sensitive"
        );
    }

    #[test]
    fn a_pattern_matches_consecutive_components_below_the_root() {
        let ex = Excludes {
            patterns: vec![vec![b".git".to_vec(), b"objects".to_vec()]],
            roots: vec![b"/work".to_vec()],
            ..Excludes::default()
        };
        assert!(ex.matches(b"/work/repo/.git/objects"));
        assert!(ex.matches(b"/work/repo/.git/objects/ab"));
        assert!(!ex.matches(b"/work/repo/.git"));
        assert!(!ex.matches(b"/work/repo/objects"));
    }

    #[test]
    fn a_root_under_a_pattern_name_is_not_excluded() {
        let ex = Excludes {
            patterns: vec![vec![b"target".to_vec()]],
            roots: vec![b"/work/target/proj".to_vec()],
            ..Excludes::default()
        };
        assert!(!ex.matches(b"/work/target/proj"));
        assert!(!ex.matches(b"/work/target/proj/src"));
        assert!(ex.matches(b"/work/target/proj/x/target"));
    }

    #[test]
    fn absolute_roots_match_as_prefixes_not_as_names() {
        let ex = Excludes {
            prefixes: vec![b"/Volumes/D/scratch".to_vec()],
            ..Excludes::default()
        };
        assert!(ex.matches(b"/Volumes/D/scratch"));
        assert!(ex.matches(b"/Volumes/D/scratch/a/b"));
        assert!(!ex.matches(b"/Volumes/D/scratch2"));
        assert!(!ex.matches(b"/Volumes/E/scratch"));
        assert!(!ex.matches(b"/home/me/scratch"), "not a name pattern");
    }

    #[test]
    fn windows_roots_compare_case_insensitively() {
        let ex = Excludes {
            ci_prefixes: vec![br"D:\scratch".to_vec()],
            ..Excludes::default()
        };
        assert!(ex.matches(br"D:\scratch"));
        assert!(ex.matches(br"d:\SCRATCH\x\y"));
        assert!(ex.matches(b"d:/Scratch/x"));
        assert!(!ex.matches(br"D:\scratch2"));
        assert!(!ex.matches(br"E:\scratch"));
        assert!(!ex.matches(br"D:\work\scratch"));
    }

    #[test]
    fn cachedir_tag_needs_the_standard_signature() {
        assert!(cachedir_tag_matches(
            b"Signature: 8a477f597d28d172789f06886806bc55\n# comment"
        ));
        assert!(!cachedir_tag_matches(b"Signature: 0000"));
        assert!(!cachedir_tag_matches(b""));
        let d = tempfile::tempdir().unwrap();
        assert!(!dir_has_cachedir_tag(d.path()));
        std::fs::write(d.path().join("CACHEDIR.TAG"), b"nope").unwrap();
        assert!(!dir_has_cachedir_tag(d.path()));
        std::fs::write(
            d.path().join("CACHEDIR.TAG"),
            b"Signature: 8a477f597d28d172789f06886806bc55\n# Created by cargo.\n",
        )
        .unwrap();
        assert!(dir_has_cachedir_tag(d.path()));
    }

    #[test]
    fn exact_prefixes_still_match_on_component_boundaries() {
        let ex = Excludes {
            prefixes: vec![b"/work/private".to_vec()],
            ..Excludes::default()
        };
        assert!(ex.matches(b"/work/private"));
        assert!(ex.matches(b"/work/private/x"));
        assert!(!ex.matches(b"/work/privateer"));
    }
}
