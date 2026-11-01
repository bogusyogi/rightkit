//! End-to-end coverage for Windows vendor CLI discovery (B-6).
//!
//! Acceptance:
//! - Real CodeRight process discovers a vendor CLI through Windows PATH containing
//!   semicolon-separated entries, drive letters, and spaces.
//! - Exercises Windows executable suffix resolution (.exe, .cmd, PATHEXT precedence).
//! - Missing vendor CLI returns existing typed unavailable results.
//! - Uses completely isolated fixtures/home/PATH; never invokes installed vendor CLIs.

use crate::support::e2e_orch;
use e2e_orch::*;

use serde_json::json;

const PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

fn isolated_path(entries: &[&std::path::Path]) -> String {
    let mut parts = Vec::with_capacity(entries.len() + 1);
    for entry in entries {
        parts.push(entry.display().to_string());
    }
    // Preserve an empty PATH segment explicitly; discovery ignores it.
    parts.insert(1, String::new());
    parts.insert(2, String::new());
    parts.join(";")
}

fn sha256_file(path: &std::path::Path) -> String {
    let digest = rightkit_crypto::sha256(AsRef::<[u8]>::as_ref(
        &std::fs::read(path).expect("read discovered executable"),
    ));
    digest
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            use std::fmt::Write;
            write!(output, "{byte:02x}").expect("write digest hex");
            output
        })
}

fn write_vendor_stub(path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent directory for vendor stub");
    }
    std::fs::write(path, b"@echo off\r\necho 9.9.9-windows-discovery-stub\r\n")
        .expect("write vendor stub file");
}

#[test]
fn windows_path_with_drive_letters_spaces_and_semicolons_discovers_vendor_cli() {
    let fixture_root = temp_dir("vendor-discovery-spaces");

    // Semicolon-separated PATH entries with spaces and drive letters:
    // 1. leading empty directory with spaces
    // 2. vendor directory with spaces containing claude.exe
    // 3. trailing empty directory with spaces
    let empty_pre = fixture_root.path().join("first empty dir with spaces");
    let vendor_dir = fixture_root
        .path()
        .join("vendor cli path with spaces and symbols");
    let empty_post = fixture_root.path().join("trailing empty dir with spaces");

    std::fs::create_dir_all(&empty_pre).expect("create empty_pre dir");
    std::fs::create_dir_all(&vendor_dir).expect("create vendor_dir");
    std::fs::create_dir_all(&empty_post).expect("create empty_post dir");

    let stub_file = vendor_dir.join("claude.exe");
    // Discovery checks suffix and hashes bytes; this is not a runnable PE.
    std::fs::write(&stub_file, b"isolated discovery-only fixture")
        .expect("write discovery-only executable fixture");

    let path_var = isolated_path(&[&empty_pre, &vendor_dir, &empty_post]);

    // Validate path format expectations
    assert!(path_var.contains(';'), "PATH must be semicolon-separated");
    assert!(path_var.contains(' '), "PATH must contain spaces");
    #[cfg(windows)]
    assert!(
        path_var.contains(':'),
        "PATH entries must contain drive letters on Windows"
    );

    let env = Env::start(
        Provider::start(|_, _| Reply::text("ok")),
        &[("PATH", &path_var), ("PATHEXT", PATHEXT)],
        |_, _| {},
    );

    // 1. Specific discovery endpoint: /cli-providers/claude/discovery
    let (s, d) = env.get("/cli-providers/claude/discovery");
    assert_eq!(s, 200, "{d}");
    assert_eq!(d["installed"], true, "{d}");

    let discovered_exe = d["executable"]
        .as_str()
        .expect("executable path must be present in discovery");
    assert!(
        discovered_exe
            .replace('\\', "/")
            .contains(&slash_path(&vendor_dir)),
        "discovered executable '{discovered_exe}' must point inside vendor_dir '{}'",
        slash_path(&vendor_dir)
    );
    assert!(
        discovered_exe.to_ascii_lowercase().ends_with("claude.exe"),
        "suffix resolution must resolve bare 'claude' to 'claude.exe', got: '{discovered_exe}'"
    );
    assert_eq!(d["executable_sha256"], sha256_file(&stub_file), "{d}");
    assert_eq!(d["protocol_version"], "stream-json", "{d}");
    assert_eq!(d["transport"], "claude_stream_json", "{d}");

    // 2. Provider listing endpoint: /cli-providers
    let (s, provs) = env.get("/cli-providers");
    assert_eq!(s, 200, "{provs}");
    let claude_row = provs["providers"]
        .as_array()
        .expect("providers list")
        .iter()
        .find(|p| p["providerId"] == "claude")
        .expect("claude provider must be listed");

    assert_eq!(claude_row["installed"], true, "{claude_row}");
    assert_eq!(
        claude_row["executableSha256"],
        sha256_file(&stub_file),
        "{claude_row}"
    );
    assert_eq!(
        claude_row["executable"].as_str(),
        Some(discovered_exe),
        "{claude_row}"
    );
}

#[test]
fn windows_executable_suffix_resolution_and_precedence() {
    let fixture_root = temp_dir("vendor-suffix-resolution");

    // Test A: resolution of .cmd extension when no .exe is present
    let cmd_dir = fixture_root.path().join("cmd vendor dir with spaces");
    std::fs::create_dir_all(&cmd_dir).expect("create cmd_dir");
    let cmd_stub = cmd_dir.join("claude.cmd");
    write_vendor_stub(&cmd_stub);

    let path_var_cmd = isolated_path(&[&cmd_dir]);
    let env_cmd = Env::start(
        Provider::start(|_, _| Reply::text("ok")),
        &[("PATH", &path_var_cmd), ("PATHEXT", PATHEXT)],
        |_, _| {},
    );

    let (s, d) = env_cmd.get("/cli-providers/claude/discovery");
    assert_eq!(s, 200, "{d}");
    assert_eq!(d["installed"], true, "{d}");
    let discovered_cmd = d["executable"]
        .as_str()
        .expect("executable path must be present");
    assert!(
        discovered_cmd.to_ascii_lowercase().ends_with("claude.cmd"),
        "bare 'claude' must resolve to 'claude.cmd' when only .cmd exists, got: '{discovered_cmd}'"
    );

    // Test B: precedence of .exe over .cmd in same directory (PATHEXT order: .COM;.EXE;.BAT;.CMD)
    let precedence_dir = fixture_root
        .path()
        .join("precedence vendor dir with spaces");
    std::fs::create_dir_all(&precedence_dir).expect("create precedence_dir");
    let exe_file = precedence_dir.join("claude.exe");
    let cmd_file = precedence_dir.join("claude.cmd");
    // Suffix precedence only; these bytes are not claimed to be executable.
    std::fs::write(&exe_file, b"isolated discovery-only fixture")
        .expect("write discovery-only executable fixture");
    write_vendor_stub(&cmd_file);

    let path_var_prec = isolated_path(&[&precedence_dir]);
    let env_prec = Env::start(
        Provider::start(|_, _| Reply::text("ok")),
        &[("PATH", &path_var_prec), ("PATHEXT", PATHEXT)],
        |_, _| {},
    );

    let (s, d) = env_prec.get("/cli-providers/claude/discovery");
    assert_eq!(s, 200, "{d}");
    assert_eq!(d["installed"], true, "{d}");
    let discovered_prec = d["executable"]
        .as_str()
        .expect("executable path must be present");
    assert!(
        discovered_prec
            .to_ascii_lowercase()
            .ends_with("claude.exe"),
        "precedence must favor 'claude.exe' over 'claude.cmd' per PATHEXT order, got: '{discovered_prec}'"
    );
}

#[test]
fn missing_vendor_cli_returns_typed_unavailable_result() {
    let empty_root = temp_dir("no-vendor-cli");
    let non_matching_dir = empty_root
        .path()
        .join("isolated dir with unrelated binaries");
    std::fs::create_dir_all(&non_matching_dir).expect("create non_matching_dir");

    // Write unrelated files to prove no false-positive match occurs
    std::fs::write(
        non_matching_dir.join("other_tool.exe"),
        b"@echo off\r\necho other\r\n",
    )
    .expect("write other_tool.exe");
    std::fs::write(
        non_matching_dir.join("not_claude.cmd"),
        b"@echo off\r\necho not_claude\r\n",
    )
    .expect("write not_claude.cmd");

    let path_var_missing = isolated_path(&[&non_matching_dir]);
    let env = Env::start(
        Provider::start(|_, _| Reply::text("ok")),
        &[("PATH", &path_var_missing), ("PATHEXT", PATHEXT)],
        |_, _| {},
    );

    // 1. /cli-providers/claude/discovery must return typed unavailable result
    let (s, d) = env.get("/cli-providers/claude/discovery");
    assert_eq!(s, 200, "{d}");
    assert_eq!(
        d["installed"], false,
        "missing vendor CLI must report installed=false"
    );
    assert!(
        d["executable"].is_null(),
        "executable must be null when vendor CLI is missing"
    );
    assert!(
        d["binary_version"].is_null(),
        "binaryVersion must be null when vendor CLI is missing"
    );
    assert!(
        d["executable_sha256"].is_null(),
        "executableSha256 must be null when vendor CLI is missing"
    );
    assert_eq!(d["protocol_version"], "stream-json", "{d}");
    assert_eq!(d["transport"], "claude_stream_json", "{d}");

    // 2. /cli-providers summary listing must also reflect installed=false
    let (s, provs) = env.get("/cli-providers");
    assert_eq!(s, 200, "{provs}");
    let claude_row = provs["providers"]
        .as_array()
        .expect("providers list")
        .iter()
        .find(|p| p["providerId"] == "claude")
        .expect("claude provider must be listed");
    assert_eq!(claude_row["installed"], false, "{claude_row}");
    assert!(
        claude_row["executable"].is_null(),
        "summary executable must be null: {claude_row}"
    );

    // 3. /drivers/claude/discovery must return lifecycle=unavailable
    let (s, dd) = env.get("/drivers/claude/discovery");
    assert_eq!(s, 200, "{dd}");
    assert_eq!(dd["driverKind"], "claude", "{dd}");
    assert_eq!(dd["installed"], false, "{dd}");
    assert_eq!(
        dd["lifecycle"], "unavailable",
        "missing vendor CLI must yield typed lifecycle 'unavailable'"
    );
    assert_eq!(dd["version"]["installed"], false, "{dd}");
    assert!(
        dd["version"]["binaryVersion"].is_null(),
        "driver binaryVersion must be null"
    );

    // 4. Starting a session driver without capability lease / missing vendor must fail closed
    env.create_session_with("session-missing-vendor", json!({"driverKind": "claude"}));
    let (s, st) = env.get("/sessions/session-missing-vendor/driver/status");
    assert_eq!(s, 200, "{st}");
    assert_eq!(st["driverKind"], "claude", "{st}");
    assert_eq!(st["drivers"], json!([]), "{st}");

    let (s, b) = env.post(
        "/sessions/session-missing-vendor/driver/start",
        json!({"bindingId": "unauthorized-binding"}),
    );
    // This proves the auth gate only; it does not prove provider launch behavior.
    assert_eq!(
        s, 401,
        "driver start without valid lease must fail with 401: {b}"
    );
    assert_eq!(
        env.provider.count(),
        0,
        "missing vendor CLI must never fall back to native model or invoke external CLI"
    );
}
