use rightkit_cache::{Config, DiskCache, Key, PutOutcome};

#[test]
fn cap_lru_recency_survives_reopen_and_group_clear_is_durable() {
    let root = tempfile::tempdir().unwrap();
    let config = Config::new(8).max_entry_bytes(8);
    let old = Key::grouped(&["account-a"], &["old"]);
    let touched = Key::grouped(&["account-a"], &["touched"]);
    let newest = Key::grouped(&["account-a"], &["newest"]);

    {
        let cache = DiskCache::open(root.path(), config).unwrap();
        assert_eq!(cache.put(&old, b"1111").unwrap(), PutOutcome::Stored);
        assert_eq!(cache.put(&touched, b"2222").unwrap(), PutOutcome::Stored);
        assert_eq!(cache.get(&old).as_deref(), Some(b"1111".as_slice()));
        assert_eq!(cache.put(&newest, b"3333").unwrap(), PutOutcome::Stored);
        assert!(
            !cache.contains(&touched),
            "least recently used row must be evicted"
        );
        assert!(cache.contains(&old));
        assert!(cache.contains(&newest));
    }

    {
        let cache = DiskCache::open(root.path(), config).unwrap();
        assert_eq!(cache.get(&old).as_deref(), Some(b"1111".as_slice()));
        assert_eq!(cache.get(&newest).as_deref(), Some(b"3333".as_slice()));
        assert_eq!(cache.clear_group(&["account-a"]), 2);
        assert!(cache.get(&old).is_none());
        assert!(cache.get(&newest).is_none());
    }
}

#[test]
fn shrinking_cap_reconciles_files_on_reopen_and_too_large_is_non_destructive() {
    let root = tempfile::tempdir().unwrap();
    let first = Key::new(&["first"]);
    let second = Key::new(&["second"]);
    {
        let cache = DiskCache::open(root.path(), Config::new(12)).unwrap();
        cache.put(&first, b"123456").unwrap();
        cache.put(&second, b"abcdef").unwrap();
    }
    let cache = DiskCache::open(root.path(), Config::new(5).max_entry_bytes(3)).unwrap();
    assert!(cache.get(&first).is_none());
    assert!(cache.get(&second).is_none());
    assert_eq!(
        cache.put(&first, b"too-large").unwrap(),
        PutOutcome::TooLarge
    );
    assert_eq!(cache.stats().entries, 0);
}
