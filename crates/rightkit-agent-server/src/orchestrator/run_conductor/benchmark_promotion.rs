impl DaemonState {
    /// Benchmark a role under a specific model: run the agent on `goal`
    /// `repeats` times and score each run with an independent verification.
    /// Returns a [`BenchmarkReport`]. Used for champion/challenger comparison.
    pub async fn benchmark_role(
        &self,
        role: &str,
        model_id: &str,
        goal: &str,
        repeats: u32,
        session_id: &str,
    ) -> rightkit_agent_orchestration::coding::BenchmarkReport {
        use rightkit_agent_tools::coding_tools::SubagentSpawner;

        // A temporary state whose registry forces the benchmarked role onto the
        // model under test; everything else (sessions, conduits) is shared.
        let mut bench_registry = AgentRegistry::with_builtins();
        let mut overrides = std::collections::HashMap::new();
        overrides.insert(
            role.to_string(),
            (model_id.to_string(), "openrouter".to_string()),
        );
        bench_registry.apply_config_overrides(&overrides);
        let mut bench_state = self.clone();
        bench_state.agent_registry = Arc::new(bench_registry);

        let mut successes = 0u32;
        for _ in 0..repeats {
            let produced = bench_state
                .spawn(
                    role,
                    goal,
                    session_id,
                    rightkit_agent_tools::coding_tools::SubagentSpawnOptions::default(),
                )
                .await;
            let ok = match produced {
                Ok(output) => {
                    let vp = format!(
                        "Goal: {goal}\n\nAn agent produced:\n{output}\n\nIndependently verify the \
                         goal is genuinely achieved in the workspace. Begin with exactly \
                         'VERDICT: PASS' or 'VERDICT: FAIL'."
                    );
                    // Verify with the daemon's real signoff (not the temp registry).
                    match self
                        .spawn(
                            "signoff",
                            &vp,
                            session_id,
                            rightkit_agent_tools::coding_tools::SubagentSpawnOptions::default(),
                        )
                        .await
                    {
                        Ok(v) => parse_verdict(&v).0,
                        Err(_) => false,
                    }
                }
                Err(_) => false,
            };
            if ok {
                successes += 1;
            }
        }
        rightkit_agent_orchestration::coding::BenchmarkReport::new(role, model_id, repeats, successes)
    }

    /// Run a champion/challenger comparison for a role and, if the challenger
    /// wins all gates, promote it by persisting the model override to config.
    pub async fn run_promotion(
        &self,
        role: &str,
        challenger_model: &str,
        goal: &str,
        repeats: u32,
        session_id: &str,
    ) -> rightkit_agent_orchestration::coding::PromotionDecision {
        let champion_model = self
            .agent_registry
            .get(role)
            .map(|d| d.model_id.clone())
            .unwrap_or_default();

        let champion = self
            .benchmark_role(role, &champion_model, goal, repeats, session_id)
            .await;
        let challenger = self
            .benchmark_role(role, challenger_model, goal, repeats, session_id)
            .await;
        let decision = rightkit_agent_orchestration::coding::decide_promotion(
            &champion,
            &challenger,
            &rightkit_agent_orchestration::coding::PromotionConfig::default(),
        );

        if let rightkit_agent_orchestration::coding::PromotionDecision::Promote { to_model, .. } = &decision {
            // Apply as a canary and begin a staged rollout: subsequent live runs
            // for this role advance it to Full or auto-roll-back.
            persist_agent_model(role, to_model);
            self.rollouts
                .start(role, &champion_model, to_model, champion.success_rate());
            self.audit.append(
                "model_promotion_canary",
                role,
                &format!(
                    "{champion_model} ({:.2}) → {to_model} ({:.2}) [canary]",
                    champion.success_rate(),
                    challenger.success_rate()
                ),
            );
            if let Some(session) = self.get_session(session_id) {
                let mut fields = std::collections::HashMap::new();
                fields.insert("role".to_string(), role.to_string());
                fields.insert("from".to_string(), champion_model.clone());
                fields.insert("to".to_string(), to_model.clone());
                fields.insert(
                    "championRate".to_string(),
                    format!("{:.2}", champion.success_rate()),
                );
                fields.insert(
                    "challengerRate".to_string(),
                    format!("{:.2}", challenger.success_rate()),
                );
                let seq = session.conduit.next_seq();
                session
                    .conduit
                    .publish(ProtocolEvent::OrchestrationObservationRecorded {
                        session_id: session_id.to_string(),
                        seq,
                        created_at: now_iso(),
                        observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                            id: format!("promotion-{}", now_millis()),
                            kind: "model_promotion".into(),
                            subject: role.to_string(),
                            summary: format!(
                                "Promoted '{role}' from {champion_model} to {to_model} (takes effect on restart)"
                            ),
                            fields,
                            created_at: now_iso(),
                        },
                    });
            }
        }
        decision
    }

    /// Construct an [`LlmClient`] for a given agent role, resolving its model and
    /// provider key (falling back to the global engine config).
    fn build_llm_for_role_in_workspace(
        &self,
        role: &str,
        workspace_config: &rightkit_app_settings::agent_config::HostConfig,
        selected_model: Option<&str>,
    ) -> Result<Arc<dyn LlmClient>, String> {
        use rightkit_llm::agent_providers::OpenAiCompatClient;
        use rightkit_agent_tools::coding_tools::ToolRegistry;

        let global = self
            .engine_cfg
            .as_ref()
            .ok_or("no provider configured (set an API key)")?;
        let (model, base_url, api_key) = if let Some(selected_model) = selected_model
            .map(str::trim)
            .filter(|model| !model.is_empty())
        {
            let route = self.provider_route_for_model(
                workspace_config,
                selected_model,
                global,
                None,
                None,
                false,
            );
            (route.model_id, route.base_url, route.api_key)
        } else {
            match self.agent_descriptor_with_workspace_config(workspace_config, role) {
                Some(d) => {
                    let base = rightkit_llm::agent_providers::find_provider(&d.provider_id)
                        .map(|p| p.base_url.to_string())
                        .unwrap_or_else(|| global.base_url.clone());
                    let key = rightkit_llm::agent_providers::load_api_key(&d.provider_id)
                        .unwrap_or_else(|| global.api_key.clone());
                    (
                        rightkit_llm::agent_providers::provider_model_id(&d.provider_id, &d.model_id),
                        base,
                        key,
                    )
                }
                None => (
                    global.model_id.clone(),
                    global.base_url.clone(),
                    global.api_key.clone(),
                ),
            }
        };
        let client = OpenAiCompatClient::new(&api_key, &model, &base_url);
        // The planner needs no tools; advertise none.
        let tools = Arc::new(ToolRegistry::new());
        Ok(Arc::new(ProviderLlmClient::new(client, tools)))
    }
}
