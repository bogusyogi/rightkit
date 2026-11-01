//! rightkit-cache: a size-capped LRU disk cache. Fills it past its cap and checks the exact eviction order, reopens it to
//! prove recency and accounting persist, refuses an oversize entry, commits a producer file through a reservation,
//! fetch-or-cache, group invalidation, and a CacheSet namespace budget.
use rightkit_cache::{CacheSet, Config, DiskCache, Key, PutOutcome};
use serde_json::{json, Value};

fn ensure(cond: bool, what: &str) -> Result<(), String> {
    if cond {
        Ok(())
    } else {
        Err(format!("rightkit-cache: {what}"))
    }
}

fn e<T: std::fmt::Display>(what: &str) -> impl FnOnce(T) -> String + '_ {
    move |err| format!("rightkit-cache: {what}: {err}")
}

fn key(name: &str) -> Key {
    Key::new(&[name])
}

fn kept(cache: &DiskCache, names: &[&str]) -> Vec<String> {
    names.iter().copied().filter(|n| cache.contains(&key(n))).map(str::to_string).collect()
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let root = ctx.dir("cache")?.join("lru");
    let names = ["a", "b", "c", "d", "e", "f", "g", "h"];
    let cfg = Config::new(40).owner_only();
    let cache = DiskCache::open(&root, cfg).map_err(e("open"))?;

    // Room for four 10-byte entries; each put past the cap evicts the least recently used, gets refresh recency.
    for n in ["a", "b", "c", "d"] {
        ensure(cache.put(&key(n), &[7; 10]).map_err(e("put"))? == PutOutcome::Stored, "put not stored")?;
    }
    ensure(cache.get(&key("a")).is_some(), "get a")?; // LRU order: b c d a
    cache.put(&key("e"), &[7; 10]).map_err(e("put e"))?; // evicts b
    ensure(cache.get(&key("c")).is_some(), "get c")?; // d a e c
    cache.put(&key("f"), &[7; 10]).map_err(e("put f"))?; // evicts d
    cache.put(&key("g"), &[7; 10]).map_err(e("put g"))?; // evicts a
    ensure(cache.get(&key("e")).is_some() && cache.get(&key("f")).is_some(), "get e/f")?; // c g e f
    cache.put(&key("h"), &[7; 10]).map_err(e("put h"))?; // evicts c
    let survivors = kept(&cache, &names);
    ensure(survivors == ["e", "f", "g", "h"], &format!("survivors after eviction were {survivors:?}, expected e f g h"))?;
    let s = cache.stats();
    ensure((s.entries, s.bytes, s.evictions) == (4, 40, 4), &format!("stats were {s:?}"))?;
    ensure(cache.get(&key("a")).is_none(), "an evicted key was still readable")?;

    // Owner-only entries on unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let p = cache.get_path(&key("h")).ok_or("rightkit-cache: no path for a stored entry")?;
        let mode = std::fs::metadata(&p).map_err(e("stat entry"))?.permissions().mode() & 0o777;
        ensure(mode & 0o077 == 0, &format!("owner_only entry has mode {mode:o}"))?;
    }

    // Refuse an entry over the cap without disturbing the rest.
    ensure(cache.put(&key("huge"), &[1; 100]).map_err(e("put huge"))? == PutOutcome::TooLarge, "an oversize entry was stored")?;
    ensure(kept(&cache, &names) == ["e", "f", "g", "h"], "an oversize put disturbed the cache")?;

    // Recency and accounting survive a restart.
    drop(cache);
    let cache = DiskCache::open(&root, cfg).map_err(e("reopen"))?;
    ensure(kept(&cache, &names) == ["e", "f", "g", "h"] && cache.stats().bytes == 40, "reopen lost entries")?;
    ensure(cache.get(&key("e")).as_deref() == Some([7u8; 10].as_slice()), "reopened content differs")?;

    // Reservation: a producer writes a temp file, commit moves it in atomically; abort leaves nothing.
    let rk = Key::grouped(&["thumbs", "asset-1"], &["320"]);
    cache.set_budget(1000).map_err(e("set_budget"))?;
    let res = cache.reserve_path(&rk, "jpg").map_err(e("reserve_path"))?;
    let tmp = res.path().to_path_buf();
    std::fs::write(&tmp, b"JPEGDATA").map_err(e("write producer output"))?;
    ensure(!cache.contains(&rk), "reserved entry visible before commit")?;
    ensure(res.commit().map_err(e("commit"))? == PutOutcome::Stored && !tmp.exists(), "commit did not move the file")?;
    ensure(cache.get(&rk).as_deref() == Some(b"JPEGDATA".as_slice()), "committed content")?;
    let aborted = cache.reserve_path(&Key::new(&["never"]), "bin").map_err(e("reserve 2"))?;
    let atmp = aborted.path().to_path_buf();
    std::fs::write(&atmp, b"x").map_err(e("write 2"))?;
    aborted.abort();
    ensure(!atmp.exists() && !cache.contains(&Key::new(&["never"])), "abort left data behind")?;

    // Group invalidation and fetch-or-cache.
    let g1 = Key::grouped(&["acct"], &["one"]);
    let g2 = Key::grouped(&["acct"], &["two"]);
    cache.put(&g1, b"1").map_err(e("put g1"))?;
    cache.put(&g2, b"2").map_err(e("put g2"))?;
    ensure(cache.clear_group(&["acct"]) == 2 && !cache.contains(&g1), "clear_group")?;
    let mut fetched = 0;
    let fk = Key::url("https://example.invalid/x.png");
    for _ in 0..2 {
        let bytes = cache.get_or_fetch::<String>(&fk, || { fetched += 1; Ok(b"png".to_vec()) })?;
        ensure(bytes == b"png", "get_or_fetch bytes")?;
    }
    ensure(fetched == 1, &format!("get_or_fetch fetched {fetched} times, expected 1 (second is a hit)"))?;

    // CacheSet: two namespaces under one combined budget.
    let set_root = ctx.dir("cache")?.join("set");
    let mut set = CacheSet::new().with_budget(30);
    for ns in ["alpha", "beta"] {
        let c = DiskCache::open(set_root.join(ns), Config::new(1000)).map_err(e("open ns"))?;
        set.add(ns, c).map_err(e("set add"))?;
    }
    for i in 0..3 {
        set.get("alpha").ok_or("rightkit-cache: namespace alpha missing")?.put(&key(&format!("a{i}")), &[1; 10]).map_err(e("put alpha"))?;
        set.get("beta").ok_or("rightkit-cache: namespace beta missing")?.put(&key(&format!("b{i}")), &[2; 10]).map_err(e("put beta"))?;
    }
    let before = set.stats();
    let evicted = set.enforce_budget();
    let after = set.stats();
    ensure(evicted > 0 && after.bytes <= 30, &format!("combined budget not enforced: before {before:?} evicted {evicted} after {after:?}"))?;

    Ok(json!({ "survivors": survivors, "evictions": s.evictions, "set_evicted": evicted, "set_bytes_after": after.bytes }))
}
