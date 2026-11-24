//! The "unlock the local key during onboarding" command body.
//!
//! An app that keeps a local database key in the OS credential store wants the permission prompt to
//! happen at a moment of its choosing (onboarding), not on the first read. The host passes its two
//! credential-store operations; this module runs them off the main thread and implements the
//! check-only branch: a resumed onboarding can show the row as done without creating the key or
//! asking the user to allow access.

/// Run one key-slot request on the calling thread.
///
/// With `check_only`, `state` answers whether the key is already stored (`Some`) and nothing is
/// created. Otherwise `get_or_create` runs and the answer is always `true` once it succeeds.
pub fn ensure_blocking<K>(
    check_only: bool,
    state: impl FnOnce() -> Result<Option<K>, String>,
    get_or_create: impl FnOnce() -> Result<K, String>,
) -> Result<bool, String> {
    if check_only {
        return state().map(|key| key.is_some());
    }
    get_or_create().map(|_| true)
}

/// [`ensure_blocking`] on the blocking pool, for an `async` Tauri command (`check_only` absent
/// means `false`). A join failure is reported as `join credential store: <error>`.
pub async fn ensure<K: Send + 'static>(
    check_only: Option<bool>,
    state: impl FnOnce() -> Result<Option<K>, String> + Send + 'static,
    get_or_create: impl FnOnce() -> Result<K, String> + Send + 'static,
) -> Result<bool, String> {
    let check_only = check_only.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || ensure_blocking(check_only, state, get_or_create))
        .await
        .map_err(|err| format!("join credential store: {err}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_only_never_creates() {
        let created = std::cell::Cell::new(false);
        let answer = ensure_blocking(
            true,
            || Ok(None::<u8>),
            || {
                created.set(true);
                Ok(1)
            },
        );
        assert_eq!(answer, Ok(false));
        assert!(!created.get());
    }

    #[test]
    fn create_answers_true() {
        assert_eq!(
            ensure_blocking(false, || Ok(None::<u8>), || Ok(7)),
            Ok(true)
        );
    }
}
