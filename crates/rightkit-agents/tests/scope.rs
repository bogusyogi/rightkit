//! No-SDK, no-HTTP, no-MCP scope invariant. Lifted from CodeRight's
//! `driver_host/tests/cli_runtime_scope.rs` and adapted: this crate speaks vendor CLI
//! protocols over stdio only. Changing a list below requires changing the design first.

use std::fs;
use std::path::{Path, PathBuf};

const FORBIDDEN_DEPENDENCIES: &[&str] = &[
    // HTTP clients and async runtimes
    "reqwest",
    "hyper",
    "axum",
    "tokio",
    "ureq",
    "isahc",
    "surf",
    "attohttpc",
    "curl",
    "rightkit-http",
    // vendor SDKs and MCP
    "anthropic",
    "anthropic-sdk",
    "openai",
    "async-openai",
    "openai-api",
    "aws-sdk-bedrock",
    "rmcp",
    "mcp",
    "rust-mcp-sdk",
    // Node bridges
    "napi",
    "neon",
];

const FORBIDDEN_SOURCE_TOKENS: &[&str] = &[
    "api.anthropic.com",
    "api.openai.com",
    "chatgpt.com/backend-api",
    "generativelanguage.googleapis.com",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "--mcp-config",
    "--strict-mcp-config",
    "reqwest::",
    "tokio::",
    "hyper::",
    "axum::",
];

fn manifest() -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap()
}

fn forbidden_dependency_hits(manifest: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for line in manifest.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let name = if line.starts_with('[') {
            // `[dependencies.reqwest]` style tables name the crate in the header.
            line.trim_matches(|c| c == '[' || c == ']')
                .rsplit('.')
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        } else {
            line.split('=').next().unwrap_or("").trim().to_string()
        };
        if FORBIDDEN_DEPENDENCIES.contains(&name.as_str()) {
            hits.push(name);
        }
    }
    hits
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn forbidden_source_hits(text: &str) -> Vec<&'static str> {
    FORBIDDEN_SOURCE_TOKENS
        .iter()
        .copied()
        .filter(|token| text.contains(token))
        .collect()
}

#[test]
fn manifest_has_no_http_sdk_mcp_or_node_dependency() {
    let hits = forbidden_dependency_hits(&manifest());
    assert!(hits.is_empty(), "forbidden dependencies: {hits:?}");
}

#[test]
fn source_names_no_vendor_api_host_mcp_flag_or_http_runtime() {
    let mut files = Vec::new();
    rs_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    assert!(!files.is_empty());
    let mut hits = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file).unwrap();
        for token in forbidden_source_hits(&text) {
            hits.push(format!("{}: {token}", file.display()));
        }
    }
    assert!(hits.is_empty(), "forbidden source tokens: {hits:#?}");
}

#[test]
fn the_scan_itself_detects_violations() {
    assert_eq!(
        forbidden_dependency_hits("[dependencies]\nreqwest = \"0.12\"\n# tokio = 1\n"),
        vec!["reqwest".to_string()]
    );
    assert_eq!(
        forbidden_dependency_hits("[target.'cfg(unix)'.dependencies.hyper]\nversion = \"1\"\n"),
        vec!["hyper".to_string()]
    );
    assert_eq!(
        forbidden_source_hits("let u = \"https://api.anthropic.com/v1\";"),
        vec!["api.anthropic.com"]
    );
}

#[test]
fn default_features_enable_nothing() {
    let text = manifest();
    assert!(text.contains("default = []"));
}

#[test]
fn no_fake_cli_ships_in_the_default_build() {
    let text = manifest();
    assert!(!text.contains("[[bin]]") && !text.contains("[[example]]"));
    assert!(!text.contains("fake"), "no fake-cli feature or target");
    let mut files = Vec::new();
    rs_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    for file in files {
        let source = fs::read_to_string(&file).unwrap();
        assert!(
            !source.contains("RK_FAKE_CLI") && !source.contains("fake_cli"),
            "{} names the test-only fake CLI",
            file.display()
        );
    }
}

/// `--permission-prompt-tool` normally names an MCP tool. The one use here passes the
/// literal `stdio` (the control channel on the child's own stdin/stdout), never an MCP tool,
/// and only `claude.rs` may spell it.
#[test]
fn permission_prompt_flag_is_only_the_stdio_control_channel() {
    let mut files = Vec::new();
    rs_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    for file in files {
        let source = fs::read_to_string(&file).unwrap();
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        if name == "claude.rs" {
            assert_eq!(
                source.matches("--permission-prompt-tool").count(),
                1,
                "claude.rs spells the flag exactly once"
            );
            assert!(source.contains("(\"--permission-prompt-tool\", \"stdio\")"));
        } else {
            assert!(
                !source.contains("--permission-prompt-tool"),
                "{} names the permission prompt flag",
                file.display()
            );
        }
        assert!(
            !source.contains("mcp__"),
            "{} names an MCP tool",
            file.display()
        );
    }
}
