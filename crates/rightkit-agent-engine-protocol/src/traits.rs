use crate::artifact::{ArtifactApproval, ArtifactRow, ArtifactVerdict};

/// The persistence contract the gated `artifact_trust` tools (P2 A3) write through — the **only**
/// path that stamps an artifact's trust-bearing fields (verdict, approvals, shippable).
///
/// Ported from `IArtifactTrustStore`. The C# interface uses nullable return types for
/// "not found"; we use `Option<ArtifactRow>` here.
pub trait ArtifactTrustStore {
    /// Resolve an artifact by its id (across any session).
    fn get_artifact(&self, id: &str) -> Option<ArtifactRow>;

    /// Resolve an artifact by (session, path) — the convenience lookup for an agent's own output.
    fn get_artifact_by_path(&self, session_id: &str, path: &str) -> Option<ArtifactRow>;

    /// Stamp `verdict` onto the artifact (A3). The caller has already enforced juror≠producer;
    /// the verdict's `bound_hash` is the artifact's current `content_hash` at call time.
    fn attach_verdict(
        &mut self,
        artifact_id: &str,
        verdict: ArtifactVerdict,
    ) -> Option<ArtifactRow>;

    /// Append a human-gate `approval` to the artifact. Runtime-only — never reached through
    /// agent tool dispatch; this is the runtime's direct path.
    fn record_approval(
        &mut self,
        artifact_id: &str,
        approval: ArtifactApproval,
    ) -> Option<ArtifactRow>;

    /// Mark the artifact `shippable` (A3) — the finalize / shippable-intent signal later gates hang on.
    fn finalize_artifact(&mut self, artifact_id: &str) -> Option<ArtifactRow>;
}

/// The single seam between thin drivers (CLI, UI, future remote client) and the runtime.
///
/// Ported from `ISessionConduit`. The C# interface uses `ValueTask` and `IAsyncEnumerable`
/// which are .NET async primitives; in Rust these are replaced with trait method signatures
/// using a generic event type. Concrete async implementations will use `tokio::sync` channels
/// or similar — this trait defines the logical contract only (no async/futures here to keep
/// this crate I/O-free).
pub trait SessionConduit {
    type Error: std::fmt::Debug;

    /// Submit a command into the runtime. Thread-safe.
    fn submit(&self, op: crate::op::Op) -> Result<(), Self::Error>;
}
