//! Forbidden-boundary tests for the CLI runtime feature.
//!
//! PR 0 of the architecture plan: these tests must fail if the feature
//! ever silently violates the scope lock in
//! `docs/architecture/CLI-RUNTIME-SCOPE.md`. They are deliberately
//! conservative: every check is a guard against a known footgun, not a
//! generic linter pass.
//!
//! The tests scan the source tree at compile/test time and assert that
//! the forbidden patterns are absent. They are not exhaustive; they are
//! tripwires. Anything that changes a test below must be matched with a
//! corresponding change to the scope lock.

use std::fs;
use std::path::{Path, PathBuf};

/// The scope-lock document is normative. It must exist and name the six
/// allowed CLIs explicitly. If the list shrinks, that is a feature
/// removal and must go through the planning flow, not a silent edit.
#[test]
fn scope_lock_document_exists_and_lists_six_providers() {
    let root = repo_root();
    let doc = root.join("docs/architecture/CLI-RUNTIME-SCOPE.md");
    assert!(
        doc.exists(),
        "CLI-RUNTIME-SCOPE.md is missing; the feature has no normative scope"
    );

    let text = fs::read_to_string(&doc).expect("scope lock must be readable");

    for required in [
        "Claude Code",
        "Codex CLI",
        "Grok Build CLI",
        "Cursor Agent CLI",
        "Cline CLI",
        "Command Code CLI",
    ] {
        assert!(
            text.contains(required),
            "scope lock must name {required:?} as an allowed provider"
        );
    }

    assert!(
        text.contains("Kiro"),
        "scope lock must explicitly name Kiro as out-of-scope"
    );
    let normalized = text.to_lowercase().replace("**", "");
    assert!(
        normalized.contains("not in this plan") || normalized.contains("not part of this plan"),
        "scope lock must state Kiro is excluded from this plan"
    );
}

/// The scope lock must forbid the documented API-key environment
/// variables. If a contributor silently adds a new BYOK route, this
/// test should fail unless the scope lock is updated first.
#[test]
fn scope_lock_document_forbids_documented_api_key_env_vars() {
    let root = repo_root();
    let doc = root.join("docs/architecture/CLI-RUNTIME-SCOPE.md");
    let text = fs::read_to_string(&doc).expect("scope lock must be readable");

    for forbidden in [
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "XAI_API_KEY",
        "CURSOR_API_KEY",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "OPENAI_BASE_URL",
        "OPENAI_ORG_ID",
    ] {
        assert!(
            text.contains(forbidden),
            "scope lock must list {forbidden} as a forbidden environment variable"
        );
    }
}

/// Built-in registry ceiling. If a contributor adds Kiro to a default
/// list, default fixture, or built-in lookup, this test fails. The
/// scope lock is the source of truth; this is just the enforcement.
#[test]
fn cli_runtime_source_does_not_name_kiro() {
    let root = repo_root();
    let src_dir = root.join("engine/crates/driver_host/src");
    assert!(
        src_dir.exists(),
        "driver_host crate source directory must exist"
    );

    let mut hits = Vec::new();
    walk_rs_files(&src_dir, &mut |path| {
        let text = fs::read_to_string(path).unwrap_or_default();
        for (lineno, line) in text.lines().enumerate() {
            // Match the standalone identifier "kiro" in identifier
            // position. Avoid false positives on words like "kiro-like"
            // by requiring word boundaries or quoted string fragments.
            let lowered = line.to_lowercase();
            if lowered
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
                .any(|tok| tok == "kiro")
            {
                hits.push(format!("{}:{}", path.display(), lineno + 1));
            }
        }
    });

    assert!(
        hits.is_empty(),
        "Kiro must not appear in cli_runtime source; hits: {hits:#?}"
    );
}

/// No external provider SDK or inference client may be added to the
/// driver_host crate. Each pattern below is a known vendor SDK we
/// forbid in this feature.
#[test]
fn cli_runtime_does_not_import_external_provider_sdks() {
    let root = repo_root();
    let manifest_path = root.join("engine/crates/driver_host/Cargo.toml");
    let manifest = fs::read_to_string(&manifest_path).expect("Cargo.toml must be readable");
    let lowered = manifest.to_lowercase();

    let forbidden_dependencies = [
        "anthropic-sdk",
        "openai-api",
        "openai_sdk",
        "aws-sdk-bedrock",
        "google-cloud-vertex",
        "xai-sdk",
        "cursor-sdk",
        "cline-sdk",
        "commandcode-sdk",
        "command-code-sdk",
        // Generic "use the vendor's API" patterns.
        "reqwest",
        // We deliberately do not add reqwest here because
        // process spawning for CLI subprocesses uses std::process.
        // But we still forbid it because the architecture only adds
        // subprocess supervision, not outbound HTTP.
    ];

    // The reqwest carve-out above is intentional: this test deliberately
    // bans HTTP client crates because the architecture forbids
    // the host app calling provider APIs over the network. Subprocess
    // supervision uses std::process.
    let mut hits = Vec::new();
    for dep in forbidden_dependencies {
        // Allow "anthropic-sdk" only in comments that explicitly forbid
        // it; otherwise any dependency declaration is forbidden.
        if lowered.contains(dep) {
            // Permit only when the dependency appears inside a
            // for-display comment that says it is forbidden.
            let allowed_in_comment = manifest
                .lines()
                .filter(|l| l.contains(dep) && (l.contains('#') || l.trim().is_empty()))
                .any(|l| {
                    l.to_lowercase().contains("forbid") || l.to_lowercase().contains("do not")
                });
            if !allowed_in_comment {
                hits.push(dep.to_string());
            }
        }
    }

    assert!(
        hits.is_empty(),
        "driver_host Cargo.toml must not depend on forbidden external SDKs: {hits:#?}"
    );
}

/// The CLI accounts UI must not expose API-key input fields under this
/// feature. This is enforced by scanning the desktop settings tree for
/// common API-key control names.
#[test]
fn cli_accounts_ui_has_no_api_key_input_field() {
    let root = repo_root();
    let settings_root = root.join("apps/rightkit-agent-tauri/src/components/settings");
    if !settings_root.exists() {
        // No settings tree present yet; PR 0 cannot block missing code.
        return;
    }

    // We look only inside directories that look like CLI account cards.
    // Anything outside this filter is treated as the legacy API-provider
    // configuration screen, which is explicitly out of scope of this
    // feature and allowed to keep its existing API-key fields.
    let mut hits = Vec::new();
    walk_tsx_files(&settings_root, &mut |path| {
        let lowered_path = path
            .strip_prefix(&settings_root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_lowercase();
        if !lowered_path.contains("cli")
            && !lowered_path.contains("account")
            && !lowered_path.contains("conductor")
        {
            return;
        }
        let text = fs::read_to_string(path).unwrap_or_default();
        for (lineno, line) in text.lines().enumerate() {
            let l = line.to_lowercase();
            // Conservative: match identifiers commonly used for API-key
            // input fields. False positives are accepted because the
            // contributing change must rename the field to use a
            // different identifier.
            let bad = l.contains("apikey")
                || l.contains("api_key")
                || l.contains("anthropic_api_key")
                || l.contains("openai_api_key")
                || l.contains("xai_api_key")
                || l.contains("cursor_api_key");
            if bad {
                hits.push(format!("{}:{}", path.display(), lineno + 1));
            }
        }
    });

    assert!(
        hits.is_empty(),
        "CLI accounts settings tree must not declare API-key fields; hits: {hits:#?}"
    );
}

/// Helper: walk all `.rs` files under a directory and invoke a closure
/// for each. Skips target/ and hidden directories.
fn walk_rs_files(dir: &Path, visit: &mut dyn FnMut(&Path)) {
    if !dir.is_dir() {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if name.starts_with('.') || name == "target" {
            continue;
        }
        if path.is_dir() {
            walk_rs_files(&path, visit);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            visit(&path);
        }
    }
}

/// Helper: walk all `.tsx` files under a directory and invoke a
/// closure for each. Skips node_modules/, target/, and hidden dirs.
fn walk_tsx_files(dir: &Path, visit: &mut dyn FnMut(&Path)) {
    if !dir.is_dir() {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if name.starts_with('.')
            || name == "target"
            || name == "node_modules"
            || name == "dist"
            || name == "build"
        {
            continue;
        }
        if path.is_dir() {
            walk_tsx_files(&path, visit);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) == Some("tsx") {
            visit(&path);
        }
    }
}

/// Resolve the repository root by walking up from `CARGO_MANIFEST_DIR`
/// until we find `docs/architecture`. This works whether the test is
/// run from the worktree or the primary checkout.
fn repo_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut cur: Option<&Path> = Some(manifest_dir.as_path());
    while let Some(p) = cur {
        if p.join("docs/architecture").is_dir() {
            return p.to_path_buf();
        }
        cur = p.parent();
    }
    panic!(
        "could not locate repository root from manifest dir {}",
        manifest_dir.display()
    );
}
