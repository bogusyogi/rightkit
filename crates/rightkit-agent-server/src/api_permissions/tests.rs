#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_openapi_components_cover_permission_routes() {
        let components = permission_openapi_components();
        for name in [
            "SetPermissionMode",
            "SetPermissionModeResponse",
            "PermissionRulesResponse",
            "PermissionRuleMutationResponse",
            "PersistentPermissionRulesResponse",
            "PersistentPermissionRuleStore",
            "WorkspaceTrustResponse",
            "PermissionDenialsResponse",
        ] {
            assert!(components.contains_key(name), "{name} schema missing");
        }

        assert_eq!(
            components["PermissionRuleRow"]["properties"]["effect"]["$ref"],
            "#/components/schemas/PermissionRuleEffect"
        );
        assert_eq!(
            components["PersistentPermissionRulesResponse"]["properties"]["projects"]
                ["additionalProperties"]["items"]["$ref"],
            "#/components/schemas/PersistentPermissionRule"
        );
        assert_eq!(
            components["PermissionDenialsResponse"]["properties"]["denials"]["items"]["$ref"],
            "#/components/schemas/PermissionDenialRow"
        );
    }

    #[test]
    fn permission_openapi_schema_refs_point_to_components() {
        assert_eq!(
            set_permission_mode_request_schema()["$ref"],
            "#/components/schemas/SetPermissionMode"
        );
        assert_eq!(
            add_permission_rule_request_schema()["$ref"],
            "#/components/schemas/AddPermissionRule"
        );
        assert_eq!(
            persistent_permission_rules_response_schema()["$ref"],
            "#/components/schemas/PersistentPermissionRulesResponse"
        );
        assert_eq!(
            workspace_trust_response_schema()["$ref"],
            "#/components/schemas/WorkspaceTrustResponse"
        );
        assert_eq!(
            permission_denials_response_schema()["$ref"],
            "#/components/schemas/PermissionDenialsResponse"
        );
    }

    #[test]
    fn permission_tightening_cascades_without_later_loosening_children() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _runtime_guard = runtime.enter();
        let state = DaemonState::new("");
        let parent = state.create_session_with_options(
            "permission-parent",
            None,
            "/tmp",
            "parent",
            None,
            crate::api::SessionRuntimeOptions {
                permission_mode: Some(rightkit_agent_policy::PermissionMode::Bypass),
                ..crate::api::SessionRuntimeOptions::default()
            },
        );
        let child = state.create_session_with_options(
            "permission-child",
            Some("worker"),
            "/tmp",
            "child",
            Some("permission-parent"),
            crate::api::SessionRuntimeOptions {
                permission_mode: Some(rightkit_agent_policy::PermissionMode::Bypass),
                ..crate::api::SessionRuntimeOptions::default()
            },
        );
        let grandchild = state.create_session_with_options(
            "permission-grandchild",
            Some("worker"),
            "/tmp",
            "grandchild",
            Some("permission-child"),
            crate::api::SessionRuntimeOptions {
                permission_mode: Some(rightkit_agent_policy::PermissionMode::Bypass),
                ..crate::api::SessionRuntimeOptions::default()
            },
        );

        set_permission_mode_and_tighten_descendants(
            &state,
            &parent,
            rightkit_agent_policy::PermissionMode::Plan,
        );
        for session in [&parent, &child, &grandchild] {
            assert_eq!(
                session.permission_mode_tx.as_ref().map(|tx| *tx.borrow()),
                Some(rightkit_agent_policy::PermissionMode::Plan)
            );
        }

        set_permission_mode_and_tighten_descendants(
            &state,
            &parent,
            rightkit_agent_policy::PermissionMode::Bypass,
        );
        assert_eq!(
            parent.permission_mode_tx.as_ref().map(|tx| *tx.borrow()),
            Some(rightkit_agent_policy::PermissionMode::Bypass)
        );
        for session in [&child, &grandchild] {
            assert_eq!(
                session.permission_mode_tx.as_ref().map(|tx| *tx.borrow()),
                Some(rightkit_agent_policy::PermissionMode::Plan),
                "loosening a parent must not silently expand a running descendant"
            );
        }
    }

    #[test]
    fn one_malformed_rule_rejects_the_whole_imported_store() {
        let mut store = PersistentPermissionRuleStore::default();
        store.user.push(PersistentPermissionRule {
            pattern: "shell(ls)".into(),
            effect: "allow".into(),
        });
        assert!(validate_permission_rule_store(&store).is_ok());
        store.user.push(PersistentPermissionRule {
            pattern: "shell(rm".into(),
            effect: "deny".into(),
        });
        assert!(validate_permission_rule_store(&store).is_err());
        let single = PersistentPermissionRule {
            pattern: "shell(rm".into(),
            effect: "deny".into(),
        };
        assert!(validate_persistent_permission_rule(&single).is_err());
    }

    #[tokio::test]
    async fn persistent_rule_list_carries_load_error_until_repaired() {
        use axum::{body::Body, http::Request};
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let dir = std::env::temp_dir().join(format!("cr-perm-list-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("permission-rules.json");
        std::fs::write(&path, "{ not json").unwrap();
        let state = DaemonState::new("").with_permission_rule_store_path(path.clone());
        let list = |state: DaemonState| async move {
            let resp = crate::api::build_router(state)
                .oneshot(
                    Request::builder()
                        .uri("/permission-rules")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = resp.into_body().collect().await.unwrap().to_bytes();
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()
        };
        let body = list(state.clone()).await;
        assert_eq!(body["loadError"]["code"], "permission_rules_corrupt");
        assert_eq!(body["loadError"]["path"], path.display().to_string());
        assert!(body["loadError"]["message"]
            .as_str()
            .unwrap()
            .contains("corrupt"));
        assert_eq!(body["user"][0]["pattern"], "*");
        assert_eq!(body["user"][0]["effect"], "deny");

        std::fs::write(&path, r#"{"user":[],"projects":{}}"#).unwrap();
        let body = list(state).await;
        assert!(body.get("loadError").is_none());
        assert_eq!(body["user"].as_array().unwrap().len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
