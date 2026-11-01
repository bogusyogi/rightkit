async fn cli_skill(command: SkillCommand) -> i32 {
    match command {
        SkillCommand::List { json: _json } => daemon_get_pretty_json("/skills", "skills").await,
        SkillCommand::Proposals => {
            daemon_get_pretty_json("/skills/proposals", "skill proposals").await
        }
        SkillCommand::Propose {
            slug,
            summary,
            content_file,
            id,
            title,
            source_session_id,
            source_task_id,
        } => {
            let content = match std::fs::read_to_string(&content_file) {
                Ok(content) => content,
                Err(e) => {
                    eprintln!("Failed to read skill proposal content file {content_file}: {e}");
                    return 1;
                }
            };
            let body = serde_json::json!({
                "id": id,
                "slug": slug,
                "title": title,
                "summary": summary,
                "content": content,
                "sourceSessionId": source_session_id,
                "sourceTaskId": source_task_id,
            });
            daemon_post_pretty_json("/skills/proposals", body, "skill proposal create").await
        }
        SkillCommand::QuarantineProposal {
            proposal_id,
            reason,
        } => {
            let body = serde_json::json!({ "reason": reason });
            daemon_post_pretty_json(
                &format!(
                    "/skills/proposals/{}/quarantine",
                    url_encode_component(&proposal_id)
                ),
                body,
                "skill proposal quarantine",
            )
            .await
        }
        SkillCommand::BenchmarkProposal {
            proposal_id,
            benchmark_id,
            gate_mode,
            score,
            passed,
            failed,
            slug,
            reason,
            verdict_file,
        } => {
            if !passed && !failed {
                eprintln!("Pass either --passed or --failed so the benchmark verdict is explicit.");
                return 1;
            }
            let verdict = match verdict_file {
                Some(path) => match read_json_file(&path) {
                    Ok(value) => Some(value),
                    Err(message) => {
                        eprintln!("{message}");
                        return 1;
                    }
                },
                None => None,
            };
            let body = serde_json::json!({
                "slug": slug,
                "benchmarkId": benchmark_id,
                "gateMode": gate_mode,
                "scoreContinuous": score,
                "passed": passed && !failed,
                "verdict": verdict,
                "reason": reason,
            });
            daemon_post_pretty_json(
                &format!(
                    "/skills/proposals/{}/benchmark",
                    url_encode_component(&proposal_id)
                ),
                body,
                "skill proposal benchmark",
            )
            .await
        }
        SkillCommand::RunBenchmarkProposal {
            proposal_id,
            kind,
            session_id,
            role,
            model,
            goal,
            repeats,
            task_id,
            agent,
            benchmark_id,
            gate_mode,
            threshold,
            reason,
        } => {
            let body = serde_json::json!({
                "kind": kind,
                "sessionId": session_id,
                "role": role,
                "model": model,
                "goal": goal,
                "repeats": repeats,
                "taskId": task_id,
                "agent": agent,
                "benchmarkId": benchmark_id,
                "gateMode": gate_mode,
                "threshold": threshold,
                "reason": reason,
            });
            daemon_post_pretty_json(
                &format!(
                    "/skills/proposals/{}/run-benchmark",
                    url_encode_component(&proposal_id)
                ),
                body,
                "skill proposal benchmark runner",
            )
            .await
        }
        SkillCommand::Quarantine { slug, reason } => {
            skill_lifecycle_action(&slug, "quarantine", reason, false).await
        }
        SkillCommand::Activate {
            slug,
            reason,
            operator_override,
        } => skill_lifecycle_action(&slug, "activate", reason, operator_override).await,
        SkillCommand::Retire { slug, reason } => {
            skill_lifecycle_action(&slug, "retire", reason, false).await
        }
        SkillCommand::TrainingRuns => {
            daemon_get_pretty_json("/runtime/skillopt/runs", "skill training runs").await
        }
        SkillCommand::TrainingRun { run_id } => {
            daemon_get_pretty_json(
                &format!("/runtime/skillopt/runs/{}", url_encode_component(&run_id)),
                "skill training run",
            )
            .await
        }
        SkillCommand::ResumeTrainingRun {
            run_id,
            checkpoint_file,
            message,
        } => {
            let checkpoint = match checkpoint_file {
                Some(path) => match read_json_file(&path) {
                    Ok(value) => Some(value),
                    Err(message) => {
                        eprintln!("{message}");
                        return 1;
                    }
                },
                None => None,
            };
            let body = serde_json::json!({
                "checkpoint": checkpoint,
                "message": message,
            });
            daemon_post_pretty_json(
                &format!(
                    "/runtime/skillopt/runs/{}/resume",
                    url_encode_component(&run_id)
                ),
                body,
                "skill training run resume",
            )
            .await
        }
        SkillCommand::ProposeTrainingCandidate {
            run_id,
            operation,
            summary,
            instructions,
        } => {
            let body = serde_json::json!({
                "operation": operation,
                "summary": summary,
                "instructions": instructions,
            });
            daemon_post_pretty_json(
                &format!(
                    "/runtime/skillopt/runs/{}/propose-candidate",
                    url_encode_component(&run_id)
                ),
                body,
                "skill training candidate propose",
            )
            .await
        }
        SkillCommand::ExecuteTrainingRun {
            run_id,
            proposed_body_file,
            validation_score,
            operation,
            summary,
            gate_mode,
            threshold,
        } => {
            let proposed_body = match std::fs::read_to_string(&proposed_body_file) {
                Ok(content) => content,
                Err(e) => {
                    eprintln!(
                        "Failed to read skill training candidate body file {proposed_body_file}: {e}"
                    );
                    return 1;
                }
            };
            let body = serde_json::json!({
                "proposedBody": proposed_body,
                "validationScore": validation_score,
                "operation": operation,
                "summary": summary,
                "gateMode": gate_mode,
                "threshold": threshold,
            });
            daemon_post_pretty_json(
                &format!(
                    "/runtime/skillopt/runs/{}/execute",
                    url_encode_component(&run_id)
                ),
                body,
                "skill training run execute",
            )
            .await
        }
        SkillCommand::CancelTrainingRun { run_id, reason } => {
            let body = serde_json::json!({ "reason": reason });
            daemon_post_pretty_json(
                &format!(
                    "/runtime/skillopt/runs/{}/cancel",
                    url_encode_component(&run_id)
                ),
                body,
                "skill training run cancel",
            )
            .await
        }
        SkillCommand::TrainingRunHistory { run_id } => {
            daemon_get_pretty_json(
                &format!(
                    "/runtime/skillopt/runs/{}/history",
                    url_encode_component(&run_id)
                ),
                "skill training run history",
            )
            .await
        }
        SkillCommand::TrainingRunRejectedEdits { run_id } => {
            daemon_get_pretty_json(
                &format!(
                    "/runtime/skillopt/runs/{}/rejected-edits",
                    url_encode_component(&run_id)
                ),
                "skill training run rejected edits",
            )
            .await
        }
        SkillCommand::StartTrainingRun {
            name,
            skill_id,
            objective,
            optimizer,
            target_model,
            gates,
            rejected_edits,
        } => {
            let gates = match parse_skillopt_gates(&gates) {
                Ok(gates) => gates,
                Err(message) => {
                    eprintln!("{message}");
                    return 1;
                }
            };
            let rejected_edits = match parse_rejected_edits(&rejected_edits) {
                Ok(edits) => edits,
                Err(message) => {
                    eprintln!("{message}");
                    return 1;
                }
            };
            let body = serde_json::json!({
                "name": name,
                "target": {
                    "skillId": skill_id,
                    "objective": objective,
                    "optimizer": optimizer,
                    "targetModel": target_model,
                },
                "rejectedEdits": rejected_edits,
                "gates": gates,
            });
            daemon_post_pretty_json("/runtime/skillopt/runs", body, "skill training run create")
                .await
        }
    }
}
