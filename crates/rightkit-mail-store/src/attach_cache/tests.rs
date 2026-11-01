use super::*;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

// The cache dir is resolved once per process via OnceLock inside `cache()`,
// through `crate::db::data_dir()`, which is the process-wide test sandbox
// (`crate::test_root`) in test binaries. Serialize the tests that share it.
static CACHE_LOCK: Mutex<()> = Mutex::new(());

fn with_temp_data_dir<T>(f: impl FnOnce() -> T) -> T {
    let _guard = CACHE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    f()
}

fn read_back(account: &str, message: &str, attachment: &str) -> Option<Vec<u8>> {
    cache().ok()?.get(&key(account, message, attachment))
}

// OnceLock memoizes the cache root for the process, so each test uses its own
// account/message/attachment ids rather than its own directory.

#[tokio::test]
async fn get_or_fetch_writes_through_then_reads_back() {
    with_temp_data_dir(|| {});
    let account = "acct-roundtrip";
    assert!(read_back(account, "msg-1", "att-1").is_none());

    let bytes = get_or_fetch(account, "msg-1", "att-1", || async {
        Ok(b"hello attachment bytes".to_vec())
    })
    .await
    .unwrap();
    assert_eq!(bytes, b"hello attachment bytes");
    assert_eq!(
        read_back(account, "msg-1", "att-1").as_deref(),
        Some(b"hello attachment bytes".as_slice()),
        "the fetched bytes must be written through to the cache"
    );
}

#[tokio::test]
async fn get_or_fetch_hits_cache_without_calling_fetch_again() {
    with_temp_data_dir(|| {});
    let account = "acct-getorfetch";
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));

    let calls_c = calls.clone();
    let bytes1 = get_or_fetch(account, "msg-2", "att-2", || async move {
        calls_c.fetch_add(1, Ordering::Relaxed);
        Ok(b"network bytes".to_vec())
    })
    .await
    .unwrap();
    assert_eq!(bytes1, b"network bytes");
    assert_eq!(calls.load(Ordering::Relaxed), 1);

    // Second call must be a cache hit: fetch must NOT run again.
    let calls_c2 = calls.clone();
    let bytes2 = get_or_fetch(account, "msg-2", "att-2", || async move {
        calls_c2.fetch_add(1, Ordering::Relaxed);
        Ok(b"should not be called".to_vec())
    })
    .await
    .unwrap();
    assert_eq!(bytes2, b"network bytes");
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "fetch must not run on a cache hit"
    );
}

#[tokio::test]
async fn fetch_error_leaves_cache_untouched() {
    with_temp_data_dir(|| {});
    let result = get_or_fetch("acct-fetch-error", "msg-3", "att-3", || async {
        Err("network down".to_string())
    })
    .await;
    assert!(result.is_err());
    assert!(read_back("acct-fetch-error", "msg-3", "att-3").is_none());
}

#[tokio::test]
async fn clear_account_removes_only_that_accounts_entries() {
    with_temp_data_dir(|| {});
    let account_a = "acct-clear-a";
    let account_b = "acct-clear-b";

    get_or_fetch(account_a, "m1", "a1", || async { Ok(b"a-bytes".to_vec()) })
        .await
        .unwrap();
    get_or_fetch(account_b, "m1", "a1", || async { Ok(b"b-bytes".to_vec()) })
        .await
        .unwrap();
    assert!(read_back(account_a, "m1", "a1").is_some());
    assert!(read_back(account_b, "m1", "a1").is_some());

    let removed = clear_account(account_a).unwrap();
    assert_eq!(removed, 1);

    assert!(
        read_back(account_a, "m1", "a1").is_none(),
        "account A's entry must be gone"
    );
    assert!(
        read_back(account_b, "m1", "a1").is_some(),
        "account B's entry must survive clearing account A"
    );
}
