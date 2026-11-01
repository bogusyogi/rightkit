mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use ed25519_dalek::{Signer, SigningKey};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn ref_target_name(schema: &Value) -> &str {
        schema["$ref"]
            .as_str()
            .and_then(|reference| reference.strip_prefix("#/components/schemas/"))
            .expect("component schema ref")
    }

    fn temp_plugin_dir(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "coderight-plugin-middleware-{label}-{}",
            now_millis()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn plugin_row_matches_fixture_contract() {
        let manifest = coderight_plugins::PluginManifest {
            id: "fixture-plugin".into(),
            name: "Fixture Plugin".into(),
            version: "1.0.0".into(),
            description: "Fixture plugin row".into(),
            entry: "plugin.js".into(),
            provides_tools: vec!["inspect_workspace".into()],
            provides_commands: vec!["inspect".into()],
            tools: std::collections::BTreeMap::new(),
            commands: std::collections::BTreeMap::new(),
            prompts: Vec::new(),
            middleware: Vec::new(),
            ui_slots: Vec::new(),
            sandbox: coderight_plugins::PluginSandboxPolicy::default(),
            enabled: true,
        };
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../data/engine__bins__coderight__tests__fixtures__cli-json__plugin-row.v1.json"
        ))
        .expect("parse plugin row fixture");

        assert_eq!(plugin_manifest_json(&manifest), fixture);
    }

    #[test]
    fn plugin_diagnostics_consumes_complete_extension_trust_contract() {
        let diagnostics = extension_execution_diagnostics_json();
        assert_eq!(
            diagnostics["schema_version"],
            "coderight.plugins.execution-trust.v1"
        );
        let classes = diagnostics["extension_execution"]
            .as_array()
            .expect("extension execution diagnostics array");
        assert_eq!(classes.len(), 4);

        let class = |name: &str| {
            classes
                .iter()
                .find(|entry| entry["class"] == name)
                .unwrap_or_else(|| panic!("missing extension trust class {name}"))
        };
        assert_eq!(class("declarative_built_in")["arbitrary_code"], false);
        assert_eq!(
            class("trusted_native_in_process")["process_authority"],
            true
        );
        assert_eq!(
            class("trusted_native_in_process")["registration_is_sandbox"],
            false
        );
        assert_eq!(class("external_process_mcp")["process_authority"], true);
        assert_eq!(class("future_isolated")["available"], false);
        assert_eq!(class("future_isolated")["registration_is_sandbox"], false);
    }

    #[test]
    fn plugin_diagnostics_does_not_restore_orphan_execution_enum() {
        let source = include_str!("routes_handlers.rs");
        assert!(!source.contains("ExtensionExecutionClass"));
        assert!(source.contains("extension_trust_catalog"));
    }

    #[tokio::test]
    async fn plugin_list_publishes_extension_execution_diagnostics() {
        let plugin_dir = temp_plugin_dir("execution-trust");
        let app = crate::api::build_router(
            DaemonState::new("")
                .with_plugin_dir(plugin_dir.clone())
                .with_plugin_marketplace_path(plugin_dir.join("marketplace.json")),
        );
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/plugins")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            value["extension_execution"].as_array().map(Vec::len),
            Some(4)
        );
        assert_eq!(
            value["extension_execution"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["class"] == "trusted_native_in_process")
                .unwrap()["registration_is_sandbox"],
            false
        );
        let _ = std::fs::remove_dir_all(plugin_dir);
    }

    fn test_marketplace_entry() -> PluginMarketplaceEntry {
        PluginMarketplaceEntry {
            manifest: coderight_plugins::PluginManifest {
                id: "signed-fixture".into(),
                name: "Signed Fixture".into(),
                version: "1.0.0".into(),
                description: "Signed test plugin".into(),
                entry: "plugin.py".into(),
                provides_tools: vec!["signed_tool".into()],
                provides_commands: Vec::new(),
                tools: std::collections::BTreeMap::new(),
                commands: std::collections::BTreeMap::new(),
                prompts: Vec::new(),
                middleware: Vec::new(),
                ui_slots: Vec::new(),
                sandbox: coderight_plugins::PluginSandboxPolicy::default(),
                enabled: true,
            },
            downloads: 0,
            rating: 0.0,
            source_url: "https://plugins.example/signed-fixture.tar.gz".into(),
            source_path: PathBuf::new(),
            checksum_sha256: "a".repeat(64),
            package_sha256: "b".repeat(64),
            signed_by: "coderight-test".into(),
            signing_public_key: String::new(),
            signature: String::new(),
            category: "tools".into(),
            tags: vec!["test".into()],
        }
    }

    fn sign_marketplace_entry(entry: &mut PluginMarketplaceEntry) {
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        entry.signing_public_key =
            rightkit_crypto::base64_encode(signing_key.verifying_key().as_bytes());
        entry.signature = rightkit_crypto::base64_encode(
            &signing_key
                .sign(&marketplace_entry_signature_payload(entry))
                .to_bytes(),
        );
    }

    #[test]
    fn marketplace_entry_ed25519_signature_verifies() {
        let mut entry = test_marketplace_entry();
        sign_marketplace_entry(&mut entry);

        verify_marketplace_entry_signature(&entry).expect("signature verifies");
        validate_synced_marketplace_catalog(
            &PluginMarketplaceCatalog {
                plugins: vec![entry],
                synced_from: String::new(),
                synced_at: String::new(),
                signed_by: String::new(),
                signature_sha256: String::new(),
            },
            true,
        )
        .expect("signed catalog validates");
    }

    #[test]
    fn marketplace_entry_signature_rejects_tampered_checksum() {
        let mut entry = test_marketplace_entry();
        sign_marketplace_entry(&mut entry);
        entry.checksum_sha256 = "c".repeat(64);

        let err = verify_marketplace_entry_signature(&entry).expect_err("tamper rejected");
        assert!(err.contains("signature verification failed"), "{err}");
    }

    #[test]
    fn signed_marketplace_catalog_requires_signature_material() {
        let entry = test_marketplace_entry();

        let err = validate_synced_marketplace_catalog(
            &PluginMarketplaceCatalog {
                plugins: vec![entry],
                synced_from: String::new(),
                synced_at: String::new(),
                signed_by: String::new(),
                signature_sha256: String::new(),
            },
            true,
        )
        .expect_err("missing signature rejected");

        assert!(err.contains("signingPublicKey"), "{err}");
    }

    async fn lower_trust_plugin_request_status(
        app: axum::Router,
        method: Method,
        uri: &str,
        body: serde_json::Value,
    ) -> axum::http::StatusCode {
        app.oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header("x-coderight-source-trust", "remoteAgentUntrusted")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
    }

    #[tokio::test]
    async fn plugin_governance_routes_reject_lower_trust_sources() {
        let plugin_dir = temp_plugin_dir("trust-gate");
        let marketplace_path = plugin_dir.join("marketplace.json");
        let app = crate::api::build_router(
            DaemonState::new("")
                .with_plugin_dir(plugin_dir.clone())
                .with_plugin_marketplace_path(marketplace_path.clone()),
        );

        let cases = [
            (
                Method::POST,
                "/plugins/marketplace/sync",
                serde_json::json!({
                    "catalogUrl": "file:///tmp/catalog.json",
                    "expectedSha256": "abc",
                    "trustAccepted": true
                }),
            ),
            (
                Method::POST,
                "/plugins/marketplace/example/install",
                serde_json::json!({"trustAccepted": true}),
            ),
            (
                Method::POST,
                "/plugins/install",
                serde_json::json!({
                    "sourcePath": plugin_dir.join("source"),
                    "trustAccepted": true
                }),
            ),
            (Method::POST, "/plugins/reload", serde_json::json!({})),
            (
                Method::POST,
                "/plugins/example/enable",
                serde_json::json!({}),
            ),
            (
                Method::POST,
                "/plugins/example/disable",
                serde_json::json!({}),
            ),
        ];

        for (method, uri, body) in cases {
            let status = lower_trust_plugin_request_status(app.clone(), method, uri, body).await;
            assert_eq!(status, axum::http::StatusCode::FORBIDDEN, "{uri}");
        }
        assert!(
            !marketplace_path.exists(),
            "lower-trust marketplace sync must fail before writing"
        );
        assert!(
            !plugin_dir.join("source").exists(),
            "lower-trust plugin install must fail before touching source paths"
        );

        let _ = std::fs::remove_dir_all(plugin_dir);
    }

    #[test]
    fn local_plugin_middleware_definitions_import_enabled_manifest_entries() {
        let plugin_dir = temp_plugin_dir("import");
        let root = plugin_dir.join("audit");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("plugin.json"),
            serde_json::json!({
                "id": "audit",
                "name": "Audit",
                "version": "1.0.0",
                "description": "Audit middleware",
                "entry": "plugin.js",
                "enabled": true,
                "middleware": [{
                    "id": "block-shell",
                    "title": "Block shell",
                    "phase": "preToolCall",
                    "effect": "deny",
                    "tool": "shell",
                    "condition": {
                        "argument": "command",
                        "contains": "| sh"
                    },
                    "reason": "Unsafe shell bootstrap",
                    "order": 120
                }]
            })
            .to_string(),
        )
        .unwrap();

        let definitions = local_plugin_middleware_definitions(&plugin_dir);

        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].id, "plugin.audit.block-shell");
        assert_eq!(
            definitions[0].source,
            rightkit_agent_engine_runtime_middleware::MiddlewareSource::LocalPlugin
        );
        assert_eq!(
            definitions[0].phase,
            rightkit_agent_engine_runtime_middleware::MiddlewarePhase::PreToolCall
        );
        assert_eq!(
            definitions[0].effect,
            rightkit_agent_engine_runtime_middleware::MiddlewareEffect::Deny
        );
        assert_eq!(definitions[0].tool.as_deref(), Some("shell"));
        assert_eq!(
            definitions[0].reason.as_deref(),
            Some("Unsafe shell bootstrap")
        );

        let _ = std::fs::remove_dir_all(plugin_dir);
    }

    #[test]
    fn local_plugin_middleware_definitions_skip_disabled_plugins_and_unknown_shapes() {
        let plugin_dir = temp_plugin_dir("skip");
        for (id, enabled, phase) in [
            ("disabled", false, "preToolCall"),
            ("unknown-phase", true, "notARealPhase"),
        ] {
            let root = plugin_dir.join(id);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join("plugin.json"),
                serde_json::json!({
                    "id": id,
                    "name": id,
                    "version": "1.0.0",
                    "description": "Middleware",
                    "entry": "plugin.js",
                    "enabled": enabled,
                    "middleware": [{
                        "id": "mw",
                        "phase": phase,
                        "effect": "deny"
                    }]
                })
                .to_string(),
            )
            .unwrap();
        }

        assert!(local_plugin_middleware_definitions(&plugin_dir).is_empty());

        let _ = std::fs::remove_dir_all(plugin_dir);
    }

    #[test]
    fn plugin_openapi_components_cover_plugin_list_shape() {
        let components = plugin_openapi_components();
        let expected = [
            "PluginListResponse",
            "PluginManifest",
            "PluginToolContribution",
            "PluginCommandContribution",
            "PluginPromptContribution",
            "PluginMiddlewareContribution",
            "PluginSandbox",
            "ExtensionExecutionTrustCatalog",
        ];

        for name in expected {
            assert!(components.contains_key(name), "missing component {name}");
        }

        assert_eq!(
            components["PluginListResponse"]["properties"]["plugins"]["items"]["$ref"],
            "#/components/schemas/PluginManifest"
        );
        assert_eq!(
            components["PluginListResponse"]["properties"]["extension_execution"]["$ref"],
            "#/components/schemas/ExtensionExecutionTrustCatalog"
        );
        assert_eq!(
            components["PluginManifest"]["properties"]["sandbox"]["$ref"],
            "#/components/schemas/PluginSandbox"
        );
        assert_eq!(
            components["PluginManifest"]["properties"]["middleware"]["items"]["$ref"],
            "#/components/schemas/PluginMiddlewareContribution"
        );
        assert_eq!(
            components["PluginSandbox"]["properties"]["isolation"]["enum"][1],
            "restrictedSubprocess"
        );
        assert!(components.contains_key(ref_target_name(&plugin_list_response_schema())));
    }

    #[test]
    fn plugin_openapi_helpers_cover_daemon_plugin_list_route() {
        let components = plugin_openapi_components();
        let endpoints: Vec<_> = crate::api_contract::DAEMON_API_ENDPOINTS
            .iter()
            .filter(|endpoint| endpoint.path == "/plugins")
            .collect();
        assert_eq!(endpoints.len(), 1);

        let method = endpoints[0].method.to_ascii_lowercase();
        let response = plugins_openapi_response_schema(&method, endpoints[0].path)
            .expect("plugin list response schema");

        assert_eq!(response["$ref"], "#/components/schemas/PluginListResponse");
        assert!(components.contains_key(ref_target_name(&response)));
        assert!(plugins_openapi_response_schema("post", "/plugins").is_none());
    }

    #[test]
    fn marketplace_base64_accepts_padded_and_unpadded_but_never_completes_wrong_padding() {
        // "hi" is aGk= padded, aGk unpadded; "h" is aA== padded, aA unpadded.
        assert_eq!(decode_base64_field("aGk=", "signature").unwrap(), b"hi");
        assert_eq!(
            decode_base64_field("aGk", "signature").unwrap(),
            b"hi",
            "unpadded older entries still decode"
        );
        assert_eq!(decode_base64_field("  aA==\n", "signature").unwrap(), b"h");
        assert_eq!(decode_base64_field("aA", "signature").unwrap(), b"h");
        // One `=` where two are needed (length 3 mod 4) used to be completed into valid input; it is refused now.
        assert_eq!(
            decode_base64_field("aA=", "signature").unwrap_err(),
            "invalid base64 signature"
        );
        // Too much padding and a stray single character are refused as before.
        assert!(decode_base64_field("aGk==", "signature").is_err());
        assert!(decode_base64_field("a", "signature").is_err());
    }
}
