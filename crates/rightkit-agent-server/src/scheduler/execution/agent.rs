pub(super) async fn fire_schedule(state: &DaemonState, root: &Path, entry: &ScheduleEntry) {
    // A schedule may fire repeatedly; every occurrence gets an isolated
    // session namespace so transcript/provider/browser/tool/budget/evidence
    // state cannot bleed between runs.
    // Provenance: adapted to Rust from OpenMausBot `server/routines.ts`
    // `RoutineManager::newRun`; RightKit uses canonical UUIDv7 for sortable,
    // collision-safe isolated execution receipts.
    let session_id = format!(
        "schedule-{}-{}",
        sanitize_id(&entry.id),
        rightkit_agent_engine_protocol::uuid_v7()
    );
    let start_span_id = record_scheduled_job_span(
        state,
        MiddlewarePhase::ScheduledJobStart,
        "started",
        &session_id,
        None,
        root,
        entry,
        None,
        serde_json::json!({}),
    );
    let metadata = load_runtime_metadata(root, entry).unwrap_or_default();
    let outcome = fire_schedule_inner(state, root, entry, &session_id, metadata).await;
    record_scheduled_job_span(
        state,
        MiddlewarePhase::ScheduledJobEnd,
        &outcome.status,
        &session_id,
        start_span_id.as_deref(),
        root,
        entry,
        Some(&outcome.message),
        serde_json::json!({}),
    );
}

async fn fire_schedule_inner(
    state: &DaemonState,
    root: &Path,
    entry: &ScheduleEntry,
    session_id: &str,
    metadata: ScheduleRuntimeMetadata,
) -> ScheduleFireOutcome {
    // S:N3a — every occurrence derives its own isolation namespace; every
    // state surface for this run is keyed by this session id and the namespace
    // is persisted on each run event so isolation is verifiable, not implied.
    let isolation = TaskIsolationV1::fresh(session_id).ok();
    // S:N4 — hop 0 of the causal receipt chain, captured at fire time.
    let trigger = trigger_receipt(&entry.id, session_id, current_epoch_millis());
    if entry.mode.eq_ignore_ascii_case("scriptOnly") {
        return fire_script_only_schedule(state, root, entry, session_id).await;
    }
    if !entry.mode.eq_ignore_ascii_case("agent") {
        return record_schedule_failure(
            root,
            entry,
            session_id,
            "only agent-mode schedules can run in this slice",
        );
    }
    // Fail closed: agent-mode non-local backends are not yet routed.
    if let Some(backend) = entry
        .execution_backend
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty() && *b != "local")
    {
        return record_schedule_failure(
            root,
            entry,
            session_id,
            &format!("execution backend '{backend}' is not routed for agent execution yet"),
        );
    }
    let rendered_recipe = match render_scheduled_recipe(state, entry) {
        Ok(value) => value,
        Err(message) => {
            return record_schedule_failure(root, entry, session_id, &message);
        }
    };
    let title_source = rendered_recipe
        .as_ref()
        .map(|rendered| rendered.title.as_str())
        .unwrap_or(entry.prompt.as_str());
    let title = format!(
        "Scheduled: {}",
        title_source.chars().take(48).collect::<String>()
    );
    let workspace = root.to_string_lossy().to_string();
    let mut options = crate::api::SessionRuntimeOptions {
        model: metadata
            .profile
            .as_ref()
            .map(|profile| profile.model.clone()),
        fallback_models: metadata
            .profile
            .as_ref()
            .map(|profile| profile.fallback_models.clone())
            .unwrap_or_default(),
        allowed_tools: metadata
            .profile
            .as_ref()
            .map(|profile| {
                profile
                    .tools
                    .iter()
                    .chain(profile.tool_grants.iter())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default(),
        ..Default::default()
    };
    if let Some(toolset_id) = entry
        .toolset
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let profile = match crate::api_toolsets::resolve_toolset_profile(toolset_id) {
            Ok(profile) => profile,
            Err(_) => {
                return record_schedule_failure(
                    root,
                    entry,
                    session_id,
                    &format!("unknown schedule toolset '{toolset_id}'"),
                );
            }
        };
        let (allowed_tools, disallowed_tools) = crate::api_toolsets::apply_toolset_profile(
            Some(&profile),
            options.allowed_tools,
            options.disallowed_tools,
        );
        options.allowed_tools = allowed_tools;
        options.disallowed_tools = disallowed_tools;
        options.toolset = Some(profile.id);
    }
    if metadata
        .profile
        .as_ref()
        .is_some_and(|profile| profile.browser.mode.eq_ignore_ascii_case("disabled"))
    {
        options.disallowed_tools.push("browser".to_string());
    }
    if options.toolset.as_deref() != Some(crate::api_toolsets::SCHEDULE_ADMIN_TOOLSET)
        && !options
            .disallowed_tools
            .iter()
            .any(|tool| tool == "schedule_cron")
    {
        options.disallowed_tools.push("schedule_cron".to_string());
    }
    options.active_skills = clean_skill_ids(&entry.skills);
    options.execution_backend = clean_optional_string(entry.execution_backend.as_deref());
    options.mcp_channel = "schedule".to_string();
    options.budget_ceiling_usd = metadata
        .profile
        .as_ref()
        .and_then(|profile| profile.budget.max_cost_micros)
        .map(|micros| micros as f64 / 1_000_000.0);
    let role = metadata
        .profile
        .as_ref()
        .map(|profile| profile.role_pack.as_str())
        .unwrap_or("scheduled-bot");
    let _scheduled_session = state.create_session_with_options(
        session_id,
        Some(role),
        &workspace,
        &title,
        None,
        options,
    );
    let text = scheduled_prompt(
        entry,
        rendered_recipe
            .as_ref()
            .map(|rendered| rendered.rendered_prompt.as_str()),
    );
    let mut completion = state
        .get_session(session_id)
        .map(|session| session.conduit.subscribe());
    match state
        .submit_text_with_trust_delivery(
            session_id,
            &text,
            rightkit_agent_engine_protocol::ContentTrust::WorkspaceTrusted,
            "scheduler",
        )
        .await
    {
        Ok((_, delivered_to_engine)) => {
            if let Some(rendered) = rendered_recipe.as_ref() {
                record_scheduled_recipe_run(state, entry, session_id, rendered);
            }
            // S:N4 — run + child-task hops are captured at submission time,
            // not fabricated retroactively at terminal time.
            let mut chain = vec![trigger];
            if delivered_to_engine {
                let admitted_ms = current_epoch_millis();
                chain.push(run_receipt(&chain[0], session_id, session_id, admitted_ms));
                chain.push(child_task_receipt(
                    &chain[1],
                    session_id,
                    session_id,
                    admitted_ms,
                ));
            }
            let submitted_message = causal_message_with_isolation(
                "submitted scheduled prompt",
                &chain,
                isolation.as_ref(),
            );
            let mut event = schedule_run_event(
                entry,
                current_epoch_minute(),
                "submitted",
                session_id,
                &submitted_message,
            );
            if delivered_to_engine {
                mark_local_execution_resolved(&mut event);
            }
            if let Err(e) = append_run_event(root, event) {
                warn!(schedule_id = %entry.id, error = %e, "failed to record submitted schedule");
            }
            if delivered_to_engine {
                if let Some(mut subscription) = completion.take() {
                    let root = root.to_path_buf();
                    let entry = entry.clone();
                    let session_id = session_id.to_string();
                    let timeout_seconds = metadata
                        .profile
                        .as_ref()
                        .and_then(|profile| profile.budget.max_seconds)
                        .unwrap_or(300)
                        .clamp(1, 900);
                    let verifier_binding = metadata
                        .profile
                        .as_ref()
                        .map(|profile| profile.verifier.clone());
                    // Cheap Arc-backed clone so the detached finalizer owns a
                    // usable state handle (session reads + verifier spawn).
                    let state = state.clone();
                    tokio::spawn(async move {
                        let terminal = async {
                            loop {
                                match subscription.recv().await {
                                    rightkit_agent_engine_core::conduit::ConduitReceive::Event(
                                        rightkit_agent_engine_protocol::ProtocolEvent::SessionTurnFinished {
                                            session_id: completed_session,
                                            status,
                                            ..
                                        },
                                    ) if completed_session == session_id => return Some(status),
                                    rightkit_agent_engine_core::conduit::ConduitReceive::Event(_) => continue,
                                    rightkit_agent_engine_core::conduit::ConduitReceive::Lagged {
                                        ..
                                    } => continue,
                                    rightkit_agent_engine_core::conduit::ConduitReceive::Closed => {
                                        return None
                                    }
                                }
                            }
                        };
                        let (status, message) = match tokio::time::timeout(
                            Duration::from_secs(timeout_seconds),
                            terminal,
                        )
                        .await
                        {
                            Ok(Some(status)) => (status, "scheduled prompt completed"),
                            Ok(None) => ("failed".to_string(), "scheduled prompt session closed"),
                            Err(_) => ("failed".to_string(), "scheduled prompt timed out"),
                        };
                        // S:N2 + S:N4 — execute the bound post-run verifier,
                        // build the real hop-distinct receipt chain, and gate
                        // the terminal status before persisting the outcome.
                        let (final_status, final_message) = finalize_agent_run(
                            &state,
                            &root,
                            &entry,
                            &session_id,
                            &status,
                            message,
                            chain,
                            isolation.as_ref(),
                            verifier_binding,
                        )
                        .await;
                        let mut terminal_event = schedule_run_event(
                            &entry,
                            current_epoch_minute(),
                            &final_status,
                            &session_id,
                            &final_message,
                        );
                        terminal_event.finished_at = Some(crate::api::now_iso());
                        let _ = append_run_event(&root, terminal_event);
                        if metadata.lifecycle.overlap.eq_ignore_ascii_case("queue-one") {
                            // Release queued occurrence for the daemon claim
                            // loop, which drains it in same minute without
                            // recursively spawning a non-Send execution task.
                            let _ = release_queued_occurrence(&root, &entry.id);
                        }
                    });
                }
            }
            ScheduleFireOutcome {
                status: "submitted".to_string(),
                message: "submitted scheduled prompt".to_string(),
            }
        }
        Err(e) => {
            warn!(schedule_id = %entry.id, error = %e, "failed to submit schedule");
            record_schedule_failure(root, entry, session_id, &e)
        }
    }
}

/// S:N2 + S:N4 — fold the run's terminal outcome, the post-run verifier
/// verdict, and the hop-distinct causal receipt chain into the terminal run
/// event. Returns `(terminal status, structured message JSON)`.
#[allow(clippy::too_many_arguments)]
async fn finalize_agent_run(
    state: &DaemonState,
    root: &Path,
    entry: &ScheduleEntry,
    session_id: &str,
    status: &str,
    human_message: &str,
    mut chain: Vec<CausalReceipt>,
    isolation: Option<&TaskIsolationV1>,
    verifier_binding: Option<crate::api_schedules::VerifierBinding>,
) -> (String, String) {
    let mut final_status = status.to_string();
    let mut display = human_message.to_string();
    let mut verdict_outcome: Option<VerifierVerdict> = None;
    let completed = status == "completed";
    let configured = verifier_binding
        .as_ref()
        .is_some_and(|binding| verifier_spec(binding).is_configured());
    if completed && configured {
        let result_excerpt = session_result_excerpt(state, session_id);
        // Unwrap-safe: `configured` proves the binding exists.
        let binding = verifier_binding
            .clone()
            .expect("configured verifier binding");
        let (verifier_session_id, verdict) = run_post_run_verifier(
            state,
            root,
            entry,
            session_id,
            status,
            &result_excerpt,
            &binding,
        )
        .await;
        let admitted_ms = current_epoch_millis();
        let verifier_hop = child_task_receipt(
            chain.last().expect("chain has at least the trigger hop"),
            &verifier_session_id,
            session_id,
            admitted_ms,
        );
        chain.push(verifier_hop);
        verdict_outcome = Some(verdict);
    }
    if let Some(verdict) = &verdict_outcome {
        // The verdict is itself the Result hop, recorded before gating so a
        // blocked run still carries its full causal evidence.
        let result = result_receipt(
            chain.last().expect("chain has at least the trigger hop"),
            &outcome_result_id(status, session_id, Some(verdict)),
            session_id,
            current_epoch_millis(),
        );
        chain.push(result);
        let required = verifier_binding
            .as_ref()
            .map(|binding| binding.required)
            .unwrap_or(false);
        if let rightkit_workflow::control_plane::verifier_binding::TerminalGate::Blocked {
            reason,
        } = gate_terminal_status(verdict, required)
        {
            // A required verifier that does not approve holds the run out of
            // the completed state; "failed" feeds bounded retry and overlap
            // accounting unchanged.
            display = format!("verifier blocked completion: {reason}");
            final_status = "failed".to_string();
        }
    } else {
        let outcome = result_receipt(
            chain.last().expect("chain has at least the trigger hop"),
            &outcome_result_id(status, session_id, None),
            session_id,
            current_epoch_millis(),
        );
        chain.push(outcome);
    }
    (
        final_status,
        structured_terminal_message(&display, &chain, isolation, verdict_outcome.as_ref()),
    )
}

/// Map the stored API binding onto the pure workflow-crate spec.
fn verifier_spec(
    binding: &crate::api_schedules::VerifierBinding,
) -> rightkit_workflow::control_plane::verifier_binding::VerifierBindingSpec {
    rightkit_workflow::control_plane::verifier_binding::VerifierBindingSpec {
        role_pack: binding.role_pack.clone(),
        model: binding.model.clone(),
        required: binding.required,
    }
}

/// S:N2 — execute the bound post-run verifier in its own isolated session and
/// classify its terminal output into a typed verdict. Missing verdicts fail
/// closed.
async fn run_post_run_verifier(
    state: &DaemonState,
    root: &Path,
    entry: &ScheduleEntry,
    run_session_id: &str,
    run_status: &str,
    result_excerpt: &str,
    binding: &crate::api_schedules::VerifierBinding,
) -> (String, VerifierVerdict) {
    let verifier_session_id = format!("{run_session_id}-verifier");
    // The verifier derives its own isolation namespace so verifier
    // transcript/provider/tool/budget state never shares the verified run's.
    if TaskIsolationV1::fresh(&verifier_session_id).is_err() {
        return (
            verifier_session_id,
            VerifierVerdict::Unavailable {
                reason: "verifier namespace could not be derived".into(),
            },
        );
    }
    let role = binding
        .role_pack
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("scheduled-verifier")
        .to_string();
    let options = crate::api::SessionRuntimeOptions {
        model: binding
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        mcp_channel: "schedule-verifier".to_string(),
        ..Default::default()
    };
    let workspace = root.to_string_lossy().to_string();
    let title = format!("Verifier: {}", entry.id);
    state.create_session_with_options(
        &verifier_session_id,
        Some(&role),
        &workspace,
        &title,
        Some(run_session_id),
        options,
    );
    let prompt = verifier_prompt(&entry.id, run_session_id, run_status, result_excerpt);
    let completion = state
        .get_session(&verifier_session_id)
        .map(|session| session.conduit.subscribe());
    match state
        .submit_text_with_trust_delivery(
            &verifier_session_id,
            &prompt,
            rightkit_agent_engine_protocol::ContentTrust::WorkspaceTrusted,
            "schedule-verifier",
        )
        .await
    {
        Ok((_, true)) => {
            let Some(mut subscription) = completion else {
                return (
                    verifier_session_id,
                    VerifierVerdict::Unavailable {
                        reason: "verifier session conduit unavailable".into(),
                    },
                );
            };
            let terminal = async {
                loop {
                    match subscription.recv().await {
                        rightkit_agent_engine_core::conduit::ConduitReceive::Event(
                            rightkit_agent_engine_protocol::ProtocolEvent::SessionTurnFinished {
                                session_id: completed_session,
                                status,
                                ..
                            },
                        ) if completed_session == verifier_session_id => return Some(status),
                        rightkit_agent_engine_core::conduit::ConduitReceive::Event(_) => continue,
                        rightkit_agent_engine_core::conduit::ConduitReceive::Lagged { .. } => {
                            continue
                        }
                        rightkit_agent_engine_core::conduit::ConduitReceive::Closed => return None,
                    }
                }
            };
            // Verifiers are bounded tighter than runs: 300s ceiling so a hung
            // verifier cannot pin the occurrence's terminal task forever.
            match tokio::time::timeout(Duration::from_secs(300), terminal).await {
                Ok(Some(status)) => {
                    let text = session_result_excerpt(state, &verifier_session_id);
                    (
                        verifier_session_id.clone(),
                        classify_verdict(&status, &text),
                    )
                }
                Ok(None) => (
                    verifier_session_id.clone(),
                    VerifierVerdict::Unavailable {
                        reason: "verifier session closed before a verdict".into(),
                    },
                ),
                Err(_) => (
                    verifier_session_id,
                    VerifierVerdict::Unavailable {
                        reason: "verifier session timed out".into(),
                    },
                ),
            }
        }
        Ok((_, false)) => (
            verifier_session_id,
            VerifierVerdict::Unavailable {
                reason: "no engine delivery for the verifier session".into(),
            },
        ),
        Err(e) => (
            verifier_session_id,
            VerifierVerdict::Unavailable {
                reason: format!("verifier submission failed: {e}"),
            },
        ),
    }
}

/// Bounded final assistant text of a session, used as the verified result
/// excerpt and the verifier's own classified output.
fn session_result_excerpt(state: &DaemonState, session_id: &str) -> String {
    let Some(session) = state.get_session(session_id) else {
        return String::new();
    };
    let transcript = lock_recover(&session.transcript, "session.transcript");
    for message in transcript.iter().rev() {
        if let TranscriptMessage::Assistant { content, .. } = message {
            if !content.trim().is_empty() {
                return content.clone();
            }
        }
    }
    String::new()
}

fn outcome_result_id(status: &str, session_id: &str, verdict: Option<&VerifierVerdict>) -> String {
    match verdict {
        Some(verdict) => {
            let tag = match verdict {
                VerifierVerdict::Approved => "approved",
                VerifierVerdict::Rejected { .. } => "rejected",
                VerifierVerdict::Unavailable { .. } => "unavailable",
            };
            format!("verdict:{tag}:{session_id}")
        }
        None => format!("outcome:{status}:{session_id}"),
    }
}

fn structured_terminal_message(
    display: &str,
    chain: &[CausalReceipt],
    isolation: Option<&TaskIsolationV1>,
    verdict: Option<&VerifierVerdict>,
) -> String {
    let mut payload = serde_json::json!({
        "message": display,
        "causalReceipts": chain,
    });
    if let Some(isolation) = isolation {
        if let Ok(value) = serde_json::to_value(isolation) {
            payload["taskIsolation"] = value;
        }
    }
    if let Some(verdict) = verdict {
        if let Ok(value) = serde_json::to_value(verdict) {
            payload["verifierVerdict"] = value;
        }
    }
    payload.to_string()
}

fn render_scheduled_recipe(
    state: &DaemonState,
    entry: &ScheduleEntry,
) -> Result<Option<rightkit_agent_recipes::RenderedRecipe>, String> {
    let Some(recipe_id) = entry
        .source_recipe_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let installed = state
        .recipes
        .lock()
        .unwrap()
        .get(recipe_id)
        .ok_or_else(|| format!("scheduled recipe '{recipe_id}' is not installed"))?;
    render_recipe(&installed.recipe, &entry.recipe_params)
        .map(Some)
        .map_err(|e| e.message)
}

fn record_scheduled_recipe_run(
    state: &DaemonState,
    entry: &ScheduleEntry,
    session_id: &str,
    rendered: &rightkit_agent_recipes::RenderedRecipe,
) {
    let Some(recipe_id) = entry.source_recipe_id.clone() else {
        return;
    };
    let run = RecipeRun {
        id: format!("rr-{}", current_epoch_millis()),
        recipe_id,
        session_id: session_id.to_string(),
        status: RecipeRunStatus::Submitted,
        params: rendered.params.clone(),
        rendered_prompt_hash: rendered.rendered_prompt_hash.clone(),
        result_artifact_id: None,
        created_at: crate::api::now_iso(),
        completed_at: None,
    };
    let snapshot = {
        let mut store = lock_recover(&state.recipes, "state.recipes");
        store.add_run(run);
        store.snapshot()
    };
    if let Err(e) = crate::api_recipes::persist_recipe_store_snapshot(state, &snapshot) {
        warn!(schedule_id = %entry.id, error = %e, "failed to persist scheduled recipe run snapshot");
    }
}
