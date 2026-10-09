//! The in-memory tree: entries, folder rollups, hard-link ownership and
//! APFS clone groups, all maintained incrementally.
//!
//! Size model (see README for the full statement):
//! * `logical`   = data fork + resource fork length.
//! * `allocated` = ALLOCSIZE (all forks), every clone counted in full.
//! * `allocated_unique`: every file contributes its `private` bytes (bytes only it
//!   holds); each clone group (files sharing extents, keyed by clone id) contributes its
//!   largest shared part once per folder that contains at least one member.
//! * A hard-linked inode contributes through exactly one owner link; other links are
//!   flagged SECONDARY_LINK and add nothing to any rollup.

use crate::backend::*;
use crate::names::NameStore;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rollup {
    pub logical: u64,
    pub alloc: u64,
    pub unique: u64,
    pub files: u64,
}

#[derive(Clone, Copy)]
pub struct Entry {
    pub name: u32,
    pub parent: u32,
    pub d: EntryData,
    pub live: bool,
}

#[derive(Default)]
struct DirData {
    children: Vec<u32>,
    roll: Rollup,
}

#[derive(Default)]
struct Group {
    members: Vec<(u32, u64)>,
    per_dir: HashMap<u32, u32>,
    s: u64,
}

pub struct Store {
    pub names: NameStore,
    ents: Vec<Entry>,
    dirs: Vec<Option<Box<DirData>>>,
    free: Vec<u32>,
    pub roots: Vec<u32>,
    links: HashMap<(u32, u64), Vec<u32>>,
    clones: HashMap<u64, Group>,
    package_exts: Vec<Vec<u8>>,
    live: usize,
    /// Slots removed / added / updated since the last `take_touched` (content index feed).
    pub removed_log: Vec<u32>,
    pub touched_log: Vec<u32>,
}

fn apply(v: &mut u64, d: i64) {
    *v = v.wrapping_add(d as u64);
}

fn is_countable_kind(k: u8) -> bool {
    k != KIND_DIR
}

impl Store {
    pub fn new(names: NameStore, package_exts: Vec<Vec<u8>>) -> Store {
        Store {
            names,
            ents: Vec::new(),
            dirs: Vec::new(),
            free: Vec::new(),
            roots: Vec::new(),
            links: HashMap::new(),
            clones: HashMap::new(),
            package_exts,
            live: 0,
            removed_log: Vec::new(),
            touched_log: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.live
    }
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }
    pub fn slots(&self) -> usize {
        self.ents.len()
    }
    pub fn is_live(&self, s: u32) -> bool {
        self.ents.get(s as usize).is_some_and(|e| e.live)
    }
    pub fn entry(&self, s: u32) -> &Entry {
        &self.ents[s as usize]
    }
    pub fn name_of(&self, s: u32) -> &[u8] {
        self.names.get(self.ents[s as usize].name)
    }
    pub fn rollup(&self, s: u32) -> Rollup {
        self.dirs[s as usize]
            .as_ref()
            .map_or(Rollup::default(), |d| d.roll)
    }
    pub fn children(&self, s: u32) -> &[u32] {
        self.dirs[s as usize].as_ref().map_or(&[], |d| &d.children)
    }

    fn is_package(&self, name: &[u8]) -> bool {
        match name.iter().rposition(|&b| b == b'.') {
            Some(p) if p + 1 < name.len() => {
                let ext = &name[p + 1..];
                self.package_exts.iter().any(|e| {
                    e.len() == ext.len()
                        && e.iter().zip(ext).all(|(a, b)| *a == b.to_ascii_lowercase())
                })
            }
            _ => false,
        }
    }

    fn alloc_slot(&mut self, e: Entry) -> u32 {
        self.live += 1;
        if let Some(s) = self.free.pop() {
            self.ents[s as usize] = e;
            self.dirs[s as usize] = None;
            s
        } else {
            self.ents.push(e);
            self.dirs.push(None);
            (self.ents.len() - 1) as u32
        }
    }

    /// Add a root entry (name is the absolute path).
    pub fn add_root(&mut self, path: &[u8], mut d: EntryData) -> std::io::Result<u32> {
        d.kind = KIND_DIR;
        let name = self.names.intern(path)?;
        let s = self.alloc_slot(Entry {
            name,
            parent: NONE,
            d,
            live: true,
        });
        self.dirs[s as usize] = Some(Box::default());
        self.roots.push(s);
        Ok(s)
    }

    /// Add `name` under directory `parent`. Contribution to ancestors is applied.
    pub fn add_child(
        &mut self,
        parent: u32,
        name: &[u8],
        mut d: EntryData,
    ) -> std::io::Result<u32> {
        if d.kind == KIND_DIR && self.is_package(name) {
            d.flags |= FLAG_PACKAGE;
        }
        d.flags &= !FLAG_SECONDARY_LINK;
        d.private = d.private.min(d.alloc);
        let nid = self.names.intern(name)?;
        let s = self.alloc_slot(Entry {
            name: nid,
            parent,
            d,
            live: true,
        });
        if d.kind == KIND_DIR {
            self.dirs[s as usize] = Some(Box::default());
        }
        if let Some(pd) = self.dirs[parent as usize].as_mut() {
            pd.children.push(s);
        }
        self.attach(s);
        self.touched_log.push(s);
        Ok(s)
    }

    /// Replace the metadata of a live entry (same kind), keeping rollups exact.
    pub fn update(&mut self, s: u32, mut d: EntryData) {
        let old = self.ents[s as usize].d;
        d.flags = (d.flags & !(FLAG_PACKAGE | FLAG_SECONDARY_LINK)) | (old.flags & FLAG_PACKAGE);
        d.private = d.private.min(d.alloc);
        if d.kind == KIND_DIR {
            self.ents[s as usize].d = EntryData {
                flags: d.flags | (old.flags & FLAG_UNREADABLE & !d.flags),
                ..d
            };
            return;
        }
        self.detach(s);
        self.ents[s as usize].d = d;
        self.attach(s);
        self.touched_log.push(s);
    }

    pub fn set_flag(&mut self, s: u32, flag: u8) {
        self.ents[s as usize].d.flags |= flag;
    }

    fn ancestors_apply(&mut self, s: u32, f: impl Fn(&mut Rollup)) {
        let mut p = self.ents[s as usize].parent;
        while p != NONE {
            if let Some(d) = self.dirs[p as usize].as_mut() {
                f(&mut d.roll);
            }
            p = self.ents[p as usize].parent;
        }
    }

    fn clone_key(d: &EntryData) -> u64 {
        if d.cloneid != 0 {
            d.cloneid
        } else {
            (1u64 << 63) | d.fileid
        }
    }

    /// Apply (sign = +1) or remove (-1) this entry's own contribution.
    fn contribute(&mut self, s: u32, sign: i64) {
        let d = self.ents[s as usize].d;
        let (lg, al, pr) = (
            d.logical as i64 * sign,
            d.alloc as i64 * sign,
            d.private as i64 * sign,
        );
        self.ancestors_apply(s, |r| {
            apply(&mut r.logical, lg);
            apply(&mut r.alloc, al);
            apply(&mut r.unique, pr);
            apply(&mut r.files, sign);
        });
        let shared = d.alloc - d.private;
        if shared > 0 {
            if sign > 0 {
                self.group_join(s, shared);
            } else {
                self.group_leave(s);
            }
        }
    }

    fn group_join(&mut self, s: u32, shared: u64) {
        let key = Self::clone_key(&self.ents[s as usize].d);
        let g = self.clones.entry(key).or_default();
        g.members.push((s, shared));
        let (old, new) = (g.s, g.s.max(shared));
        if new != old {
            for &dir in g.per_dir.keys() {
                if let Some(dd) = self.dirs[dir as usize].as_mut() {
                    apply(&mut dd.roll.unique, (new - old) as i64);
                }
            }
            g.s = new;
        }
        let mut p = self.ents[s as usize].parent;
        while p != NONE {
            let c = g.per_dir.entry(p).or_insert(0);
            *c += 1;
            if *c == 1 {
                if let Some(dd) = self.dirs[p as usize].as_mut() {
                    apply(&mut dd.roll.unique, g.s as i64);
                }
            }
            p = self.ents[p as usize].parent;
        }
    }

    fn group_leave(&mut self, s: u32) {
        let key = Self::clone_key(&self.ents[s as usize].d);
        let Some(g) = self.clones.get_mut(&key) else {
            return;
        };
        let mut p = self.ents[s as usize].parent;
        while p != NONE {
            if let Some(c) = g.per_dir.get_mut(&p) {
                *c -= 1;
                if *c == 0 {
                    g.per_dir.remove(&p);
                    if let Some(dd) = self.dirs[p as usize].as_mut() {
                        apply(&mut dd.roll.unique, -(g.s as i64));
                    }
                }
            }
            p = self.ents[p as usize].parent;
        }
        g.members.retain(|m| m.0 != s);
        let new = g.members.iter().map(|m| m.1).max().unwrap_or(0);
        if new != g.s {
            for &dir in g.per_dir.keys() {
                if let Some(dd) = self.dirs[dir as usize].as_mut() {
                    apply(&mut dd.roll.unique, -((g.s - new) as i64));
                }
            }
            g.s = new;
        }
        if g.members.is_empty() {
            self.clones.remove(&key);
        }
    }

    /// Join rollups (and the hard-link group) after the entry is placed.
    fn attach(&mut self, s: u32) {
        let d = self.ents[s as usize].d;
        if !is_countable_kind(d.kind) {
            return;
        }
        if d.nlink > 1 && d.fileid != 0 {
            let v = self.links.entry((d.dev, d.fileid)).or_default();
            v.push(s);
            if v.len() > 1 {
                self.ents[s as usize].d.flags |= FLAG_SECONDARY_LINK;
                return;
            }
        }
        self.contribute(s, 1);
    }

    /// Leave rollups (handing hard-link ownership on) before the entry changes or goes.
    fn detach(&mut self, s: u32) {
        let d = self.ents[s as usize].d;
        if !is_countable_kind(d.kind) {
            return;
        }
        if d.nlink > 1 && d.fileid != 0 {
            let key = (d.dev, d.fileid);
            if let Some(v) = self.links.get_mut(&key) {
                if let Some(pos) = v.iter().position(|&x| x == s) {
                    v.remove(pos);
                    let next = v.first().copied();
                    if v.is_empty() {
                        self.links.remove(&key);
                    }
                    if pos == 0 {
                        self.contribute(s, -1);
                        if let Some(n) = next {
                            self.ents[n as usize].d.flags &= !FLAG_SECONDARY_LINK;
                            self.contribute(n, 1);
                        }
                    }
                    return;
                }
            }
        }
        self.contribute(s, -1);
    }

    /// Remove an entry and its whole subtree. Returns the removed count.
    pub fn remove_subtree(&mut self, s: u32) -> usize {
        let parent = self.ents[s as usize].parent;
        if parent != NONE {
            if let Some(pd) = self.dirs[parent as usize].as_mut() {
                pd.children.retain(|&c| c != s);
            }
        }
        self.remove_detached_subtree(s)
    }

    /// As `remove_subtree`, but the caller already pruned the parent's child list.
    pub fn remove_detached_subtree(&mut self, s: u32) -> usize {
        let mut stack = vec![s];
        let mut all = Vec::new();
        while let Some(x) = stack.pop() {
            all.push(x);
            stack.extend_from_slice(self.children(x));
        }
        // Detach leaves first while every ancestor is still in place.
        for &x in &all {
            self.detach(x);
        }
        for &x in &all {
            self.ents[x as usize].live = false;
            self.dirs[x as usize] = None;
            self.free.push(x);
            self.removed_log.push(x);
        }
        if let Some(i) = self.roots.iter().position(|&r| r == s) {
            self.roots.remove(i);
        }
        self.live -= all.len();
        all.len()
    }

    /// Drop children no longer present: caller passes the set of slots to keep.
    pub fn prune_children(&mut self, dir: u32, keep: &HashSet<u32>) -> Vec<u32> {
        let gone: Vec<u32> = self
            .children(dir)
            .iter()
            .copied()
            .filter(|c| !keep.contains(c))
            .collect();
        if let Some(pd) = self.dirs[dir as usize].as_mut() {
            pd.children.retain(|c| keep.contains(c));
        }
        for &g in &gone {
            self.remove_detached_subtree(g);
        }
        gone
    }

    pub fn path_of(&self, s: u32) -> Vec<u8> {
        let mut parts = Vec::new();
        let mut x = s;
        while x != NONE {
            parts.push(x);
            x = self.ents[x as usize].parent;
        }
        let mut out = Vec::new();
        for (i, &p) in parts.iter().rev().enumerate() {
            let n = self.name_of(p);
            if i > 0 && out != b"/" {
                out.push(b'/');
            }
            out.extend_from_slice(n);
        }
        out
    }

    pub fn depth(&self, s: u32) -> usize {
        let mut n = 0;
        let mut x = self.ents[s as usize].parent;
        while x != NONE {
            n += 1;
            x = self.ents[x as usize].parent;
        }
        n
    }

    pub fn find_child(&self, dir: u32, name: &[u8]) -> Option<u32> {
        self.children(dir)
            .iter()
            .copied()
            .find(|&c| self.name_of(c) == name)
    }

    /// Resolve an absolute path to a slot.
    pub fn lookup(&self, path: &[u8]) -> Option<u32> {
        let path = if path.len() > 1 {
            path.strip_suffix(b"/").unwrap_or(path)
        } else {
            path
        };
        for &r in &self.roots {
            let rp = self.name_of(r);
            if path == rp {
                return Some(r);
            }
            let rest = if rp == b"/" {
                path.strip_prefix(b"/")
            } else {
                path.strip_prefix(rp).and_then(|x| x.strip_prefix(b"/"))
            };
            if let Some(rest) = rest {
                let mut cur = r;
                for comp in rest.split(|&b| b == b'/').filter(|c| !c.is_empty()) {
                    cur = self.find_child(cur, comp)?;
                }
                return Some(cur);
            }
        }
        None
    }

    pub fn root_containing(&self, path: &[u8]) -> Option<u32> {
        self.roots.iter().copied().find(|&r| {
            let rp = self.name_of(r);
            rp == b"/" || path == rp || (path.starts_with(rp) && path.get(rp.len()) == Some(&b'/'))
        })
    }

    pub fn live_slots(&self) -> impl Iterator<Item = u32> + '_ {
        self.ents
            .iter()
            .enumerate()
            .filter(|(_, e)| e.live)
            .map(|(i, _)| i as u32)
    }

    pub fn take_logs(&mut self) -> (Vec<u32>, Vec<u32>) {
        (
            std::mem::take(&mut self.removed_log),
            std::mem::take(&mut self.touched_log),
        )
    }
}

impl Store {
    pub fn package_exts_clone(&self) -> Vec<Vec<u8>> {
        self.package_exts.clone()
    }
}
