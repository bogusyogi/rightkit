#[cfg(test)]
mod corpus_bakeoff_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use http_body_util::BodyExt;
    use std::sync::Mutex;
    use tower::ServiceExt;

    static CORPUS_TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct CorpusFixture {
        root: PathBuf,
        manifest: PathBuf,
    }

    impl Drop for CorpusFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn init_clean_git_workspace(path: &Path) {
        fs::write(path.join("README.md"), "fixture workspace\n").unwrap();
        let run = |args: &[&str]| {
            let output = rightkit_child_process::command_std("git")
                .args(args)
                .current_dir(path)
                .env("GIT_AUTHOR_NAME", "RightKit Test")
                .env("GIT_AUTHOR_EMAIL", "test@coderight.local")
                .env("GIT_COMMITTER_NAME", "RightKit Test")
                .env("GIT_COMMITTER_EMAIL", "test@coderight.local")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {:?}: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run(&["init", "-q"]);
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "fixture"]);
    }

    fn corpus_fixture(verifier_inside_workspace: bool) -> CorpusFixture {
        let root = std::env::temp_dir().join(format!(
            "coderight-corpus-bakeoff-test-{}-{}",
            std::process::id(),
            CORPUS_TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        for path in [
            "private",
            "harbor/task-a",
            "workspaces/private",
            "workspaces/harbor",
            "verifier",
        ] {
            fs::create_dir_all(root.join(path)).unwrap();
        }
        fs::write(
            root.join("private/instruction.md"),
            "Return fixture-output.",
        )
        .unwrap();
        fs::write(
            root.join("harbor/task-a/task.toml"),
            "schema_version = \"1.3\"\n[task]\nname = \"fixture/task-a\"\n",
        )
        .unwrap();
        fs::write(
            root.join("harbor/task-a/instruction.md"),
            "Return fixture-output.",
        )
        .unwrap();
        fs::write(root.join("verifier/expected.txt"), "fixture-output").unwrap();
        if verifier_inside_workspace {
            fs::write(
                root.join("workspaces/private/expected.txt"),
                "fixture-output",
            )
            .unwrap();
        }
        init_clean_git_workspace(&root.join("workspaces/private"));
        init_clean_git_workspace(&root.join("workspaces/harbor"));
        let private_expected = if verifier_inside_workspace {
            "workspaces/private/expected.txt"
        } else {
            "verifier/expected.txt"
        };
        let manifest = root.join("corpus.json");
        fs::write(
            &manifest,
            serde_json::to_vec_pretty(&json!({
                "schemaVersion": CORPUS_SCHEMA_VERSION,
                "suiteNamespace": "local-fixture",
                "candidateRoles": ["coder", "code-reviewer"],
                "baselineRole": "coder",
                "tasks": [
                    {
                        "caseNonce": "private-a",
                        "source": {
                            "kind": "private",
                            "instructionPath": "private/instruction.md",
                            "workspaceRoot": "workspaces/private"
                        },
                        "verifier": {
                            "kind": "exactResponse",
                            "expectedOutputPath": private_expected
                        },
                        "candidateCostLimitUsd": 0.05
                    },
                    {
                        "caseNonce": "harbor-a",
                        "source": {
                            "kind": "harbor",
                            "taskRoot": "harbor/task-a",
                            "workspaceRoot": "workspaces/harbor"
                        },
                        "verifier": {
                            "kind": "exactResponse",
                            "expectedOutputPath": "verifier/expected.txt"
                        },
                        "candidateCostLimitUsd": 0.05
                    }
                ],
                "gate": {"minimumVerifiedCompletionRate": 1.0},
                "timeoutSeconds": 5
            }))
            .unwrap(),
        )
        .unwrap();
        CorpusFixture { root, manifest }
    }

    #[derive(Default)]
    struct FixtureRuntime {
        calls: Mutex<Vec<(ExecutionTopology, String, String)>>,
    }

    #[async_trait]
    impl ExecutedBakeoffRuntime for FixtureRuntime {
        async fn execute(
            &self,
            _state: &DaemonState,
            topology: ExecutionTopology,
            request: &ExecutionRequest,
            config: &ExecutedBakeoffRuntimeConfig,
        ) -> Result<ExecutionRecord, String> {
            self.calls.lock().unwrap().push((
                topology,
                request.task_id.as_str().to_string(),
                config
                    .workspace_root_for(request.task_id.as_str())
                    .to_string(),
            ));
            Ok(ExecutionRecord {
                artifacts: vec![ExecutionArtifact {
                    name: RESPONSE_ARTIFACT_NAME.to_string(),
                    body: "fixture-output".to_string(),
                    produced_by_tool_call_id: Some("fixture-tool".to_string()),
                }],
                tool_calls: vec![ToolCallTrace {
                    id: "fixture-tool".to_string(),
                    tool_name: "fixture".to_string(),
                }],
                review_findings: Vec::new(),
                observed_cost_usd: if topology == ExecutionTopology::RoutedOrchestrated {
                    0.02
                } else {
                    0.019
                },
                escalation_count: 0,
                observability: ExecutionObservability {
                    useful_tool_attribution: true,
                    review_findings: true,
                    escalations: true,
                },
                execution_failure: None,
            })
        }
    }

    #[test]
    fn loader_adapts_private_and_harbor_tasks_without_exposing_verifier_inputs() {
        let fixture = corpus_fixture(false);
        let loaded = load_corpus_manifest(&fixture.manifest).unwrap();

        assert_eq!(loaded.body.tasks.len(), 2);
        assert_eq!(loaded.sources.private, 1);
        assert_eq!(loaded.sources.harbor, 1);
        assert_eq!(loaded.manifest_sha256.len(), 64);
        assert_eq!(loaded.body.tasks[0].prompt, "Return fixture-output.");
        assert_eq!(
            loaded.body.tasks[0].expected_output.as_deref(),
            Some("fixture-output")
        );
        assert_ne!(
            loaded.body.tasks[0].workspace_root,
            loaded.body.tasks[1].workspace_root
        );
    }

    #[test]
    fn loader_rejects_verifier_inputs_inside_agent_workspace() {
        let fixture = corpus_fixture(true);
        let error = match load_corpus_manifest(&fixture.manifest) {
            Ok(_) => panic!("verifier input inside the agent workspace was accepted"),
            Err(error) => error,
        };
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.message.contains("outside the agent workspace"));
    }

    #[test]
    fn loader_rejects_committed_sibling_verifier_inside_workspace_git_toplevel() {
        let fixture = corpus_fixture(false);
        let _ = fs::remove_dir_all(fixture.root.join("workspaces/private/.git"));
        let _ = fs::remove_dir_all(fixture.root.join("workspaces/harbor/.git"));
        init_clean_git_workspace(&fixture.root);

        let error = match load_corpus_manifest(&fixture.manifest) {
            Ok(_) => panic!("committed sibling verifier was exposed to the agent worktree"),
            Err(error) => error,
        };

        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error
            .message
            .contains("outside the agent workspace Git repository"));
    }

    #[test]
    fn corpus_digest_changes_when_resolved_instruction_changes() {
        let fixture = corpus_fixture(false);
        let before = load_corpus_manifest(&fixture.manifest)
            .unwrap()
            .manifest_sha256;
        fs::write(
            fixture.root.join("private/instruction.md"),
            "Return changed-fixture-output.",
        )
        .unwrap();

        let after = load_corpus_manifest(&fixture.manifest)
            .unwrap()
            .manifest_sha256;

        assert_ne!(before, after);
    }

    #[test]
    fn corpus_digest_changes_when_verifier_expectation_changes() {
        let fixture = corpus_fixture(false);
        let before = load_corpus_manifest(&fixture.manifest)
            .unwrap()
            .manifest_sha256;
        fs::write(
            fixture.root.join("verifier/expected.txt"),
            "changed-expected-output",
        )
        .unwrap();

        let after = load_corpus_manifest(&fixture.manifest)
            .unwrap()
            .manifest_sha256;

        assert_ne!(before, after);
    }

    #[test]
    fn loader_builds_hidden_command_and_artifact_contract_for_coding_task() {
        let fixture = corpus_fixture(false);
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&fixture.manifest).unwrap()).unwrap();
        manifest["tasks"][0]["verifier"] = json!({
            "kind": "command",
            "command": ["cargo", "test", "--quiet"],
            "requiredArtifacts": [{
                "name": "result",
                "path": "result.txt",
                "expectedOutputPath": "verifier/expected.txt"
            }]
        });
        fs::write(
            &fixture.manifest,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let loaded = load_corpus_manifest(&fixture.manifest).unwrap();
        let task = &loaded.body.tasks[0];

        assert!(task.expected_output.is_none());
        let verifier = task.coding_verifier.as_ref().expect("coding verifier");
        assert_eq!(verifier.command, ["cargo", "test", "--quiet"]);
        assert_eq!(verifier.required_artifacts[0].path, "result.txt");
        assert_eq!(
            verifier.required_artifacts[0].expected_output,
            "fixture-output"
        );
    }

    #[tokio::test]
    async fn authenticated_operator_route_executes_mixed_corpus_and_aggregates_metrics() {
        let fixture = corpus_fixture(false);
        let runtime = Arc::new(FixtureRuntime::default());
        let state = DaemonState::new("corpus-token").with_executed_bakeoff_runtime(runtime.clone());
        let response = crate::api::build_router(state)
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/runtime/corpus-bakeoff")
                    .header("authorization", "Bearer corpus-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"manifestPath": fixture.manifest}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["schemaVersion"], CORPUS_REPORT_SCHEMA_VERSION);
        assert_eq!(body["taskCount"], 2);
        assert_eq!(body["sources"], json!({"private": 1, "harbor": 1}));
        assert_eq!(body["decision"]["passed"], true);
        assert_eq!(body["report"]["per_task"].as_array().unwrap().len(), 2);
        assert_eq!(body["report"]["comparison"]["same_cost_comparable"], true);
        let calls = runtime.calls.lock().unwrap();
        assert_eq!(calls.len(), 4);
        let workspaces = calls
            .iter()
            .map(|(_, _, workspace)| workspace)
            .collect::<BTreeSet<_>>();
        assert_eq!(workspaces.len(), 2);
    }

    #[tokio::test]
    async fn operator_route_requires_daemon_bearer() {
        let fixture = corpus_fixture(false);
        let response = crate::api::build_router(DaemonState::new("corpus-token"))
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/runtime/corpus-bakeoff")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"manifestPath": fixture.manifest}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
