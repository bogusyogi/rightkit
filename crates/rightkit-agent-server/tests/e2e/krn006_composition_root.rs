//! KRN-006 -- composition-root construction count and dependency guard.
//!
//! This fixture is intentionally source-level: it can run while a sibling
//! lane has the main binary in a transiently uncompilable state. Counts are
//! scoped to production source and distinguish the InlineHost/ServiceHost
//! roots from projection and test-only constructors.

const STATE_ROOT: &str = include_str!("../../src/api_state_impl_01.rs");
const STATE_WIRING: &str = include_str!("../../src/api_state_impl_02.rs");
const DAEMON_STATE: &str = include_str!("../../src/daemon_state.rs");
const SERVICE_HOST: &str = include_str!("../../src/main_runtime_server.rs");
const INLINE_HOST: &str = include_str!("../../src/main_runtime_session_host.rs");
const CONTEXT_MODE: &str = include_str!("../../src/cli_definitions_command_run.rs");

fn production_source(source: &str) -> String {
    // Test modules can precede production items & files can use CRLF.
    // Top-level module closing braces are unindented; nested items are not.
    let mut lines = source.split_inclusive('\n').peekable();
    let mut production = String::new();
    while let Some(line) = lines.next() {
        if line.trim() == "#[cfg(test)]"
            && lines.peek().is_some_and(|next| next.starts_with("mod "))
        {
            for test_line in lines.by_ref() {
                if test_line.trim_end() == "}" {
                    break;
                }
            }
        } else {
            production.push_str(line);
        }
    }
    production
}

fn count(source: &str, needle: &str) -> usize {
    source.match_indices(needle).count()
}

fn require_once(source: &str, needle: &str, owner: &str) {
    assert_eq!(
        count(source, needle),
        1,
        "KRN-006 {owner} must expose exactly one {needle:?}"
    );
}

#[test]
fn krn_006_state_root_constructs_each_authority_once() {
    let root = &production_source(STATE_ROOT);
    let wiring = &production_source(STATE_WIRING);

    // DaemonState::new is the application kernel/state composition seam. The
    // authority implementations are injected below it, never rebuilt by
    // projections or trait implementations.
    require_once(
        root,
        "pub fn new(token: impl Into<String>) -> Self",
        "kernel",
    );
    require_once(root, "let driver_host = match", "process/effect host");
    require_once(
        root,
        "process_resource_authority::process_resource_governor()",
        "resource governor",
    );
    require_once(
        root,
        "GovernedProcessLaunchAdapter::new(",
        "resource/process authority",
    );
    require_once(wiring, "pub fn with_storage<S>", "durable store authority");
    require_once(
        wiring,
        "self.storage = Some(storage);",
        "durable store injection",
    );
    require_once(
        wiring,
        "pub(crate) fn with_mutation_diagnostics(",
        "effect/mutation diagnostics authority",
    );
    require_once(
        wiring,
        "pub(crate) fn with_membrane(",
        "Membrane context authority",
    );

    // Production state is Membrane-bound by default. The legacy mode flag is
    // retained only as a deprecated no-op.
    require_once(
        root,
        "context_mode: crate::cli::ContextMode::Membrane",
        "production context mode",
    );
    require_once(
        wiring,
        "pub(crate) fn with_context_mode(mut self, mode: crate::cli::ContextMode)",
        "context-mode compatibility field",
    );
}

#[test]
fn krn_006_each_host_uses_the_single_degraded_binding_path() {
    for (name, source) in [("ServiceHost", SERVICE_HOST), ("InlineHost", INLINE_HOST)] {
        let source = &production_source(source);
        require_once(
            source,
            "rightkit_memory::backend::native::connect_from_environment()",
            name,
        );
        require_once(source, ".with_membrane(", name);
        assert!(
            source.contains("with_context_degraded("),
            "KRN-006 {name} must keep unavailable context visibly degraded"
        );
    }
}

#[test]
fn krn_006_context_mode_flag_is_a_deprecated_noop() {
    let mode = &production_source(CONTEXT_MODE);
    let effective = mode
        .split("pub fn effective() -> Self {")
        .nth(1)
        .and_then(|body| body.trim_start().split('\n').next())
        .unwrap_or_default();
    assert!(
        effective.contains("Self::Membrane"),
        "KRN-006 legacy context-mode flag must resolve to the canonical binding path"
    );
    assert!(
        mode.contains("deprecated and ignored"),
        "KRN-006 legacy context-mode flag must warn that it is ignored"
    );
}

#[test]
fn krn_006_state_and_trait_implementations_cannot_construct_peer_authorities() {
    let forbidden = [
        "Storage::open(",
        "Storage::open_in_memory(",
        "FileService::new(",
        "ResourceGovernor::new(",
        "ResourceGovernor::for_profile(",
        "RuntimeBindingSlots::new(",
        "connect_from_environment_with_identity(",
        "SessionEngine::new(",
        "PolicyDecisionPoint::new(",
        "GovernedProcessLaunchAdapter::new(",
    ];

    for (owner, source) in [
        ("DaemonState trait implementations", DAEMON_STATE),
        ("DaemonState wiring adapters", STATE_WIRING),
    ] {
        let source = &production_source(source);
        for &needle in &forbidden {
            assert_eq!(
                count(source, needle),
                0,
                "KRN-006 peer authority construction in {owner}: {needle:?}"
            );
        }
    }
}

#[test]
fn krn_006_direct_context_requires_explicit_mode_and_normal_mode_requires_binding() {
    let wiring = &production_source(STATE_WIRING);
    // The removed direct mode is a compatibility no-op: only an unavailable binding
    // (recorded through `with_context_degraded`) selects the no-injection path.
    assert!(wiring.contains("pub(crate) fn uses_direct_context(&self) -> bool"));
    assert!(wiring.contains("self.context_degraded.is_some()"));
    assert!(wiring.contains("pub(crate) fn with_context_degraded("));
    assert!(
        !wiring.contains("context_mode = crate::cli::ContextMode::Direct")
            && !wiring.contains("ContextMode::Direct;"),
        "KRN-006 must not silently select direct mode in state composition"
    );
}
