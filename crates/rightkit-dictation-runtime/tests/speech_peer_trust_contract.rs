//! Source contract: the shared speech service admits only Orthic-signed peers, and the check runs before
//! any request is dispatched. The live signature queries need a signed build; this proves wiring and
//! fail-closed ordering (see `speech_service/peer_trust.rs` for the identities and their sources).

const TRANSPORT: &str = include_str!("../src/speech_service/transport.rs");
const PEER_TRUST: &str = include_str!("../src/speech_service/peer_trust.rs");
const MOD_RS: &str = include_str!("../src/speech_service/mod.rs");
const CONN: &str = include_str!("../src/speech_service/conn.rs");
const FILES: &str = include_str!("../src/speech_service/files.rs");
const LEASE: &str = include_str!("../src/speech_service/lease.rs");
const MANIFEST: &str = include_str!("../Cargo.toml");

/// Production code of a source file: everything before its `#[cfg(test)]` module.
fn production(source: &str) -> &str {
    source.split("#[cfg(test)]").next().unwrap()
}

#[test]
fn the_check_runs_before_the_first_request_is_dispatched() {
    let transport = production(TRANSPORT);
    let admit = transport
        .find("peer_trust::admit(&conn.peer)")
        .expect("transport must call peer_trust::admit on the accepted connection");
    let serve = transport
        .find("super::conn::serve(")
        .expect("transport must hand the connection to conn::serve");
    assert!(admit < serve, "admit must run before serve reads a byte");
    assert_eq!(
        transport.matches("super::conn::serve(").count(),
        1,
        "conn::serve must have exactly one call site, after the admit gate"
    );
    // The refused path closes the connection and returns before serve.
    let gate = &transport[admit..serve];
    assert!(
        gate.contains("(conn.shutdown)();"),
        "a refused peer must be shut down"
    );
    assert!(
        gate.contains("return;"),
        "a refused peer must not reach serve"
    );
    assert!(
        !gate.contains("conn.reader"),
        "nothing may read the connection before admit"
    );
}

#[test]
fn nothing_else_in_the_service_can_serve_a_connection() {
    for (name, source) in [("mod.rs", MOD_RS), ("files.rs", FILES), ("lease.rs", LEASE)] {
        let source = production(source);
        assert!(
            !source.contains("conn::serve(") && !source.contains("serve(ctx"),
            "{name} must not serve connections around the peer_trust gate"
        );
    }
    assert!(
        CONN.contains("pub fn serve("),
        "conn::serve moved; update this contract and re-prove the gate"
    );
    assert!(MOD_RS.contains("mod peer_trust;"));
}

#[test]
fn macos_verifies_the_audit_token_against_the_orthic_team() {
    let source = PEER_TRUST;
    assert!(
        source.contains("set_audit_token("),
        "macOS must bind the audit token (race-free)"
    );
    assert!(
        !source.contains("set_pid("),
        "macOS must never fall back to a bare pid"
    );
    assert!(source.contains("copy_guest_with_attribues(None"));
    assert!(source.contains(".check_validity(Flags::NONE, &requirement)"));
    assert!(source.contains("anchor apple generic and certificate leaf[subject.OU] = "));
    // docs/RIGHT_RELEASE_BUILD_UPLOAD.md:51, docs/SIGNING.md:19.
    assert!(source.contains("ORTHIC_MAC_TEAM_ID: &str = \"6KLGD3LLKF\""));
    assert!(
        source.contains("the kernel gave no audit token"),
        "no token must be unverifiable, not trusted"
    );
}

#[test]
fn windows_verifies_authenticode_and_the_orthic_signer() {
    let source = PEER_TRUST;
    assert!(source.contains("QueryFullProcessImageNameW("));
    assert!(source.contains("WinVerifyTrust("));
    assert!(source.contains("WINTRUST_ACTION_GENERIC_VERIFY_V2"));
    assert!(
        source.contains("WTD_STATEACTION_CLOSE"),
        "the trust state must be released"
    );
    assert!(
        source.contains("peer.started != before"),
        "a reused pid must be refused"
    );
    // docs/WINDOWS-SIGNING.md:5.
    assert!(source.contains("ORTHIC_WINDOWS_SIGNER_CN: &str = \"Damned Ventures LLC\""));
    assert!(source.contains("windows_signer_matches(&name, expected_cn)"));
}

#[test]
fn unsupported_platforms_and_release_builds_fail_closed_with_no_environment_override() {
    let source = PEER_TRUST;
    assert!(source.contains("no peer code-signature verification on this platform"));
    assert!(source.contains("(_, BuildMode::Release) => Decision::Refuse"));
    // The only bypass is the compile-time test/test-mode mode: no environment read in this module.
    assert!(source.contains("#[cfg(any(test, feature = \"test-mode\"))]"));
    for forbidden in ["std::env", "dev_var", "env::var", "RIGHTKIT_"] {
        assert!(
            !source.contains(forbidden),
            "peer_trust must not read `{forbidden}`"
        );
    }
    assert!(source.contains("event = \"service_peer_untrusted\""));
}

#[test]
fn the_platform_dependencies_are_declared() {
    assert!(MANIFEST.contains("security-framework = { version = \"3.7\""));
    assert!(MANIFEST.contains("\"Win32_Security_WinTrust\""));
    assert!(MANIFEST.contains("\"Win32_Security_Cryptography\""));
}
