use rightkit_cache::{Config, DiskCache, Key};

fn key(name: &str) -> Key {
    Key::new(&[name])
}

/// The names from `names` that are still stored, in the order given.
fn kept(cache: &DiskCache, names: &[&str]) -> Vec<String> {
    names
        .iter()
        .copied()
        .filter(|name| cache.contains(&key(name)))
        .map(|name| name.to_string())
        .collect()
}

#[test]
fn puts_and_gets_evict_exactly_the_expected_keys_in_lru_order() {
    let root = tempfile::tempdir().unwrap();
    // Room for four 10-byte entries.
    let cache = DiskCache::open(root.path(), Config::new(40)).unwrap();
    let names = ["a", "b", "c", "d", "e", "f", "g", "h"];
    for name in ["a", "b", "c", "d"] {
        cache.put(&key(name), &[7; 10]).unwrap();
    }
    assert!(cache.get(&key("a")).is_some()); // least recent first: b c d a
    cache.put(&key("e"), &[7; 10]).unwrap(); // evicts b
    assert!(cache.get(&key("c")).is_some()); // d a e c
    cache.put(&key("f"), &[7; 10]).unwrap(); // evicts d
    cache.put(&key("g"), &[7; 10]).unwrap(); // evicts a
    assert!(cache.get(&key("e")).is_some()); // c f g e
    assert!(cache.get(&key("f")).is_some()); // c g e f
    cache.put(&key("h"), &[7; 10]).unwrap(); // evicts c
                                             // `kept` reports in `names` order, so the survivors read alphabetically.
    assert_eq!(kept(&cache, &names), ["e", "f", "g", "h"]);
    let stats = cache.stats();
    assert_eq!((stats.entries, stats.bytes, stats.evictions), (4, 40, 4));
}

#[test]
fn pinned_entries_are_skipped_and_rejoin_the_order_when_unpinned() {
    let root = tempfile::tempdir().unwrap();
    let cache = DiskCache::open(root.path(), Config::new(40)).unwrap();
    let names = ["a", "b", "c", "d", "e", "f", "g", "h"];
    cache.put_pinned(&key("a"), &[7; 10]).unwrap();
    for name in ["b", "c", "d"] {
        cache.put(&key(name), &[7; 10]).unwrap();
    }
    cache.put(&key("e"), &[7; 10]).unwrap(); // evicts b, the oldest unpinned entry
    cache.put(&key("f"), &[7; 10]).unwrap(); // evicts c
    cache.put(&key("g"), &[7; 10]).unwrap(); // evicts d
    assert_eq!(kept(&cache, &names), ["a", "e", "f", "g"]);

    // Unpinned, `a` is the least recently used entry (it was never read), so it goes first.
    assert!(cache.unpin(&key("a")).unwrap());
    cache.put(&key("h"), &[7; 10]).unwrap();
    assert_eq!(kept(&cache, &names), ["e", "f", "g", "h"]);
    assert_eq!(cache.pinned_bytes(), 0);
}

/// The eviction rule the cache has always used, written out directly: after every write, evict
/// the least recently used entries until the total fits the cap. Gets refresh recency.
#[test]
fn random_puts_and_gets_match_the_reference_eviction_rule() {
    const CAP: u64 = 60;
    let root = tempfile::tempdir().unwrap();
    let cache = DiskCache::open(root.path(), Config::new(CAP)).unwrap();
    // Resident entries as (id, size, last use).
    let mut model: Vec<(usize, u64, u64)> = Vec::new();
    let mut clock = 0u64;
    let mut seed: u64 = 42;
    let mut next = move |bound: u64| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % bound
    };
    for _ in 0..600 {
        clock += 1;
        let id = next(20) as usize;
        if next(3) == 0 {
            let hit = cache.get(&key(&id.to_string())).is_some();
            let resident = model.iter_mut().find(|e| e.0 == id);
            assert_eq!(hit, resident.is_some(), "get of {id}");
            if let Some(entry) = resident {
                entry.2 = clock;
            }
        } else {
            let size = next(15) + 1;
            cache
                .put(&key(&id.to_string()), &vec![0u8; size as usize])
                .unwrap();
            model.retain(|e| e.0 != id);
            model.push((id, size, clock));
            while model.iter().map(|e| e.1).sum::<u64>() > CAP {
                let (idx, _) = model.iter().enumerate().min_by_key(|(_, e)| e.2).unwrap();
                model.remove(idx);
            }
        }
        for id in 0..20 {
            assert_eq!(
                cache.contains(&key(&id.to_string())),
                model.iter().any(|e| e.0 == id),
                "residency of {id} after write {clock}"
            );
        }
        assert!(cache.stats().bytes <= CAP);
    }
}
