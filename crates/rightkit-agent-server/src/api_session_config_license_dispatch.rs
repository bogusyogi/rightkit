use super::*;

/// The fixed Pro feature set a daemon starts with.
///
/// Release builds start with none: Pro comes only from the daemon's own verification of the stored
/// licence record (`crate::license::LicenseGate`, attached at boot). No environment variable, flag
/// or request can grant a feature. A `test-mode` build (QA and the e2e rigs) and unit tests are the
/// Pro product so Pro-gated routes are reachable without a signed licence; that set is compiled in,
/// not read from anywhere, and release builds cannot enable the feature.
pub(crate) fn initial_license_features() -> HashSet<String> {
    #[cfg(any(test, feature = "test-mode"))]
    {
        [
            "routing",
            "brews",
            "evolution",
            "execution_backends",
            "schedules",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    #[cfg(not(any(test, feature = "test-mode")))]
    {
        HashSet::new()
    }
}

pub(crate) fn require_license_feature(
    state: &DaemonState,
    feature: &str,
) -> Result<(), StatusCode> {
    if state.has_license_feature(feature) {
        Ok(())
    } else {
        Err(StatusCode::PAYMENT_REQUIRED)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionEntryOutcome {
    Direct,
    Conduct,
}

pub(crate) async fn run_dispatched_session_entry<Emit, Direct, DirectFut, Conduct, ConductFut>(
    auto_routing_enabled: bool,
    task: &str,
    classifier: Option<&dyn DispatchClassifier>,
    mut emit_decision: Emit,
    direct: Direct,
    conduct: Conduct,
) -> Result<SessionEntryOutcome, String>
where
    Emit: FnMut(&DispatchDecision),
    Direct: FnOnce() -> DirectFut,
    DirectFut: std::future::Future<Output = Result<(), String>>,
    Conduct: FnOnce() -> ConductFut,
    ConductFut: std::future::Future<Output = Result<(), String>>,
{
    let decision = if auto_routing_enabled {
        DispatchRouter::new().decide(task, classifier).await
    } else {
        DispatchDecision::direct(
            1.0,
            "Automatic dispatch routing requires the `routing` Pro feature; running direct.",
            JudgeSource::Heuristic,
        )
    };

    emit_decision(&decision);

    match decision.strategy {
        ExecutionStrategy::Direct => {
            direct().await?;
            Ok(SessionEntryOutcome::Direct)
        }
        ExecutionStrategy::Conduct { .. } => {
            conduct().await?;
            Ok(SessionEntryOutcome::Conduct)
        }
    }
}

pub(crate) struct LlmDispatchClassifier {
    pub(crate) llm: Arc<dyn rightkit_agent_engine_core::llm::LlmClient>,
}

#[async_trait::async_trait]
impl DispatchClassifier for LlmDispatchClassifier {
    async fn classify(&self, task: &str) -> Result<ClassifierOutcome, String> {
        let messages = vec![
            TranscriptMessage::System {
                id: "dispatch-classifier-system".into(),
                created_at: "dispatch".into(),
                content: concat!(
                    "Classify whether a coding task should run direct or be decomposed by a ",
                    "conductor. Output only JSON: {\"complexity\":\"simple|complex\",",
                    "\"reason\":\"short rationale\"}. simple means a single agent turn should ",
                    "handle it; complex means multi-step, repo-wide, or decomposition-worthy."
                )
                .into(),
                pinned: true,
            },
            TranscriptMessage::User {
                id: "dispatch-classifier-user".into(),
                created_at: "dispatch".into(),
                content: task.to_string(),
                trust: None,
                pinned: false,
                attachments: Vec::new(),
            },
        ];
        let response = self.llm.complete(&messages).await?;
        parse_dispatch_classifier_response(&response.text)
    }
}

fn parse_dispatch_classifier_response(raw: &str) -> Result<ClassifierOutcome, String> {
    let json_text = extract_json_object(raw)
        .ok_or_else(|| "classifier response did not contain a JSON object".to_string())?;
    let value: serde_json::Value = serde_json::from_str(json_text)
        .map_err(|err| format!("classifier response was not valid JSON: {err}"))?;
    let complexity = match value
        .get("complexity")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "simple" => ClassifierComplexity::Simple,
        "complex" => ClassifierComplexity::Complex,
        other => {
            return Err(format!(
                "classifier complexity must be simple or complex, got `{other}`"
            ));
        }
    };
    let reason = value
        .get("reason")
        .and_then(|value| value.as_str())
        .unwrap_or("Classifier did not provide a rationale.")
        .trim()
        .to_string();
    Ok(ClassifierOutcome { complexity, reason })
}

fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    (end >= start).then_some(&raw[start..=end])
}

// ── Request / response shapes ─────────────────────────────────────────────────
