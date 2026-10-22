//! 0.5.0 store tests: clone groups on the second member, volume-qualified keys, bounded
//! capacity & changed-column commits.

use super::*;
use std::collections::BTreeMap;

fn fd(dev: u32, fileid: u64, cloneid: u64, alloc: u64, private: u64) -> EntryData {
    EntryData {
        kind: KIND_FILE,
        nlink: 1,
        logical: alloc,
        alloc,
        private,
        dev,
        fileid,
        cloneid,
        mtime: 1,
        ..EntryData::default()
    }
}

fn dir_d(dev: u32) -> EntryData {
    EntryData {
        kind: KIND_DIR,
        dev,
        mtime: 1,
        ..EntryData::default()
    }
}

fn anon() -> (Store, u32) {
    let mut s = Store::create(None, Vec::new(), 1).unwrap();
    let r = s.add_root(b"/r", dir_d(1)).unwrap();
    (s, r)
}

/// `unique` of every folder recomputed from the entries alone: the private bytes of every
/// owning file below it, plus, for every clone group with a member below it, the group's
/// largest shared part once (the model in the module docs).
fn brute_unique(s: &Store) -> HashMap<u32, u64> {
    let mut max_shared: HashMap<CloneKey, u64> = HashMap::new();
    let mut owners: Vec<u32> = Vec::new();
    for x in s.live_slots() {
        let e = s.entry(x);
        if e.d.kind == KIND_DIR || e.d.flags & FLAG_SECONDARY_LINK != 0 {
            continue;
        }
        owners.push(x);
        let shared = e.d.alloc - e.d.private;
        if shared > 0 {
            let m = max_shared.entry(CloneKey::of(&e.d)).or_insert(0);
            *m = (*m).max(shared);
        }
    }
    let mut out: HashMap<u32, u64> = HashMap::new();
    let mut seen: HashSet<(u32, CloneKey)> = HashSet::new();
    for x in owners {
        let e = s.entry(x);
        let shared = e.d.alloc - e.d.private;
        let key = CloneKey::of(&e.d);
        let mut p = s.parent_slot(x);
        while p != NONE {
            *out.entry(p).or_insert(0) += e.d.private;
            if shared > 0 && seen.insert((p, key)) {
                *out.get_mut(&p).unwrap() += max_shared[&key];
            }
            p = s.parent_slot(p);
        }
    }
    out
}

fn assert_unique_matches(s: &Store) {
    let want = brute_unique(s);
    for x in s.live_slots() {
        if s.entry(x).d.kind == KIND_DIR {
            assert_eq!(
                s.rollup(x).unique,
                want.get(&x).copied().unwrap_or(0),
                "unique of {}",
                String::from_utf8_lossy(&s.path_of(x))
            );
        }
    }
}

#[allow(dead_code)]
fn dump(s: &Store) -> BTreeMap<Vec<u8>, String> {
    s.live_slots()
        .map(|x| {
            (
                s.path_of(x),
                format!("{:?} {:?}", s.entry(x).d, s.rollup(x)),
            )
        })
        .collect()
}

// ------------------------------------------------------------------ clone groups

#[test]
fn a_single_member_allocates_no_group_and_a_second_promotes_it() {
    let (mut s, r) = anon();
    let a = s.add_child(r, b"a", dir_d(1)).unwrap();
    let b = s.add_child(r, b"b", dir_d(1)).unwrap();
    let f1 = s.add_child(a, b"f1", fd(1, 10, 7, 8192, 4096)).unwrap();
    assert_eq!(s.aux_counts(), (0, 1, 1));
    assert_eq!(s.multi_clone_groups(), 0, "one member: a table entry");
    assert_eq!(s.rollup(a).unique, 4096 + 4096);
    assert_eq!(s.rollup(r).unique, 8192);
    assert_unique_matches(&s);

    let f2 = s.add_child(b, b"f2", fd(1, 11, 7, 12288, 4096)).unwrap();
    assert_eq!(s.aux_counts(), (0, 1, 2));
    assert_eq!(s.multi_clone_groups(), 1, "second member: a group");
    // The group's largest shared part (8192) counts once in every folder holding a member.
    assert_eq!(s.rollup(a).unique, 4096 + 8192);
    assert_eq!(s.rollup(b).unique, 4096 + 8192);
    assert_eq!(s.rollup(r).unique, 4096 + 4096 + 8192);
    assert_unique_matches(&s);

    // A third member with a larger shared part raises every holder; removing it lowers them.
    let f3 = s.add_child(a, b"f3", fd(1, 12, 7, 20480, 4096)).unwrap();
    assert_eq!(s.rollup(b).unique, 4096 + 16384);
    assert_unique_matches(&s);
    s.remove_subtree(f3);
    assert_eq!(s.rollup(b).unique, 4096 + 8192);
    assert_unique_matches(&s);

    s.remove_subtree(f1);
    assert_eq!(
        s.multi_clone_groups(),
        0,
        "one member left: back to a table entry"
    );
    assert_eq!(s.aux_counts(), (0, 1, 1));
    assert_eq!(s.rollup(a).unique, 0);
    assert_eq!(s.rollup(b).unique, 4096 + 8192);
    assert_eq!(s.rollup(r).unique, 4096 + 8192);
    assert_unique_matches(&s);

    s.remove_subtree(f2);
    assert_eq!(s.aux_counts(), (0, 0, 0));
    assert_eq!(s.rollup(r).unique, 0);
}

#[test]
fn removing_the_largest_member_lowers_the_group_and_demotes_to_the_survivor() {
    let (mut s, r) = anon();
    let a = s.add_child(r, b"a", dir_d(1)).unwrap();
    let b = s.add_child(r, b"b", dir_d(1)).unwrap();
    let big = s
        .add_child(a, b"big", fd(1, 1, 3, 16384 + 4096, 4096))
        .unwrap();
    let small = s
        .add_child(b, b"small", fd(1, 2, 3, 4096 + 4096, 4096))
        .unwrap();
    assert_eq!(s.multi_clone_groups(), 1);
    assert_eq!(s.rollup(b).unique, 4096 + 16384);
    s.remove_subtree(big);
    assert_eq!(s.multi_clone_groups(), 0);
    assert_eq!(
        s.rollup(b).unique,
        4096 + 4096,
        "the survivor's own shared part"
    );
    assert_eq!(s.rollup(r).unique, 4096 + 4096);
    assert_unique_matches(&s);
    s.remove_subtree(small);
    assert_eq!(s.aux_counts(), (0, 0, 0));
}

#[test]
fn clone_keys_do_not_merge_groups_of_different_devices() {
    // The measured root store held a second volume's whole tree. Clone ids and file ids are
    // per volume, so unrelated files of the two volumes shared a group, and the group's
    // largest shared part (here 16384) was charged to folders holding only the small one:
    // `allocated_unique` came out 12,038,144 B higher on the real store.
    let (mut s, r) = anon();
    let a = s.add_child(r, b"a", dir_d(1)).unwrap();
    let b = s.add_child(r, b"b", dir_d(2)).unwrap();
    s.add_child(a, b"x", fd(1, 10, 5, 8192, 4096)).unwrap();
    s.add_child(b, b"y", fd(2, 10, 5, 20480, 4096)).unwrap();
    assert_eq!(s.aux_counts().1, 2, "one group per volume");
    assert_eq!(s.multi_clone_groups(), 0);
    assert_eq!(
        s.rollup(a).unique,
        4096 + 4096,
        "not raised to the other volume's 16384"
    );
    assert_eq!(s.rollup(b).unique, 4096 + 16384);
    assert_unique_matches(&s);
}

#[test]
fn shared_extents_without_a_clone_id_never_merge_with_a_clone_id() {
    let (mut s, r) = anon();
    let a = s.add_child(r, b"a", dir_d(1)).unwrap();
    // Fallback key: file id 9 (no clone id reported). Clone id 9 is another group.
    s.add_child(a, b"x", fd(1, 9, 0, 8192, 4096)).unwrap();
    s.add_child(a, b"y", fd(1, 3, 9, 8192, 4096)).unwrap();
    assert_eq!(s.aux_counts().1, 2);
    assert_eq!(s.multi_clone_groups(), 0);
    assert_unique_matches(&s);
}

#[test]
fn random_churn_keeps_unique_equal_to_a_recount_and_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(Some(dir.path()), Vec::new(), 7).unwrap();
    let r = s.add_root(b"/r", dir_d(1)).unwrap();
    let mut folders = vec![r];
    for i in 0..12 {
        let p = folders[i / 3];
        folders.push(
            s.add_child(p, format!("d{i}").as_bytes(), dir_d(1))
                .unwrap(),
        );
    }
    let mut x = 0x2545_f491_4f6c_dd1du64;
    let mut rnd = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut files: Vec<u32> = Vec::new();
    let mut step_no = 0u64;
    let mut run = |s: &mut Store, files: &mut Vec<u32>, steps: u64| {
        for _ in 0..steps {
            step_no += 1;
            match rnd() % 5 {
                0..=1 => {
                    let dir = folders[(rnd() % folders.len() as u64) as usize];
                    let dev = 1 + (rnd() % 2) as u32;
                    let private = 4096 * (rnd() % 3 + 1);
                    let alloc = private + 4096 * (rnd() % 4);
                    let d = fd(dev, step_no + 100, rnd() % 6, alloc, private);
                    let slot = s
                        .add_child(dir, format!("f{step_no}").as_bytes(), d)
                        .unwrap();
                    files.push(slot);
                }
                2 if !files.is_empty() => {
                    let i = (rnd() % files.len() as u64) as usize;
                    let f = files.swap_remove(i);
                    s.remove_subtree(f);
                }
                _ if !files.is_empty() => {
                    let i = (rnd() % files.len() as u64) as usize;
                    let f = files[i];
                    let mut d = s.entry(f).d;
                    d.private = 4096 * (rnd() % 3 + 1);
                    d.alloc = d.private + 4096 * (rnd() % 4);
                    d.cloneid = rnd() % 6;
                    s.update(f, d);
                }
                _ => {}
            }
            if step_no.is_multiple_of(97) {
                assert_unique_matches(s);
            }
        }
    };
    run(&mut s, &mut files, 1500);
    assert_unique_matches(&s);
    let counts = s.aux_counts();
    let rollups: Vec<Rollup> = folders.iter().map(|&f| s.rollup(f)).collect();
    s.commit(5).unwrap();
    drop(s);

    let (mut s2, _) = Store::open_existing(dir.path(), Vec::new(), 7)
        .unwrap()
        .unwrap();
    assert_eq!(
        s2.aux_counts(),
        counts,
        "the tables rebuilt at open match the live ones"
    );
    assert_eq!(
        folders.iter().map(|&f| s2.rollup(f)).collect::<Vec<_>>(),
        rollups
    );
    // And the rebuilt tables carry on correctly.
    run(&mut s2, &mut files, 600);
    assert_unique_matches(&s2);
}

#[test]
fn singleton_clone_tables_are_small() {
    let (mut s, r) = anon();
    let a = s.add_child(r, b"a", dir_d(1)).unwrap();
    for i in 0..20_000u64 {
        s.add_child(
            a,
            format!("c{i}").as_bytes(),
            fd(1, i + 2, i + 1, 8192, 4096),
        )
        .unwrap();
    }
    assert_eq!(s.multi_clone_groups(), 0);
    let parts = s.heap_parts();
    assert_eq!(
        parts.clone_multi, 0,
        "no Group, no per-folder table, no member list"
    );
    // A table entry is a key (16 B) and a slot (4 B, padded): well under the 88 B pair plus
    // 80 B group plus allocations a one-member group used to cost.
    assert!(
        parts.clone_solo / 20_000 <= 64,
        "{} B per singleton",
        parts.clone_solo / 20_000
    );
    assert_unique_matches(&s);
}

#[test]
fn hard_link_ownership_handoff_keeps_clone_membership() {
    let (mut s, r) = anon();
    let a = s.add_child(r, b"a", dir_d(1)).unwrap();
    let b = s.add_child(r, b"b", dir_d(1)).unwrap();
    let mut link = fd(1, 77, 4, 8192, 4096);
    link.nlink = 2;
    let l1 = s.add_child(a, b"l1", link).unwrap();
    let l2 = s.add_child(b, b"l2", link).unwrap();
    assert!(s.kind_flags(l2).1 & FLAG_SECONDARY_LINK != 0);
    assert_eq!(s.aux_counts().2, 1, "only the owner is a clone member");
    assert_unique_matches(&s);
    s.remove_subtree(l1);
    assert!(
        s.kind_flags(l2).1 & FLAG_SECONDARY_LINK == 0,
        "ownership handed on"
    );
    assert_eq!(s.aux_counts().2, 1);
    assert_eq!(s.rollup(b).unique, 4096 + 4096);
    assert_unique_matches(&s);
}

// ------------------------------------------------------------------ capacity

fn build(dir: &Path, n: usize) -> Store {
    let mut s = Store::create(Some(dir), Vec::new(), 7).unwrap();
    let root = s.add_root(b"/r", dir_d(1)).unwrap();
    let mut dirs = vec![root];
    for i in 0..(n / 8) {
        let p = dirs[i / 8];
        dirs.push(
            s.add_child(p, format!("d{i}").as_bytes(), dir_d(1))
                .unwrap(),
        );
    }
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    for i in 0..(n - n / 8) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let d = dirs[(x % dirs.len() as u64) as usize];
        s.add_child(
            d,
            format!("file{i}.dat").as_bytes(),
            fd(1, i as u64 + 2, 0, 4096, 4096),
        )
        .unwrap();
    }
    s.commit(1).unwrap();
    s
}

#[test]
fn settled_bytes_per_entry_before_and_after_bounded_growth() {
    let n = 200_000;
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    crate::col::LEGACY_GROWTH.with(|l| l.set(true));
    let before = build(a.path(), n);
    crate::col::LEGACY_GROWTH.with(|l| l.set(false));
    let after = build(b.path(), n);
    assert_eq!(before.len(), after.len());
    let (oc, nc, live) = (
        before.capacity_bytes() as f64,
        after.capacity_bytes() as f64,
        after.live_bytes() as f64,
    );
    let per = |x: f64| x / after.len() as f64;
    eprintln!(
        "200k entries, settled: doubling {:.1} B/entry ({:.1}% unused), bounded {:.1} B/entry ({:.1}% unused), live payload {:.1} B/entry",
        per(oc),
        100.0 * before.unused_bytes() as f64 / oc,
        per(nc),
        100.0 * after.unused_bytes() as f64 / nc,
        per(live)
    );
    assert!(nc < oc, "bounded growth allocates less than doubling");
    assert!(
        nc <= live * 1.25,
        "unused capacity stays bounded: {nc} of live {live}"
    );
    assert!(
        !after.wants_compaction(0),
        "a settled store does not ask for a rewrite"
    );
}

#[test]
fn a_rewrite_sized_to_need_settles_tight_and_keeps_every_rollup() {
    let dir = tempfile::tempdir().unwrap();
    let old = build(dir.path(), 100_000);
    let out = tempfile::tempdir().unwrap();
    let hint = old.copy_hint();
    assert_eq!(hint.slots, old.len());
    assert_eq!(hint.dirs, old.dir_count());
    let mut fresh = Store::create_sized(Some(out.path()), Vec::new(), 7, hint).unwrap();
    old.copy_into(&mut fresh).unwrap();
    fresh.commit(1).unwrap();
    assert_eq!(fresh.len(), old.len());
    assert_eq!(
        fresh.names.len(),
        hint.names,
        "exactly the names the tree uses"
    );
    assert_eq!(fresh.rollup(fresh.roots[0]), old.rollup(old.roots[0]));
    // `build` leaves the old store nearly tight already, and a sized rewrite adds up to a
    // sixteenth of headroom per column: it may not exceed that, whatever the old looked like.
    assert!(
        fresh.capacity_bytes() <= old.capacity_bytes() + old.capacity_bytes() / 16,
        "fresh {} old {}",
        fresh.capacity_bytes(),
        old.capacity_bytes()
    );
    assert!(
        fresh.capacity_bytes() as f64 <= fresh.live_bytes() as f64 * 1.15,
        "capacity {} live {}",
        fresh.capacity_bytes(),
        fresh.live_bytes()
    );
    assert!(!fresh.wants_compaction(0));
}

#[test]
fn capacity_stays_bounded_under_churn_at_a_constant_size() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = build(dir.path(), 60_000);
    let peak_slots = s.slots();
    let names = s.names.len();
    for round in 0..5usize {
        let victims: Vec<u32> = s
            .live_slots()
            .filter(|&x| s.kind_flags(x).0 == KIND_FILE)
            .take(20_000)
            .collect();
        let mut gone: Vec<(Vec<u8>, u32)> = Vec::new();
        for v in victims {
            let parent = s.parent_slot(v);
            gone.push((s.name_of(v).to_vec(), parent));
            s.remove_subtree(v);
        }
        for (i, (name, parent)) in gone.into_iter().enumerate() {
            // Recycled names: the name store does not grow, the slots are reused.
            let id = 1_000_000 + (round * 20_000 + i) as u64;
            s.add_child(parent, &name, fd(1, id, 0, 4096, 4096))
                .unwrap();
        }
        assert!(
            s.slots() <= peak_slots,
            "round {round}: freed slots are reused ({} > {peak_slots})",
            s.slots()
        );
        assert_eq!(s.names.len(), names);
        assert!(
            s.unused_bytes() * 5 <= s.capacity_bytes(),
            "round {round}: unused {} of {}",
            s.unused_bytes(),
            s.capacity_bytes()
        );
        s.commit(round as u64 + 2).unwrap();
    }
    assert!(!s.wants_compaction(0));
}

#[test]
fn many_dead_slots_ask_for_a_rewrite_and_a_rewrite_ends_the_request() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = build(dir.path(), 60_000);
    let victims: Vec<u32> = s
        .live_slots()
        .filter(|&x| s.kind_flags(x).0 == KIND_FILE)
        .take(40_000)
        .collect();
    for v in victims {
        s.remove_subtree(v);
    }
    assert!(s.wants_compaction(0), "two thirds of the files are gone");
    let out = tempfile::tempdir().unwrap();
    let mut fresh = Store::create_sized(Some(out.path()), Vec::new(), 7, s.copy_hint()).unwrap();
    s.copy_into(&mut fresh).unwrap();
    fresh.commit(1).unwrap();
    assert!(!fresh.wants_compaction(0));
    assert!(fresh.capacity_bytes() * 10 < s.capacity_bytes() * 6);
    assert_eq!(fresh.rollup(fresh.roots[0]), s.rollup(s.roots[0]));
}

// ------------------------------------------------------------------ changed-column commits

fn big_store(dir: &Path) -> (Store, u32) {
    let mut s = Store::create(Some(dir), Vec::new(), 7).unwrap();
    let root = s.add_root(b"/r", dir_d(1)).unwrap();
    for i in 0..50_000 {
        s.add_child(
            root,
            format!("f{i}").as_bytes(),
            fd(1, i + 2, 0, 4096, 4096),
        )
        .unwrap();
    }
    (s, root)
}

#[test]
fn a_commit_writes_only_the_dirty_columns_and_a_clean_store_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let page = crate::col::page_size() as u64;
    let (mut s, root) = big_store(dir.path());
    s.commit(1).unwrap();
    s.take_flushed_bytes();

    // Nothing changed, same event: no column, no header, no byte.
    s.commit(1).unwrap();
    assert_eq!(s.take_flushed_bytes(), 0);
    assert_eq!(s.pending_write_bytes(), 0);

    // One flag on one entry dirties one page of one column.
    let f = s.find_child(root, b"f25000").unwrap();
    s.set_flag(f, FLAG_HIDDEN);
    assert_eq!(
        s.pending_write_bytes(),
        2 * page,
        "the flags page and the header"
    );
    let mut seen: Vec<String> = Vec::new();
    s.commit_with(2, |c| {
        seen.push(c.to_string());
        Ok(())
    })
    .unwrap();
    assert_eq!(seen, vec!["c_flags".to_string(), "header".to_string()]);
    // set_flag made the dirty header durable first (one page); the commit wrote the flags
    // page and the header.
    assert_eq!(s.take_flushed_bytes(), 3 * page);
    assert_eq!(s.pending_write_bytes(), 0);
}

#[test]
fn an_idle_commit_with_a_new_event_id_writes_the_header_page_only() {
    let dir = tempfile::tempdir().unwrap();
    let page = crate::col::page_size() as u64;
    let (mut s, _) = big_store(dir.path());
    s.commit(1).unwrap();
    s.take_flushed_bytes();
    assert_eq!(s.pending_commit_bytes(1), 0);
    assert_eq!(s.pending_commit_bytes(2), page);
    s.commit(2).unwrap();
    assert_eq!(s.take_flushed_bytes(), page);
    s.commit(2).unwrap();
    assert_eq!(s.take_flushed_bytes(), 0);
    // An explicit snapshot rewrites the header even when nothing changed.
    s.commit_forced(2).unwrap();
    assert_eq!(s.take_flushed_bytes(), page);
    // And what was written reads back.
    drop(s);
    let (s2, info) = Store::open_existing(dir.path(), Vec::new(), 7)
        .unwrap()
        .unwrap();
    assert_eq!(info.event_id, 2);
    assert_eq!(s2.len(), 50_001);
}

#[test]
fn a_clean_commit_still_refreshes_a_stale_header_stamp() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, _) = big_store(dir.path());
    s.commit(1).unwrap();
    s.take_flushed_bytes();
    s.committed_at -= HEADER_REFRESH_SECS + 1;
    s.commit(1).unwrap();
    assert!(
        s.take_flushed_bytes() > 0,
        "an hour-old confirmation is renewed even with nothing to write"
    );
}

// ------------------------------------------------------------------ mount boundaries

#[test]
fn legacy_clone_keys_keep_incremental_rollups_coherent_until_rewrite() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(Some(dir.path()), Vec::new(), 7).unwrap();
    s.legacy_clone_keys = true;
    let r = s.add_root(b"/r", dir_d(1)).unwrap();
    let a = s.add_child(r, b"a", dir_d(1)).unwrap();
    let b = s.add_child(r, b"b", dir_d(2)).unwrap();
    s.add_child(a, b"x", fd(1, 10, 9, 8192, 4096)).unwrap();
    let y = s.add_child(b, b"y", fd(2, 11, 9, 20480, 4096)).unwrap();
    assert_eq!(s.rollup(a).unique, 20480); // Legacy cross-device maximum.
    s.commit(1).unwrap();
    drop(s);
    let disk_bytes = || -> BTreeMap<_, _> {
        std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), read_state_file(&entry.path()))
            })
            .collect()
    };
    let before_open = disk_bytes();
    let (mut s, _) = Store::open_existing(dir.path(), Vec::new(), 7)
        .unwrap()
        .unwrap();
    assert_eq!(
        disk_bytes(),
        before_open,
        "legacy open must not rewrite files"
    );
    assert_eq!(s.rollup(a).unique, 20480);
    assert!(s.clone_rollups_need_rewrite());
    s.remove_subtree(y);
    assert_eq!(s.rollup(a).unique, 8192); // Subtraction uses the same legacy algebra.
    assert_eq!(s.rollup(b).unique, 0);
    s.commit(2).unwrap();
    assert_eq!(&s.hdr.as_ref().unwrap()[..8], LEGACY_MAGIC);
    s.add_child(b, b"y", fd(2, 11, 9, 20480, 4096)).unwrap();
    let out = tempfile::tempdir().unwrap();
    let mut fresh = Store::create_sized(Some(out.path()), Vec::new(), 7, s.copy_hint()).unwrap();
    s.copy_into(&mut fresh).unwrap();
    fresh.commit(2).unwrap();
    assert!(!fresh.clone_rollups_need_rewrite());
    assert_eq!(fresh.rollup(fresh.lookup(b"/r/a").unwrap()).unique, 8192);
    assert_unique_matches(&fresh);
    assert_eq!(&fresh.hdr.as_ref().unwrap()[..8], MAGIC);
    drop(fresh);
    let (fresh, _) = Store::open_existing(out.path(), Vec::new(), 7)
        .unwrap()
        .unwrap();
    assert!(!fresh.clone_rollups_need_rewrite());
    assert_unique_matches(&fresh);
}

#[test]
fn unique_name_churn_is_garbage_and_rewrite_stops_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = build(dir.path(), 60_000);
    let slots = s.slots();
    let files: Vec<_> = s
        .live_slots()
        .filter(|&x| s.kind_flags(x).0 == KIND_FILE)
        .collect();
    for round in 0..4 {
        for &x in &files {
            let entry = s.entry(x);
            let parent = s.parent_slot(x);
            s.remove_subtree(x);
            let reused = s
                .add_child(
                    parent,
                    format!("renamed-{round}-{x}-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx").as_bytes(),
                    entry.d,
                )
                .unwrap();
            assert_eq!(reused, x);
        }
    }
    assert_eq!(s.slots(), slots);
    assert!(s.wants_compaction(0));
    let expected = s.live_bytes();
    s.commit(3).unwrap();
    drop(s);
    let (s, _) = Store::open_existing(dir.path(), Vec::new(), 7)
        .unwrap()
        .unwrap();
    assert_eq!(s.live_bytes(), expected);
    let out = tempfile::tempdir().unwrap();
    let mut fresh = Store::create_sized(Some(out.path()), Vec::new(), 7, s.copy_hint()).unwrap();
    s.copy_into(&mut fresh).unwrap();
    assert_eq!(fresh.live_bytes(), expected);
    assert!(!fresh.wants_compaction(0));
    assert_eq!(fresh.rollup(fresh.roots[0]), s.rollup(s.roots[0]));
}
