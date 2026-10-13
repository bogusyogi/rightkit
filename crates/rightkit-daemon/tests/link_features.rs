//! Feature-split guard for the outbound link: the device client (`link`) must not pull the server
//! stack. Runs in every build; it reads the manifest and the link-side sources, so it fails if a
//! later change makes tokio, axum or hyper reachable from the `link` feature.

const MANIFEST: &str = include_str!("../Cargo.toml");

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
fn the_link_feature_enables_no_server_crate() {
    let link = feature_body("link");
    for banned in [
        "tokio",
        "axum",
        "hyper",
        "hyper-util",
        "tokio-rustls",
        "rightkit-contracts",
        "rightkit-oauth",
        "form_urlencoded",
    ] {
        assert!(
            !link.contains(&format!("dep:{banned}")),
            "link must not enable {banned}: {link}"
        );
    }
    assert!(
        link.contains("dep:rustls") && link.contains("dep:ring"),
        "{link}"
    );
    // `remote` implies `link`, never the other way round.
    assert!(feature_body("remote").contains("\"link\""));
}

#[test]
fn only_remote_enables_the_server_stack() {
    let remote = feature_body("remote");
    for dep in [
        "tokio",
        "axum",
        "hyper",
        "hyper-util",
        "tokio-rustls",
        "rightkit-contracts",
        "rightkit-oauth",
    ] {
        let line = MANIFEST
            .lines()
            .find(|l| l.starts_with(&format!("{dep} = ")))
            .unwrap_or_else(|| panic!("dependency {dep}"));
        assert!(line.contains("optional = true"), "{dep} must stay optional");
        assert!(
            remote.contains(&format!("dep:{dep}")),
            "remote must enable {dep}"
        );
        let enabling: Vec<&str> = MANIFEST
            .lines()
            .filter(|l| l.contains(&format!("\"dep:{dep}\"")))
            .collect();
        assert_eq!(
            enabling.len(),
            1,
            "only one feature list may enable {dep}: {enabling:?}"
        );
    }
}

#[test]
fn default_features_carry_neither_link_nor_remote() {
    assert!(
        MANIFEST.lines().any(|l| l == "default = [\"local\"]"),
        "default must stay [\"local\"]"
    );
}

#[test]
fn link_side_sources_name_no_server_crate() {
    let sources = [
        ("link/mod.rs", include_str!("../src/link/mod.rs")),
        ("link/frame.rs", include_str!("../src/link/frame.rs")),
        ("link/client.rs", include_str!("../src/link/client.rs")),
        (
            "remote/client/mod.rs",
            include_str!("../src/remote/client/mod.rs"),
        ),
        (
            "remote/client/transport.rs",
            include_str!("../src/remote/client/transport.rs"),
        ),
        (
            "remote/client/error.rs",
            include_str!("../src/remote/client/error.rs"),
        ),
        (
            "remote/signing.rs",
            include_str!("../src/remote/signing.rs"),
        ),
        ("remote/util.rs", include_str!("../src/remote/util.rs")),
    ];
    for (name, src) in sources {
        for (i, line) in src.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for banned in ["tokio", "axum", "hyper"] {
                assert!(
                    !line.contains(banned),
                    "{name}:{} names {banned} on the link side: {line}",
                    i + 1
                );
            }
        }
    }
}
