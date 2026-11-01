//! Process-global test coordination for state Rust cannot isolate per test.

use std::sync::{Mutex, MutexGuard, OnceLock};

/// In-test `MemoryBackend` holding rows in process memory. Stands in for the
/// Pulse-backed store so tests can seed, recall and inspect memory without a
/// Membrane CLI.
pub(crate) struct FakeMemoryBackend {
    binding: rightkit_memory::backend::BackendBinding,
    rows: Mutex<Vec<FakeRow>>,
}

struct FakeRow {
    id: String,
    scope: String,
    tier: rightkit_memory::backend::MemoryTier,
    content: String,
    keywords: Vec<String>,
    inject_count: u32,
}

type FakeError = rightkit_memory::backend::MemoryBackendError;
type FakeCtx = rightkit_memory::backend::CallContext;
type FakeEntry = rightkit_memory::backend::MemoryEntry;

impl FakeMemoryBackend {
    pub(crate) fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            binding: rightkit_memory::backend::BackendBinding::new(
                rightkit_memory::backend::BackendIdentity::explicit("test-fake", 1, 1, None),
            ),
            rows: Mutex::new(Vec::new()),
        })
    }

    fn upsert(
        &self,
        id: String,
        scope: &str,
        tier: rightkit_memory::backend::MemoryTier,
        content: &str,
        keywords: Vec<String>,
    ) {
        let mut rows = self.rows.lock().unwrap();
        if let Some(row) = rows.iter_mut().find(|row| row.id == id) {
            row.content = content.to_string();
            row.keywords = keywords;
            row.tier = tier;
            row.scope = scope.to_string();
        } else {
            rows.push(FakeRow {
                id,
                scope: scope.to_string(),
                tier,
                content: content.to_string(),
                keywords,
                inject_count: 0,
            });
        }
    }

    fn entry(row: &FakeRow, score: f64) -> FakeEntry {
        FakeEntry {
            id: row.id.clone(),
            tier: row.tier,
            content: row.content.clone(),
            keywords: row.keywords.clone(),
            score,
            created_at: "2026-01-01T00:00:00Z".into(),
            access_count: 0,
            embedding: None,
            scope_id: row.scope.clone(),
        }
    }

    fn matches(row: &FakeRow, query: &str) -> bool {
        let haystack = format!("{} {}", row.id, row.content).to_lowercase();
        let lowered = query.to_lowercase();
        let mut terms = lowered
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| t.len() > 2)
            .map(str::to_string)
            .collect::<Vec<_>>();
        if terms.is_empty() {
            terms.push(lowered.clone());
        }
        terms.iter().any(|term| haystack.contains(term.as_str()))
    }

    fn visible(row: &FakeRow) -> bool {
        !rightkit_memory::memory_provenance::is_provenance_id(&row.id)
    }

    /// Seed a row the way `put` would and return its id (`scope/name`).
    pub(crate) fn seed(&self, name: &str, content: &str, scope: &str) -> String {
        let id = format!("{scope}/{name}");
        self.upsert(
            id.clone(),
            scope,
            rightkit_memory::backend::MemoryTier::Semantic,
            content,
            Vec::new(),
        );
        id
    }

    /// Stored rows, excluding the provenance sidecar scope.
    pub(crate) fn len(&self) -> usize {
        self.rows
            .lock()
            .unwrap()
            .iter()
            .filter(|row| Self::visible(row))
            .count()
    }

    pub(crate) fn contains(&self, id: &str) -> bool {
        self.rows.lock().unwrap().iter().any(|row| row.id == id)
    }

    pub(crate) fn inject_count(&self, id: &str) -> u32 {
        self.rows
            .lock()
            .unwrap()
            .iter()
            .find(|row| row.id == id)
            .map(|row| row.inject_count)
            .unwrap_or(0)
    }
}

#[async_trait::async_trait]
impl rightkit_memory::backend::MemoryBackend for FakeMemoryBackend {
    fn binding(&self) -> &rightkit_memory::backend::BackendBinding {
        &self.binding
    }
    async fn activity_json(&self, _: &FakeCtx, _: usize) -> Result<serde_json::Value, FakeError> {
        Ok(serde_json::json!([]))
    }
    async fn delete(&self, _: &FakeCtx, id: &str) -> Result<bool, FakeError> {
        let mut rows = self.rows.lock().unwrap();
        let before = rows.len();
        rows.retain(|row| row.id != id);
        Ok(rows.len() != before)
    }
    async fn embedder_dim(&self, _: &FakeCtx) -> Result<usize, FakeError> {
        Ok(0)
    }
    async fn entries(&self, _: &FakeCtx, limit: usize) -> Result<Vec<FakeEntry>, FakeError> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|row| Self::visible(row))
            .take(limit)
            .map(|row| Self::entry(row, 1.0))
            .collect())
    }
    async fn get_full(
        &self,
        _: &FakeCtx,
        id: &str,
    ) -> Result<rightkit_memory::backend::FullMemoryRecord, FakeError> {
        match self.rows.lock().unwrap().iter().find(|row| row.id == id) {
            Some(row) => Ok(rightkit_memory::backend::FullMemoryRecord {
                id: row.id.clone(),
                content: row.content.clone(),
                access_count: 0,
                lifecycle: None,
            }),
            None => Err(FakeError::Invalid {
                code: "not_found".into(),
                message: "memory not found".into(),
            }),
        }
    }
    #[allow(clippy::too_many_arguments)]
    async fn log_recall_with_replay(
        &self,
        _: &FakeCtx,
        _: &str,
        _: Option<&str>,
        _: usize,
        _: usize,
        _: usize,
        _: usize,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<(), FakeError> {
        Ok(())
    }
    async fn metrics_json(&self, _: &FakeCtx) -> Result<serde_json::Value, FakeError> {
        Ok(serde_json::json!({}))
    }
    async fn put(
        &self,
        _: &FakeCtx,
        name: &str,
        content: &str,
        scope: &str,
        tier: rightkit_memory::backend::MemoryTier,
    ) -> Result<String, FakeError> {
        let id = format!("{scope}/{name}");
        self.upsert(id.clone(), scope, tier, content, Vec::new());
        Ok(id)
    }
    async fn recall_scored(
        &self,
        _: &FakeCtx,
        query: &str,
        limit: usize,
        scopes: &[String],
    ) -> Result<Vec<(FakeEntry, f64)>, FakeError> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|row| Self::visible(row))
            .filter(|row| scopes.iter().any(|scope| scope == &row.scope))
            .filter(|row| Self::matches(row, query))
            .take(limit)
            .map(|row| (Self::entry(row, 0.5), 0.5))
            .collect())
    }
    async fn record_injections(&self, _: &FakeCtx, ids: &[String]) -> Result<(), FakeError> {
        let mut rows = self.rows.lock().unwrap();
        for row in rows.iter_mut().filter(|row| ids.contains(&row.id)) {
            row.inject_count += 1;
        }
        Ok(())
    }
    async fn remember(
        &self,
        _: &FakeCtx,
        content: &str,
        keywords: Vec<String>,
    ) -> Result<FakeEntry, FakeError> {
        let id = format!("global/mem-{}", self.rows.lock().unwrap().len());
        self.upsert(
            id.clone(),
            "global",
            rightkit_memory::backend::MemoryTier::Semantic,
            content,
            keywords,
        );
        let rows = self.rows.lock().unwrap();
        Ok(Self::entry(
            rows.iter().find(|row| row.id == id).unwrap(),
            1.0,
        ))
    }
    async fn scopes(&self, _: &FakeCtx) -> Result<Vec<String>, FakeError> {
        let mut scopes: Vec<String> = Vec::new();
        for row in self.rows.lock().unwrap().iter() {
            if Self::visible(row) && !scopes.contains(&row.scope) {
                scopes.push(row.scope.clone());
            }
        }
        Ok(scopes)
    }
    async fn search(
        &self,
        _: &FakeCtx,
        query: &str,
        limit: usize,
    ) -> Result<Vec<FakeEntry>, FakeError> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|row| Self::visible(row))
            .filter(|row| Self::matches(row, query))
            .take(limit)
            .map(|row| Self::entry(row, 1.0))
            .collect())
    }
    async fn try_put(
        &self,
        context: &FakeCtx,
        name: &str,
        content: &str,
        scope: &str,
        tier: rightkit_memory::backend::MemoryTier,
    ) -> Result<String, FakeError> {
        self.put(context, name, content, scope, tier).await
    }
    async fn list(
        &self,
        _: &FakeCtx,
        scope: Option<&str>,
    ) -> Result<Vec<rightkit_memory::backend::MemoryListRow>, FakeError> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|row| Self::visible(row))
            .filter(|row| scope.map_or(true, |scope| scope == row.scope))
            .map(|row| rightkit_memory::backend::MemoryListRow {
                id: row.id.clone(),
                tier: row.tier,
                chars: row.content.chars().count(),
                access_count: 0,
                inject_count: row.inject_count,
            })
            .collect())
    }
    async fn remember_consolidated(
        &self,
        _: &FakeCtx,
        name: &str,
        content: &str,
        keywords: Vec<String>,
        _: f64,
    ) -> Result<Option<FakeEntry>, FakeError> {
        self.upsert(
            name.to_string(),
            "global",
            rightkit_memory::backend::MemoryTier::Semantic,
            content,
            keywords,
        );
        let rows = self.rows.lock().unwrap();
        Ok(rows
            .iter()
            .find(|row| row.id == name)
            .map(|row| Self::entry(row, 1.0)))
    }
}

/// Serialize every daemon test that mutates process environment variables.
/// Individual module-local locks do not protect concurrently running sibling
/// test modules, which made credential and sandbox tests race each other.
pub(crate) fn process_env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Explain an `EPERM` that came from the environment, not the code.
///
/// Running `cargo test` inside a restrictive sandbox (an agent harness, a CI
/// container with a seccomp/seatbelt profile) denies the filesystem, `git
/// init` template copy, and socket-bind operations these tests need. The bare
/// failure is a `PermissionDenied` panic that reads exactly like a product bug:
/// a reviewer running the suite that way saw ~30 of them and concluded a
/// recorded "0 failed" result had been fabricated. It had not — the same commit
/// is green unsandboxed.
///
/// A test that dies on `Operation not permitted` must say so, so the next
/// person debugs their sandbox instead of the code.
pub(crate) fn env_denied_message(
    what: &str,
    path: &std::path::Path,
    error: &std::io::Error,
) -> String {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        format!(
            "cannot {what} at {} — the OS denied it (Operation not permitted).\n\
             This is almost always the ENVIRONMENT, not the code: `cargo test` is running \
             inside a sandbox that blocks writes here, `git init` hook-template copies, or \
             socket binds. Re-run the suite outside the sandbox before treating this as a \
             product defect.\n\
             Underlying error: {error}",
            path.display()
        )
    } else {
        format!("cannot {what} at {}: {error}", path.display())
    }
}

#[cfg(test)]
mod env_denied_tests {
    use super::env_denied_message;
    use std::io::{Error, ErrorKind};
    use std::path::Path;

    #[test]
    fn permission_denied_names_the_sandbox() {
        let msg = env_denied_message(
            "create the test workspace",
            Path::new("/x/target/test-tmp"),
            &Error::new(ErrorKind::PermissionDenied, "Operation not permitted"),
        );

        assert!(msg.contains("ENVIRONMENT, not the code"), "{msg}");
        assert!(msg.contains("outside the sandbox"), "{msg}");
    }

    #[test]
    fn other_errors_stay_terse() {
        let msg = env_denied_message(
            "create the test workspace",
            Path::new("/x"),
            &Error::new(ErrorKind::NotFound, "nope"),
        );

        assert!(!msg.contains("sandbox"), "{msg}");
        assert!(msg.contains("nope"), "{msg}");
    }
}
