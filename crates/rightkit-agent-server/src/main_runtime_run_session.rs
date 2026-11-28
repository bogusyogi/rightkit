// KRN run-session composition.
//
// `run_session` is the single CLI/TUI semantic execution path. Host selection
// happens below presentation: InlineHost runs the existing runtime
// in-process, while ServiceHost adapts the existing daemon transport to the
// same session/event contract.

include!("main_runtime_session_host.rs");

fn input_event_kind(event: &RunInputEvent) -> &'static str {
    match event {
        RunInputEvent::UserMessage { .. } => "user_message",
        RunInputEvent::ToolResult { .. } => "tool_result",
        RunInputEvent::Control { .. } => "control",
        RunInputEvent::ApprovalResponse { .. } => "approval_response",
    }
}

mod hosted_run_session {
    use super::*;

    // Shared output/event helpers and their existing regression tests. This
    // file no longer contains an executable daemon-only run loop.
    include!("main_runtime_run_session_service.rs");

    /// Canonical Refactor-1 run path. Both InlineHost and ServiceHost enter the
    /// same prompt normalization, output projection, semantic event handling,
    /// resync, terminal disposition, and exit-code logic here.
    pub(super) async fn run_hosted(args: super::RunArgs, control: Arc<RunCancellation>) -> i32 {
        run_hosted_with(args, control, super::SessionExecutionHost::select_for_call).await
    }

    async fn run_hosted_with<F, Fut>(
        mut args: super::RunArgs,
        control: Arc<RunCancellation>,
        select_host: F,
    ) -> i32
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<super::SessionExecutionHost, String>>,
    {
        if let Err(message) = validate_run_options(&args) {
            return run_error_exit(
                select_run_output_mode(&args, None),
                RunExitCode::Usage,
                &message,
            );
        }
        args.system_prompt = match resolve_prompt_text(
            "--system-prompt",
            args.system_prompt.take(),
            "--system-prompt-file",
            args.system_prompt_file.take(),
        ) {
            Ok(value) => value,
            Err(message) => {
                return run_error_exit(
                    select_run_output_mode(&args, None),
                    RunExitCode::Usage,
                    &message,
                );
            }
        };
        let append_prompt = match resolve_prompt_text(
            "--append-system-prompt",
            args.append_system_prompt.take(),
            "--append-system-prompt-file",
            args.append_system_prompt_file.take(),
        ) {
            Ok(value) => value,
            Err(message) => {
                return run_error_exit(
                    select_run_output_mode(&args, None),
                    RunExitCode::Usage,
                    &message,
                );
            }
        };
        args.append_system_prompt = append_prompt;
        let RunArgs {
            prompt,
            workspace,
            session_id,
            exit_on_error,
            input_format,
            output_schema,
            ..
        } = args.clone();
        let (prepared_append, loaded_schema) =
            match prepare_output_schema(output_schema.clone(), args.append_system_prompt.clone()) {
                Ok(prepared) => prepared,
                Err((code, message)) => {
                    let code = if code == RunExitCode::SchemaViolation.as_i32() {
                        RunExitCode::SchemaViolation
                    } else {
                        RunExitCode::Usage
                    };
                    return run_error_exit(select_run_output_mode(&args, None), code, &message);
                }
            };
        args.append_system_prompt = prepared_append;
        let output_mode = select_run_output_mode(&args, loaded_schema.as_ref());
        // Start JSONL ingestion before host admission. The first user message
        // gates admission, while later records remain live for the run loop.
        let (prompt, mut input_stream) = match input_format {
            InputFormat::Text => (prompt, None),
            InputFormat::StreamJson => {
                let mut input = spawn_jsonl_reader(std::io::BufReader::new(std::io::stdin()), 32);
                let prompt = loop {
                    match input.recv().await {
                        Some(Ok((_line, RunInputEvent::UserMessage { text, .. }))) => break text,
                        Some(Ok((
                            _,
                            RunInputEvent::Control {
                                action: ControlAction::Abort,
                                ..
                            },
                        ))) => {
                            control.settled();
                            return interrupted_exit(output_mode);
                        }
                        Some(Ok((line, event))) => {
                            // Headless R11: stderr carries only the error object and NDJSON
                            // events in machine mode, so a refused line is logged, not printed.
                            tracing::warn!(
                                line,
                                kind = ?input_event_kind(&event),
                                "stream-json input line refused before initial user_message"
                            );
                        }
                        Some(Err(error)) => {
                            return run_error_exit(
                                output_mode,
                                RunExitCode::SchemaViolation,
                                &error.to_string(),
                            );
                        }
                        None => {
                            let message =
                                "--input-format stream-json stdin ended before a user_message event";
                            return run_error_exit(output_mode, RunExitCode::Usage, message);
                        }
                    }
                };
                (prompt, Some(input))
            }
        };
        if prompt.trim().is_empty() {
            let message = format!(
                "non-interactive `{app} run` requires a non-empty prompt; use --pipe or --input-format stream-json for stdin input",
                app = crate::host_identity::app()
            );
            return run_error_exit(output_mode, RunExitCode::Usage, &message);
        }
        let attachments = match headless_input::read_cli_image_attachments(&args.image) {
            Ok(images) => (!images.is_empty()).then_some(images),
            Err(message) => return run_error_exit(output_mode, RunExitCode::Usage, &message),
        };

        let workspace_root = workspace.map(std::path::PathBuf::from).unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        });
        let workspace_root = workspace_root.display().to_string();
        let session_id = resolve_run_session_id(session_id);
        if let Some(message) = crate::main_runtime::missing_required_route_key(
            std::path::Path::new(&workspace_root),
            args.model.as_deref(),
            args.provider.as_deref(),
        ) {
            return run_error_exit(output_mode, RunExitCode::ProviderAuthFailure, &message);
        }
        // A headless run with no resolvable provider would otherwise get the
        // engine's canned "no model provider is active" reply and exit 0, so
        // a script would read it as success. Resolve the route the same way
        // the engine does and fail typed before anything starts. (The TUI,
        // `Silent`, shows that reply to a person and keeps going.)
        if !matches!(output_mode, RunOutputMode::Silent) && vendor_route_for_args(&args).is_none() {
            let env_vars: std::collections::HashMap<String, String> = std::env::vars().collect();
            let config = crate::main_runtime::load_merged_config_for_workspace(
                std::path::Path::new(&workspace_root),
                &env_vars,
            );
            if crate::main_runtime::resolve_engine_config(
                &config,
                args.model.as_deref(),
                args.provider.as_deref(),
            )
            .is_none()
            {
                let routes = config.provider_routes.keys().cloned().collect::<Vec<_>>();
                let message = if routes.is_empty() {
                    format!(
                        "no model provider is configured: add a provider key (`{app} onboard` or Settings > Providers) or a provider route in config.toml",
                        app = crate::host_identity::app()
                    )
                } else {
                    format!(
                        "no model provider resolved for this run: configured routes are {}; pick one with `--model <route>::<model>` or set it as the default",
                        routes.join(", ")
                    )
                };
                return run_error_exit(output_mode, RunExitCode::ProviderAuthFailure, &message);
            }
        }
        // Vendor-CLI routes (`claude-code/*`, `antigravity/*`) run through
        // the vendor's installed CLI rather than a session-execution host.
        if let Some((vendor_kind, vendor_model)) = vendor_route_for_args(&args) {
            if let Err(message) = validate_vendor_cli_options(&args, vendor_kind) {
                return run_error_exit(output_mode, RunExitCode::Usage, &message);
            }
            return run_vendor_turn_headless(
                &args,
                vendor_kind,
                vendor_model,
                session_id,
                workspace_root,
                control,
            )
            .await;
        }
        if control.requested.is_cancelled() {
            return interrupted_exit(output_mode);
        }
        // Nothing ran when no host could be had: exit 3 `unreachable`.
        let host = match select_host().await {
            Ok(host) => host,
            Err(error) => {
                return run_error_exit(output_mode, RunExitCode::Unreachable, &error);
            }
        };

        // A headless resume of a stored session whose workspace folder is gone would continue in
        // whatever folder the caller happens to be in; say so and name the way out instead. The
        // terminal (Silent) draws its own three-way choice.
        if !matches!(output_mode, RunOutputMode::Silent)
            && !RUN_FORKED_SESSION.load(std::sync::atomic::Ordering::Relaxed)
        {
            if let Ok(Some(row)) = host.session_snapshot(&session_id).await {
                if let Some(message) = missing_session_workspace_message(&row, &workspace_root) {
                    return run_error_exit(output_mode, RunExitCode::Usage, &message);
                }
            }
        }
        if let Err(error) = host
            .ensure_session(&args, &session_id, &workspace_root)
            .await
        {
            return run_error_exit(output_mode, RunExitCode::Generic, &error);
        }
        if control.requested.is_cancelled() {
            return interrupted_exit(output_mode);
        }
        // Do not race cancellation against POST: the request may already have
        // admitted a turn before its response reaches this presentation.
        let _ = control.host.set((host.clone(), session_id.clone()));
        control
            .admission_started
            .store(true, std::sync::atomic::Ordering::Release);
        let mut after = match host
            .submit_message_with_nonce(
                &session_id,
                &prompt,
                &workspace_root,
                attachments,
                Some(matches!(output_mode, RunOutputMode::Silent)),
                RUN_REQUEST_ID.get().map(String::as_str),
            )
            .await
        {
            Ok(sequence) => sequence,
            Err(error) => {
                return run_error_exit(output_mode, RunExitCode::Generic, &error);
            }
        };

        let mut first_error = None;
        let mut output_state = RunOutputState {
            schema_retries_left: SCHEMA_RETRIES.load(std::sync::atomic::Ordering::Relaxed),
            ..Default::default()
        };
        if let Some(reason) = host.context_degraded_reason() {
            // D18: typed degraded signal, never a silent shadow store.
            if matches!(output_mode, RunOutputMode::StreamJson) {
                emit_cli_json_line(&serde_json::json!({
                    "schema_version": crate::host_identity::schema("cli.stream_event.v1"),
                    "event": "contextDegraded",
                    "seq": 0,
                    "session_id": session_id,
                    "ts": "",
                    "data": { "$kind": "contextDegraded", "reason": reason },
                }));
            } else if !matches!(output_mode, RunOutputMode::Silent | RunOutputMode::Json) {
                eprintln!("warning: context off ({reason}); running without context");
            }
            output_state.context_degraded = Some(reason);
        }
        let mut reconnects = 0_u8;
        let host_label = host.label();
        // A TUI presents cards to a person (tools rule 1): its stream is a human
        // subscriber. A headless run's stream is not, so its cards are denied
        // as `unattended`, unless `--approval-callback` makes its client the one who answers.
        let human = matches!(output_mode, RunOutputMode::Silent) || args.approval_callback;
        let mut interrupt_sent = false;
        // R15: the moment the interrupt was sent; the settle wait is bounded.
        let mut interrupt_at: Option<tokio::time::Instant> = None;
        let mut unattended_interaction_error: Option<String> = None;
        let mut unattended_interaction_exit_code: Option<RunExitCode> = None;
        let mut denied_unattended = false;
        // `--approval-callback`: approvals the run raised that its client has not answered yet.
        let mut open_approvals: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut pending_input_messages = std::collections::VecDeque::new();
        // Highest durable-fact seq already presented. Live delivery and the
        // durable history catch-up can both carry the same durable event;
        // each seq is presented once. Live projections (deltas) share the
        // pending turn seq and are never deduped here.
        let mut last_durable_seq = after;
        let is_live_projection = |event: &serde_json::Value| {
            matches!(
                daemon_event_kind(event),
                Some(
                    "assistantDelta"
                        | "toolOutputDelta"
                        | "toolArgumentsPartial"
                        | "streamResyncRequired"
                )
            )
        };

        let mut stream = match host.open_event_stream(&session_id, after, human).await {
            Ok(stream) => stream,
            Err(error) => {
                return run_error_exit(output_mode, RunExitCode::Generic, &error);
            }
        };

        // `--schema-retries`: an answer that broke the schema is sent back as a new turn instead of
        // ending the run. `$next` is the loop's own `continue`.
        macro_rules! retry_schema_answer {
            ($next:expr) => {
                if let Some(text) = output_state.schema_retry_prompt.take() {
                    match host
                        .submit_message(&session_id, &text, &workspace_root)
                        .await
                    {
                        Ok(sequence) => {
                            after = sequence;
                            last_durable_seq = sequence;
                            stream = match host.open_event_stream(&session_id, after, human).await {
                                Ok(stream) => stream,
                                Err(error) => {
                                    return run_error_exit(
                                        output_mode,
                                        RunExitCode::Generic,
                                        &error,
                                    )
                                }
                            };
                            first_error = None;
                            let retries_left = output_state.schema_retries_left;
                            output_state = RunOutputState::default();
                            output_state.schema_retries_left = retries_left;
                            reconnects = 0;
                            interrupt_sent = false;
                            $next
                        }
                        Err(error) => {
                            return run_error_exit(output_mode, RunExitCode::Generic, &error)
                        }
                    }
                }
            };
        }

        'run: loop {
            // A terminal (`human`) has its own bounded wait in `cancel_and_wait` and keeps the turn while the engine is
            // still stopping; ending the turn's task here would let a queued message start before the old turn settled.
            let settle_deadline = interrupt_settle_deadline(
                interrupt_sent,
                &mut interrupt_at,
                tokio::time::Instant::now(),
            )
            .filter(|_| !matches!(output_mode, RunOutputMode::Silent));
            let next = tokio::select! {
                _ = async {
                    match settle_deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending::<()>().await,
                    }
                }, if settle_deadline.is_some() => {
                    // R15: the interrupt did not settle inside the wait: the turn
                    // may still be running, so the outcome is unknown (exit 3).
                    return run_error_exit(
                        output_mode,
                        RunExitCode::Unknown,
                        "Turn cancellation is still awaiting engine settlement.",
                    );
                }
                _ = control.requested.cancelled(), if !interrupt_sent => {
                    // R15: an interrupt that did not settle (the host's ten second
                    // wait) leaves the outcome unknown: exit 3, never a guess.
                    if let Err(error) = host.interrupt(&session_id).await {
                        return run_error_exit(output_mode, RunExitCode::Unknown, &error);
                    }
                    interrupt_sent = true;
                    tracing::info!(session_id = %session_id, host = host_label, "turn_interrupt_requested");
                    continue;
                }
                input = async {
                    input_stream
                        .as_mut()
                        .expect("stream-json input branch is guarded")
                        .recv()
                        .await
                }, if input_stream.is_some() => {
                    match input {
                        Some(Ok((line, RunInputEvent::Control { action: ControlAction::Abort, reason }))) => {
                            eprintln!(
                                "stream-json input line {line}: abort{}",
                                reason.map(|value| format!(" ({value})")).unwrap_or_default()
                            );
                            control.requested.cancel();
                        }
                        Some(Ok((line, RunInputEvent::UserMessage { text, .. }))) => {
                            if text.trim().is_empty() {
                                eprintln!("stream-json input line {line}: empty user_message ignored");
                            } else {
                                pending_input_messages.push_back(text);
                            }
                        }
                        Some(Ok((
                            line,
                            RunInputEvent::ApprovalResponse {
                                tool_call_id,
                                approved,
                                remember,
                                reason,
                            },
                        ))) => {
                            if args.approval_callback && open_approvals.remove(&tool_call_id) {
                                // A resolve error means the engine already answered: ignore it.
                                let _ = host
                                    .resolve_approval_body(
                                        &session_id,
                                        approval_response_body(
                                            &tool_call_id,
                                            approved,
                                            remember.as_deref(),
                                            reason.as_deref(),
                                        ),
                                    )
                                    .await;
                            } else {
                                eprintln!(
                                    "stream-json input line {line}: approval_response for no open approval ignored"
                                );
                            }
                        }
                        Some(Ok((line, event))) => {
                            eprintln!(
                                "stream-json input line {line}: {:?} refused after initial turn admission",
                                input_event_kind(&event)
                            );
                        }
                        Some(Err(error)) => {
                            let message = format!("stream-json input rejected: {error}");
                            eprintln!("{message}");
                            unattended_interaction_error = Some(message);
                            control.requested.cancel();
                            input_stream = None;
                            // The client's input is gone: nobody can answer an open approval.
                            for call_id in open_approvals.drain() {
                                let _ = host
                                    .resolve_approval_body(
                                        &session_id,
                                        detached_denial_body(&call_id),
                                    )
                                    .await;
                            }
                        }
                        None => {
                            input_stream = None;
                            // Input ended: deny every open approval so the turn never waits.
                            for call_id in open_approvals.drain() {
                                let _ = host
                                    .resolve_approval_body(
                                        &session_id,
                                        detached_denial_body(&call_id),
                                    )
                                    .await;
                            }
                        }
                    }
                    continue;
                }
                event = stream.next_event() => event,
            };
            match next {
                Ok(Some(event)) => {
                    reconnects = 0;
                    if !is_live_projection(&event) {
                        let durable_seq = daemon_event_seq(&event);
                        if durable_seq > 0 && durable_seq <= last_durable_seq {
                            continue;
                        }
                        last_durable_seq = durable_seq;
                    }
                    let is_resync = daemon_event_kind(&event) == Some("streamResyncRequired");
                    // A resync frame names a gap. Its sequence is synthetic and
                    // must never advance the durable cursor before catch-up.
                    if !is_resync {
                        after = after.max(daemon_event_seq(&event));
                    }
                    control.push_live_projection(&event);
                    process_daemon_event_with_mode(
                        &event,
                        &session_id,
                        output_mode,
                        loaded_schema.as_ref(),
                        args.color,
                        &mut output_state,
                        &mut first_error,
                    );
                    if let Some(message) = persistence_failed_message(&event) {
                        return run_error_exit(output_mode, RunExitCode::Unknown, &message);
                    }
                    if args.approval_callback {
                        match daemon_event_kind(&event) {
                            Some("toolApprovalRequested") => {
                                if let Some(call_id) = approval_call_id(&event) {
                                    open_approvals.insert(call_id);
                                }
                            }
                            Some("toolApprovalResolved") => {
                                if let Some(call_id) = approval_call_id(&event) {
                                    open_approvals.remove(&call_id);
                                }
                            }
                            _ => {}
                        }
                    }
                    if !matches!(output_mode, RunOutputMode::Silent) {
                        match apply_unattended_policy(
                            &host,
                            &session_id,
                            &event,
                            !args.approval_callback,
                        )
                        .await
                        {
                            UnattendedInteraction::ApprovalDenied => denied_unattended = true,
                            UnattendedInteraction::PlanEndsRun => {
                                unattended_interaction_error = Some(
                                    "headless run cannot approve a plan; the plan is the result and nothing in it was executed"
                                        .to_string(),
                                );
                                unattended_interaction_exit_code =
                                    Some(RunExitCode::InteractionRequired);
                                if !interrupt_sent {
                                    if let Err(error) = host.interrupt(&session_id).await {
                                        return run_error_exit(
                                            output_mode,
                                            RunExitCode::Generic,
                                            &error,
                                        );
                                    }
                                    interrupt_sent = true;
                                }
                            }
                            UnattendedInteraction::QuestionDeclined
                            | UnattendedInteraction::Continue => {}
                        }
                    }
                    if !daemon_event_is_terminal(&event) {
                        continue;
                    }
                    if !is_resync {
                        if let Err(error) = host.wait_for_quiescence(&session_id).await {
                            return run_error_exit(output_mode, RunExitCode::Generic, &error);
                        }
                        retry_schema_answer!(continue);
                        if let Some(path) = args.output_last_message.as_deref() {
                            if let Err(error) = write_last_message(path, &output_state) {
                                return run_error_exit(output_mode, RunExitCode::Generic, &error);
                            }
                        }
                        if (input_stream.is_some() || !pending_input_messages.is_empty())
                            && unattended_interaction_error.is_none()
                        {
                            let queued = pending_input_messages.pop_front();
                            let next_input = if queued.is_some() {
                                queued.map(|text| {
                                    Ok((
                                        0,
                                        RunInputEvent::UserMessage {
                                            text,
                                            nonce: None,
                                            trust: None,
                                        },
                                    ))
                                })
                            } else {
                                match input_stream.as_mut() {
                                    Some(input) => input.recv().await,
                                    None => None,
                                }
                            };
                            match next_input {
                                Some(Ok((line, RunInputEvent::UserMessage { text, .. }))) => {
                                    if text.trim().is_empty() {
                                        eprintln!("stream-json input line {line}: empty user_message ignored");
                                        continue;
                                    }
                                    match host
                                        .submit_message(&session_id, &text, &workspace_root)
                                        .await
                                    {
                                        Ok(sequence) => {
                                            after = sequence;
                                            last_durable_seq = sequence;
                                            stream = match host
                                                .open_event_stream(&session_id, after, human)
                                                .await
                                            {
                                                Ok(stream) => stream,
                                                Err(error) => {
                                                    return run_error_exit(
                                                        output_mode,
                                                        RunExitCode::Generic,
                                                        &error,
                                                    )
                                                }
                                            };
                                            first_error = None;
                                            output_state = RunOutputState::default();
                                            reconnects = 0;
                                            interrupt_sent = false;
                                            continue;
                                        }
                                        Err(error) => {
                                            return run_error_exit(
                                                output_mode,
                                                RunExitCode::Generic,
                                                &error,
                                            )
                                        }
                                    }
                                }
                                Some(Ok((
                                    line,
                                    RunInputEvent::Control {
                                        action: ControlAction::Abort,
                                        reason,
                                    },
                                ))) => {
                                    eprintln!(
                                        "stream-json input line {line}: abort{}",
                                        reason
                                            .map(|value| format!(" ({value})"))
                                            .unwrap_or_default()
                                    );
                                    control.settled();
                                    return interrupted_exit(output_mode);
                                }
                                Some(Ok((line, event))) => {
                                    eprintln!(
                                        "stream-json input line {line}: {:?} ignored between turns",
                                        input_event_kind(&event)
                                    );
                                    continue;
                                }
                                Some(Err(error)) => {
                                    return run_error_exit(
                                        output_mode,
                                        RunExitCode::SchemaViolation,
                                        &error.to_string(),
                                    )
                                }
                                None => drop(input_stream.take()),
                            }
                        }
                        if args.ephemeral {
                            if let Err(error) =
                                host.session_action("delete", &session_id, None).await
                            {
                                return run_error_exit(output_mode, RunExitCode::Generic, &error);
                            }
                        }
                        control.settled();
                        if interrupt_sent {
                            if let Some(message) = unattended_interaction_error.take() {
                                return run_error_exit(
                                    output_mode,
                                    unattended_interaction_exit_code
                                        .unwrap_or(RunExitCode::Generic),
                                    &message,
                                );
                            }
                            return interrupted_exit(output_mode);
                        }
                        let final_code = unattended_exit_code(
                            final_run_exit_code(
                                output_state.output_error.is_none(),
                                exit_on_error,
                                first_error,
                                output_state
                                    .output_error
                                    .or_else(|| daemon_event_run_exit_code(&event)),
                                RunExitCode::Generic,
                            ),
                            denied_unattended,
                        );
                        return exit_code_with_failure_object(
                            output_mode,
                            &output_state,
                            final_code,
                        );
                    }

                    match host.events_after(&session_id, after).await {
                        Ok(history) => {
                            for hist_event in history {
                                if !is_live_projection(&hist_event) {
                                    let durable_seq = daemon_event_seq(&hist_event);
                                    if durable_seq > 0 && durable_seq <= last_durable_seq {
                                        continue;
                                    }
                                    last_durable_seq = durable_seq;
                                }
                                let hist_is_resync =
                                    daemon_event_kind(&hist_event) == Some("streamResyncRequired");
                                if !hist_is_resync {
                                    after = after.max(daemon_event_seq(&hist_event));
                                }
                                process_daemon_event_with_mode(
                                    &hist_event,
                                    &session_id,
                                    output_mode,
                                    loaded_schema.as_ref(),
                                    args.color,
                                    &mut output_state,
                                    &mut first_error,
                                );
                                if let Some(message) = persistence_failed_message(&hist_event) {
                                    return run_error_exit(
                                        output_mode,
                                        RunExitCode::Unknown,
                                        &message,
                                    );
                                }
                                if args.approval_callback {
                                    match daemon_event_kind(&hist_event) {
                                        Some("toolApprovalRequested") => {
                                            if let Some(call_id) = approval_call_id(&hist_event) {
                                                open_approvals.insert(call_id);
                                            }
                                        }
                                        Some("toolApprovalResolved") => {
                                            if let Some(call_id) = approval_call_id(&hist_event) {
                                                open_approvals.remove(&call_id);
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                if !matches!(output_mode, RunOutputMode::Silent) {
                                    match apply_unattended_policy(
                                        &host,
                                        &session_id,
                                        &hist_event,
                                        !args.approval_callback,
                                    )
                                    .await
                                    {
                                        UnattendedInteraction::ApprovalDenied => {
                                            denied_unattended = true
                                        }
                                        UnattendedInteraction::PlanEndsRun => {
                                            unattended_interaction_error = Some(
                                                "headless run cannot approve a plan; the plan is the result and nothing in it was executed"
                                                    .to_string(),
                                            );
                                            unattended_interaction_exit_code =
                                                Some(RunExitCode::InteractionRequired);
                                            if !interrupt_sent {
                                                if let Err(error) =
                                                    host.interrupt(&session_id).await
                                                {
                                                    return run_error_exit(
                                                        output_mode,
                                                        RunExitCode::Generic,
                                                        &error,
                                                    );
                                                }
                                                interrupt_sent = true;
                                            }
                                        }
                                        UnattendedInteraction::QuestionDeclined
                                        | UnattendedInteraction::Continue => {}
                                    }
                                }
                                if daemon_event_is_terminal(&hist_event) && !hist_is_resync {
                                    if let Err(error) = host.wait_for_quiescence(&session_id).await
                                    {
                                        return run_error_exit(
                                            output_mode,
                                            RunExitCode::Generic,
                                            &error,
                                        );
                                    }
                                    retry_schema_answer!(continue 'run);
                                    if let Some(path) = args.output_last_message.as_deref() {
                                        if let Err(error) = write_last_message(path, &output_state)
                                        {
                                            return run_error_exit(
                                                output_mode,
                                                RunExitCode::Generic,
                                                &error,
                                            );
                                        }
                                    }
                                    if (input_stream.is_some()
                                        || !pending_input_messages.is_empty())
                                        && unattended_interaction_error.is_none()
                                    {
                                        let queued = pending_input_messages.pop_front();
                                        let next_input = if queued.is_some() {
                                            queued.map(|text| {
                                                Ok((
                                                    0,
                                                    RunInputEvent::UserMessage {
                                                        text,
                                                        nonce: None,
                                                        trust: None,
                                                    },
                                                ))
                                            })
                                        } else {
                                            match input_stream.as_mut() {
                                                Some(input) => input.recv().await,
                                                None => None,
                                            }
                                        };
                                        match next_input {
                                            Some(Ok((
                                                line,
                                                RunInputEvent::UserMessage { text, .. },
                                            ))) => {
                                                if text.trim().is_empty() {
                                                    eprintln!("stream-json input line {line}: empty user_message ignored");
                                                    continue 'run;
                                                }
                                                match host
                                                    .submit_message(
                                                        &session_id,
                                                        &text,
                                                        &workspace_root,
                                                    )
                                                    .await
                                                {
                                                    Ok(sequence) => {
                                                        after = sequence;
                                                        last_durable_seq = sequence;
                                                        stream = match host
                                                            .open_event_stream(
                                                                &session_id,
                                                                after,
                                                                human,
                                                            )
                                                            .await
                                                        {
                                                            Ok(stream) => stream,
                                                            Err(error) => {
                                                                return run_error_exit(
                                                                    output_mode,
                                                                    RunExitCode::Generic,
                                                                    &error,
                                                                )
                                                            }
                                                        };
                                                        first_error = None;
                                                        output_state = RunOutputState::default();
                                                        reconnects = 0;
                                                        interrupt_sent = false;
                                                        continue 'run;
                                                    }
                                                    Err(error) => {
                                                        return run_error_exit(
                                                            output_mode,
                                                            RunExitCode::Generic,
                                                            &error,
                                                        );
                                                    }
                                                }
                                            }
                                            Some(Ok((
                                                line,
                                                RunInputEvent::Control {
                                                    action: ControlAction::Abort,
                                                    reason,
                                                },
                                            ))) => {
                                                eprintln!(
                                                    "stream-json input line {line}: abort{}",
                                                    reason
                                                        .map(|value| format!(" ({value})"))
                                                        .unwrap_or_default()
                                                );
                                                control.settled();
                                                return interrupted_exit(output_mode);
                                            }
                                            Some(Ok((line, event))) => {
                                                eprintln!(
                                                    "stream-json input line {line}: {:?} ignored between turns",
                                                    input_event_kind(&event)
                                                );
                                                continue 'run;
                                            }
                                            Some(Err(error)) => {
                                                return run_error_exit(
                                                    output_mode,
                                                    RunExitCode::SchemaViolation,
                                                    &error.to_string(),
                                                );
                                            }
                                            None => drop(input_stream.take()),
                                        }
                                    }
                                    if args.ephemeral {
                                        if let Err(error) =
                                            host.session_action("delete", &session_id, None).await
                                        {
                                            return run_error_exit(
                                                output_mode,
                                                RunExitCode::Generic,
                                                &error,
                                            );
                                        }
                                    }
                                    control.settled();
                                    if interrupt_sent {
                                        if let Some(message) = unattended_interaction_error.take() {
                                            return run_error_exit(
                                                output_mode,
                                                unattended_interaction_exit_code
                                                    .unwrap_or(RunExitCode::Generic),
                                                &message,
                                            );
                                        }
                                        return interrupted_exit(output_mode);
                                    }
                                    let final_code = unattended_exit_code(
                                        final_run_exit_code(
                                            output_state.output_error.is_none(),
                                            exit_on_error,
                                            first_error,
                                            output_state.output_error.or_else(|| {
                                                daemon_event_run_exit_code(&hist_event)
                                            }),
                                            RunExitCode::Generic,
                                        ),
                                        denied_unattended,
                                    );
                                    return exit_code_with_failure_object(
                                        output_mode,
                                        &output_state,
                                        final_code,
                                    );
                                }
                            }
                        }
                        Err(error) => {
                            return run_error_exit(output_mode, RunExitCode::Generic, &error);
                        }
                    }
                    match host.open_event_stream(&session_id, after, human).await {
                        Ok(new_stream) => stream = new_stream,
                        Err(error) => {
                            return run_error_exit(output_mode, RunExitCode::Generic, &error);
                        }
                    }
                }
                Ok(None) if reconnects < 2 => {
                    reconnects += 1;
                    if !matches!(output_mode, RunOutputMode::Silent) {
                        eprintln!(
                            "[{host_label}] reconnect {reconnects}/2 after event stream closed"
                        );
                    }
                    match host.open_event_stream(&session_id, after, human).await {
                        Ok(new_stream) => stream = new_stream,
                        Err(error) => {
                            return run_error_exit(output_mode, RunExitCode::Generic, &error);
                        }
                    }
                }
                Ok(None) => {
                    let message = format!("{host_label} event stream closed unexpectedly");
                    return run_error_exit(output_mode, RunExitCode::Generic, &message);
                }
                Err(error) if reconnects < 2 => {
                    reconnects += 1;
                    if !matches!(output_mode, RunOutputMode::Silent) {
                        eprintln!(
                            "[{host_label}] reconnect {reconnects}/2 after event read failure: {error}"
                        );
                    }
                    match host.open_event_stream(&session_id, after, human).await {
                        Ok(new_stream) => stream = new_stream,
                        Err(error) => {
                            return run_error_exit(output_mode, RunExitCode::Generic, &error);
                        }
                    }
                }
                Err(error) => {
                    return run_error_exit(output_mode, RunExitCode::Generic, &error);
                }
            }
        }
    }

    /// Reject run flags the selected vendor CLI has no equivalent for.
    ///
    /// Claude Code (<https://code.claude.com/docs/en/cli-reference>) has `--max-turns`,
    /// `--max-budget-usd`, `--system-prompt[-file]`, `--append-system-prompt[-file]`,
    /// `--mcp-config` and `--add-dir`; these are forwarded by
    /// `rightkit_agents::driver_host::ClaudeRunFlags`. Antigravity (`agy --help`) only has
    /// `--add-dir`; it has no turn/budget/system-prompt flags and registers MCP servers through
    /// `agy mcp add`, not per run.
    fn validate_vendor_cli_options(
        args: &RunArgs,
        kind: rightkit_agents::driver_host::vendor_cli::VendorCli,
    ) -> Result<(), String> {
        use rightkit_agents::driver_host::vendor_cli::VendorCli;
        let mut unsupported = Vec::new();
        if !args.image.is_empty() {
            unsupported.push("--image");
        }
        if args.ephemeral {
            unsupported.push("--ephemeral/--no-session");
        }
        if args.output_last_message.is_some() {
            unsupported.push("--output-last-message");
        }
        if args.no_mcp {
            unsupported.push("--no-mcp");
        }
        if !args.tools.is_empty() {
            unsupported.push("--tools");
        }
        if !args.exclude_tools.is_empty() {
            unsupported.push("--exclude-tools");
        }
        if kind == VendorCli::Antigravity {
            if args.max_turns.is_some() {
                unsupported.push("--max-turns");
            }
            if args.max_budget_usd.is_some() {
                unsupported.push("--max-budget-usd");
            }
            if args.system_prompt.is_some() || args.system_prompt_file.is_some() {
                unsupported.push("--system-prompt/--system-prompt-file");
            }
            if args.append_system_prompt.is_some() || args.append_system_prompt_file.is_some() {
                unsupported.push("--append-system-prompt/--append-system-prompt-file");
            }
            if args.mcp_config.is_some() {
                unsupported.push("--mcp-config");
            }
        }
        if matches!(args.input_format, InputFormat::StreamJson) {
            unsupported.push("--input-format stream-json");
        }
        if unsupported.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "{} route does not support {}",
                kind.display_name(),
                unsupported.join(", ")
            ))
        }
    }
}

// Preserve the root names consumed by main_runtime_entry.rs. There is one
// implementation; these are visibility imports, not duplicate logic.
use hosted_run_session::{run_error_exit, select_run_output_mode, RunOutputMode};

/// A `persistenceFailed` event ends the run (headless-cli.md section 2.5): the session's storage
/// latched, the engine stopped the turn and nothing durable follows, so no `sessionTurnFinished`
/// will ever arrive. What the turn did before the failure (a tool may have run) is not recorded,
/// so the outcome is exit 3 `unknown`, not retryable: check the workspace before running again.
fn persistence_failed_message(event: &serde_json::Value) -> Option<String> {
    if daemon_event_kind(event) != Some("persistenceFailed") {
        return None;
    }
    let reason = event
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .unwrap_or("the event store refused a write");
    Some(format!(
        "the session could not be saved ({reason}); the turn was stopped and what it did before the failure is not recorded, so check the workspace before running it again"
    ))
}

/// Shared CLI/TUI execution entry. Host selection is deliberately beneath
/// presentation so both surfaces acquire InlineHost by default.
async fn run_session(args: RunArgs) -> i32 {
    run_session_controlled(args, RunCancellation::new()).await
}

async fn run_session_controlled(mut args: RunArgs, control: Arc<RunCancellation>) -> i32 {
    let _completion = RunCompletionGuard(control.clone());
    args.allowed_tools.append(&mut args.tools);
    args.disallowed_tools.append(&mut args.exclude_tools);
    // Expand pi-style include/exclude globs before both InlineHost and
    // ServiceHost see the same canonical tool filter.
    match resolve_cli_tool_filters_with_mcp(
        args.toolset.as_deref(),
        std::mem::take(&mut args.allowed_tools),
        std::mem::take(&mut args.disallowed_tools),
        args.no_mcp,
    ) {
        Ok((toolset, allowed, disallowed)) => {
            args.toolset = toolset;
            args.allowed_tools = allowed;
            args.disallowed_tools = disallowed;
        }
        Err(message) => {
            return run_error_exit(
                select_run_output_mode(&args, None),
                RunExitCode::Usage,
                &message,
            );
        }
    }
    // The route is chosen at host selection (R13); an in-process run needs
    // the local resource profile, and a daemon-routed run is not hurt by it.
    let profile =
        match rightkit_agent_coding_tools::process_resource_authority::local_resource_profile_from_env() {
            Ok(profile) => profile,
            Err(error) => {
                return run_error_exit(
                    select_run_output_mode(&args, None),
                    RunExitCode::Usage,
                    &error,
                );
            }
        };
    if let Err(existing) =
        rightkit_agent_coding_tools::process_resource_authority::install_process_resource_profile(
            profile,
        )
    {
        let message = format!(
            "InlineHost requested resource profile {profile:?}, but this process is already bound to {existing:?}"
        );
        return run_error_exit(
            select_run_output_mode(&args, None),
            RunExitCode::Usage,
            &message,
        );
    }

    let code = hosted_run_session::run_hosted(args, control).await;
    // A `SessionEnd` hook runs fire-and-forget at the end of the turn; the process exits right
    // after this, and every child dies with it (issue 448), so wait for those hooks (bounded).
    // The host can report the turn finished a moment before the engine records the session end,
    // so first wait (briefly) for that record, then for the hook tasks it started.
    rightkit_hooks::agent_hooks::wait_session_ends(std::time::Duration::from_secs(3)).await;
    rightkit_hooks::agent_hooks::wait_pending(std::time::Duration::from_secs(10)).await;
    code
}
