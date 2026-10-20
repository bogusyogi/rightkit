//! Feature-split guard for the loopback host (PK06-5, ruling D5): `remote-loopback` is a separate
//! feature from `remote`. It must not enable `remote`, `link` or any server crate, and `remote`
//! must not enable it, so a desktop build that enables `remote` is still exactly what the release
//! scanner (outside this crate) refuses, and one that enables only `remote-loopback` is a different
//! scanner row. Runs in every build: it reads the manifest and the sources.

const MANIFEST: &str = include_str!("../Cargo.toml");
const LIB: &str = include_str!("../src/lib.rs");

/// Text of a feature's `[...]` list (multi-line lists included).
fn feature_body(name: &str) -> String {
    let start = MANIFEST
        .lines()
        .position(|l| l.starts_with(&format!("{name} = [")))
        .unwrap_or_else(|| panic!("feature {name} not found"));
    let mut body = String::new();
    for l in MANIFEST.lines().skip(start) {
        body.push_str(l);
        body.push('\n');
        if l.contains(']') {
            break;
        }
    }
    body
}

#[test]
fn the_loopback_feature_enables_nothing() {
    let body = feature_body("remote-loopback");
    let list = body
        .split_once('=')
        .map(|(_, rest)| rest.trim())
        .unwrap_or_default();
    assert_eq!(
        list, "[]",
        "remote-loopback must stay an empty feature (std only, no dependency, no other feature): {body}"
    );
}

#[test]
fn no_feature_enables_the_loopback_feature_and_remote_stays_its_own_feature() {
    // The scanner keys on the `remote` feature name: it must still exist and still be the one
    // that carries the server stack.
    let remote = feature_body("remote");
    assert!(
        remote.contains("dep:axum") && remote.contains("dep:tokio"),
        "{remote}"
    );
    for name in ["remote", "link", "relay", "testing", "test-mode", "local"] {
        let body = feature_body(name);
        assert!(
            !body.contains("remote-loopback"),
            "{name} must not enable remote-loopback: {body}"
        );
    }
    let default = MANIFEST
        .lines()
        .find(|l| l.starts_with("default = "))
        .expect("default features");
    assert_eq!(default, "default = [\"local\"]");
}

#[test]
fn only_the_loopback_feature_gates_the_loopback_module() {
    let lines: Vec<&str> = LIB.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.trim() == "pub mod loopback;")
        .expect("lib.rs declares the loopback module");
    assert_eq!(
        lines[at - 1].trim(),
        "#[cfg(feature = \"remote-loopback\")]",
        "the module is gated by remote-loopback and nothing else"
    );
    // And the remote module is not opened by the loopback feature.
    let remote_at = lines
        .iter()
        .position(|l| l.trim() == "pub mod remote;")
        .expect("lib.rs declares the remote module");
    assert_eq!(
        lines[remote_at - 1].trim(),
        "#[cfg(any(feature = \"remote\", feature = \"link\"))]"
    );
}

#[test]
fn loopback_sources_name_no_server_crate_and_not_the_remote_module() {
    let sources = [
        ("loopback/mod.rs", include_str!("../src/loopback/mod.rs")),
        ("loopback/http.rs", include_str!("../src/loopback/http.rs")),
        ("loopback/auth.rs", include_str!("../src/loopback/auth.rs")),
        (
            "loopback/presence.rs",
            include_str!("../src/loopback/presence.rs"),
        ),
        (
            "loopback/record.rs",
            include_str!("../src/loopback/record.rs"),
        ),
    ];
    for (name, src) in sources {
        for (i, line) in src.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for banned in [
                "tokio",
                "axum",
                "hyper",
                "rustls",
                "rightkit_contracts",
                "rightkit_oauth",
                "crate::remote",
                "crate::link",
            ] {
                assert!(
                    !line.contains(banned),
                    "{name}:{} names {banned}: {line}",
                    i + 1
                );
            }
        }
    }
}
