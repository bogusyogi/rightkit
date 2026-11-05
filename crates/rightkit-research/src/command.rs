//! The `research` command: a search over host-opened source records or one pure research step
//! (`ledger-check`, `ledger-render`, `citecheck`, `meter`, `retraction`, `issue-patch`,
//! `apply-patch`), returning JSON.
//!
//! The app maps its argv onto [`ResearchOptions`] and passes a [`Cancellation`] linked to its
//! own signal handling (`Cancellation::from_probe`); nothing here reads the command line, spawns
//! a search provider or touches the network unless a retraction step is asked to.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::research_port::{citecheck, ledger, meter, patch_guard, patcher, retraction};
use crate::Cancellation;

/// A command failure with the exit code the app should use.
#[derive(Debug)]
pub struct CommandError {
    /// Exit code: 1 policy, 2 incomplete, 3 internal, 4 usage, 5 integrity.
    pub code: i32,
    /// What went wrong.
    pub message: String,
}

impl CommandError {
    /// Bad input (exit 4).
    pub fn usage(message: impl Into<String>) -> Self {
        Self {
            code: 4,
            message: message.into(),
        }
    }

    /// The command could not finish (exit 2).
    pub fn incomplete(message: impl Into<String>) -> Self {
        Self {
            code: 2,
            message: message.into(),
        }
    }

    /// An I/O or serialisation failure (exit 3).
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: 3,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CommandError {}

/// A command result: JSON, or the failure.
pub type CommandResult = Result<Value, CommandError>;

fn io_error(error: impl std::fmt::Display) -> CommandError {
    CommandError::internal(error.to_string())
}

/// Everything the `research` command takes. The app maps its argv onto this; [`Default`] holds
/// the command-line defaults.
#[derive(Debug, Clone)]
pub struct ResearchOptions {
    /// Route query for the host-injected search. Not required with `step`.
    pub query: Option<String>,
    /// Complete ResearchRoute v2 JSON file; its subject.query must equal --query.
    pub route_json: Option<PathBuf>,
    /// Run one Research step instead of a search: `ledger-check`,
    /// `ledger-render`, `citecheck`, `meter`, `retraction`, `issue-patch`, or `apply-patch`.
    pub step: Option<String>,
    /// Evidence ledger rows (JSONL) for `ledger-check` and `ledger-render`,
    /// and for `citecheck`.
    pub evidence: Option<PathBuf>,
    /// Claim ledger rows (JSONL) for `ledger-check` and `ledger-render`.
    pub claims: Option<PathBuf>,
    /// Markdown draft to bind citations for `citecheck`.
    pub draft: Option<PathBuf>,
    /// Run identifier bound into an issue-patch/apply-patch receipt.
    pub run_id: Option<String>,
    /// Existing raw HMAC key file (at least 32 bytes) for patch receipts.
    pub key_file: Option<PathBuf>,
    /// Authenticated patch receipt JSON for `apply-patch`.
    pub receipt: Option<PathBuf>,
    /// Single-file unified diff for `apply-patch`.
    pub diff: Option<PathBuf>,
    /// Output path for `ledger-render`.
    pub out: Option<PathBuf>,
    /// Brief title for `ledger-render`.
    pub title: String,
    /// DOI list (one per line) for `retraction`.
    pub dois: Option<PathBuf>,
    /// DOIs already disclosed as retracted in the draft, one per line.
    pub disclosed: Option<PathBuf>,
    /// Let unknown retraction status pass instead of blocking.
    pub allow_unknown: bool,
    /// JSON map of DOI to pre-fetched retraction metadata; selecting this keeps the step offline.
    pub retraction_fixture: Option<PathBuf>,
    /// Do not use public metadata endpoints for missing retraction fixtures.
    pub offline: bool,
    /// JSON budget/usage state for `meter`. The updated state is reported, not written.
    pub meter_state: Option<PathBuf>,
    /// Metered effect for `meter`: `external_request` or `worker`.
    pub effect: Option<String>,
    /// Units consumed by one `meter` step.
    pub units: u64,
    /// Worker lifecycle for `meter --effect worker`: `start` or `finish`.
    pub worker_event: Option<String>,
    /// Providers to query (comma separated on the command line).
    pub provider: Vec<String>,
    /// Most hits per provider.
    pub max_hits: u32,
    /// Host-opened source record files.
    pub source_records: Vec<PathBuf>,
    /// Independent providers required for a proven result.
    pub min_independent_providers: usize,
    /// Let records without a usable URL count toward `--min-independent-providers` using their
    /// self-declared `provider` label. Off by default: independence otherwise rests on source
    /// identity (registrable domain), never on a label the host controls.
    pub allow_self_declared_independence: bool,
    /// Convenience route domain (default: general); conflicts with --route-json are refused.
    pub domain: Option<String>,
    /// Convenience route sensitivity (default: public); conflicts with --route-json are refused.
    pub sensitivity: Option<String>,
    /// Result schema id prefix: `kind` fields read `<kind>` and `<kind>-step`. Apps keep the id
    /// their stored results already carry.
    pub kind: String,
}

impl Default for ResearchOptions {
    fn default() -> Self {
        Self {
            query: None,
            route_json: None,
            step: None,
            evidence: None,
            claims: None,
            draft: None,
            run_id: None,
            key_file: None,
            receipt: None,
            diff: None,
            out: None,
            title: "Research brief".into(),
            dois: None,
            disclosed: None,
            allow_unknown: false,
            retraction_fixture: None,
            offline: false,
            meter_state: None,
            effect: None,
            units: 1,
            worker_event: None,
            provider: Vec::new(),
            max_hits: 5,
            source_records: Vec::new(),
            min_independent_providers: 2,
            allow_self_declared_independence: false,
            domain: None,
            sensitivity: None,
            kind: "rightkit-research".into(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn terminal_unproven(
    query: String,
    failures: Vec<crate::SourceFailure>,
    available_source_records: usize,
    independent_provider_count: usize,
    required_independent_providers: usize,
    independence: Value,
    external_requests: usize,
    route: &crate::ResearchRoute,
    authorization: &crate::ResearchAuthorization,
    selected_provider_denominator: usize,
) -> CommandResult {
    let report = crate::ResearchReport::terminal_unproven(query.clone(), failures)
        .map_err(|error| CommandError::incomplete(error.to_string()))?;
    let budget = crate::BudgetSnapshot {
        limits: crate::BudgetLimits::default(),
        usage: crate::BudgetUsage::default(),
    };
    let stages = vec![
        crate::StageRecord {
            stage: crate::WorkflowStage::Created,
            completed: true,
            detail: Some(format!(
                "route_frozen:{};allowed_effects:{};effect_grant:{};approval_receipts:{};selected_provider_denominator:{}",
                route.digest().map_err(|error| CommandError::incomplete(error.to_string()))?,
                route.allowed_effects.join(","),
                authorization.effect_grant.join(","),
                if route.human_gates.is_empty() { "not-required" } else { "missing" },
                selected_provider_denominator
            )),
        },
        crate::StageRecord {
            stage: crate::WorkflowStage::Unproven,
            completed: true,
            detail: Some("required_host_evidence_unavailable_or_provider_denominator_unmet".into()),
        },
    ];
    let receipt = crate::ResearchReceipt::from_terminal_bound(
        &report,
        budget,
        stages,
        available_source_records as u64,
        external_requests as u64,
        route,
        authorization,
        selected_provider_denominator as u64,
    )
    .map_err(|error| CommandError::incomplete(error.to_string()))?;
    Ok(json!({
        "schemaVersion": 1,
        "kind": "rightkit-research",
        "status": "unproven",
        "verdict": "UNPROVEN",
        "incomplete": true,
        "query": query,
        "route": route,
        "availableSourceRecords": available_source_records,
        "independentProviders": independent_provider_count,
        "requiredIndependentProviders": required_independent_providers,
        "independence": independence,
        "failures": report.omissions.clone(),
        "externalRequests": receipt.external_requests,
        "report": report,
        "receipt": receipt,
    }))
}

fn terminal_cancelled(
    query: String,
    route: &crate::ResearchRoute,
    authorization: &crate::ResearchAuthorization,
    selected_provider_denominator: usize,
) -> CommandResult {
    let report = crate::ResearchReport::terminal_cancelled(query.clone())
        .map_err(|error| CommandError::incomplete(error.to_string()))?;
    let budget = crate::BudgetSnapshot {
        limits: crate::BudgetLimits::default(),
        usage: crate::BudgetUsage::default(),
    };
    let stages = vec![
        crate::StageRecord {
            stage: crate::WorkflowStage::Created,
            completed: true,
            detail: Some(format!(
                "route_frozen:{};allowed_effects:{};effect_grant:{};approval_receipts:{};selected_provider_denominator:{}",
                route.digest().map_err(|error| CommandError::incomplete(error.to_string()))?,
                route.allowed_effects.join(","),
                authorization.effect_grant.join(","),
                if route.human_gates.is_empty() { "not-required" } else { "missing" },
                selected_provider_denominator
            )),
        },
        crate::StageRecord {
            stage: crate::WorkflowStage::Cancelled,
            completed: true,
            detail: Some("caller_cancellation_observed".into()),
        },
    ];
    let receipt = crate::ResearchReceipt::from_terminal_bound(
        &report,
        budget,
        stages,
        0,
        0,
        route,
        authorization,
        selected_provider_denominator as u64,
    )
    .map_err(|error| CommandError::incomplete(error.to_string()))?;
    Ok(json!({
        "schemaVersion": 1,
        "kind": "rightkit-research",
        "status": "cancelled",
        "incomplete": true,
        "query": query,
        "route": route,
        "verdict": "CANCELLED",
        "failures": report.omissions.clone(),
        "externalRequests": 0,
        "report": report,
        "receipt": receipt,
    }))
}

fn validate_route_input(
    query: &str,
    route: crate::ResearchRoute,
    domain: Option<&str>,
    sensitivity: Option<&str>,
) -> Result<crate::ResearchRoute, CommandError> {
    if route.subject.query.as_deref() != Some(query) {
        return Err(CommandError::usage(
            "--route-json subject.query must exactly match --query",
        ));
    }
    if domain.is_some_and(|value| value != route.domain) {
        return Err(CommandError::usage(
            "--domain conflicts with --route-json domain",
        ));
    }
    if sensitivity.is_some_and(|value| value != route.sensitivity) {
        return Err(CommandError::usage(
            "--sensitivity conflicts with --route-json sensitivity",
        ));
    }
    route
        .validate()
        .map_err(|error| CommandError::usage(error.to_string()))?;
    Ok(route)
}

fn route_unavailable_reason(
    route: &crate::ResearchRoute,
    has_source_records: bool,
) -> Option<&'static str> {
    if route.operation == "manage-corpus" || route.operation == "generate-artifact" {
        return Some(
            "stateful research operation is not implemented by this host-opened-record command",
        );
    }
    if route.provider == "notebooklm" {
        return Some("requested provider execution is unavailable; this command consumes host-opened source records only");
    }
    if route.provider != "local-corpus"
        && !(has_source_records && matches!(route.provider.as_str(), "browser" | "domain-default"))
    {
        return Some("requested provider execution is unavailable; this command consumes host-opened source records only");
    }
    if route.allowed_effects.iter().any(|effect| {
        matches!(
            effect.as_str(),
            "upload-notebooklm" | "create-artifact" | "patch-sourced-draft" | "spawn-worker"
        )
    }) {
        return Some(
            "requested stateful effect is not implemented by this host-opened-record command",
        );
    }
    None
}

fn terminal_route_unavailable(
    query: String,
    reason: &str,
    route: &crate::ResearchRoute,
    authorization: &crate::ResearchAuthorization,
    selected_provider_denominator: usize,
) -> CommandResult {
    let mut result = terminal_unproven(
        query,
        vec![crate::SourceFailure {
            provider: route.provider.clone(),
            stage: crate::WorkflowStage::Discovering,
            reason: reason.to_string(),
        }],
        0,
        0,
        1,
        Value::Null,
        0,
        route,
        authorization,
        selected_provider_denominator,
    )?;
    if let Value::Object(fields) = &mut result {
        fields.insert("status".into(), Value::String("unavailable".into()));
        fields.insert("verdict".into(), Value::String("UNAVAILABLE".into()));
        fields.insert("reason".into(), Value::String(reason.into()));
    }
    Ok(result)
}

fn resolve_route(
    query: &str,
    args: &ResearchOptions,
) -> Result<crate::ResearchRoute, CommandError> {
    if let Some(path) = &args.route_json {
        let bytes = std::fs::read(path)
            .map_err(|error| CommandError::usage(format!("cannot read --route-json: {error}")))?;
        let route: crate::ResearchRoute = serde_json::from_slice(&bytes)
            .map_err(|error| CommandError::usage(format!("invalid --route-json: {error}")))?;
        return validate_route_input(
            query,
            route,
            args.domain.as_deref(),
            args.sensitivity.as_deref(),
        );
    }
    let mut route = crate::ResearchRoute::host_injected_with(
        query,
        args.domain.as_deref().unwrap_or("general"),
        args.sensitivity.as_deref().unwrap_or("public"),
    );
    if route.domain == "medical" {
        route.subject.issue = Some(query.to_string());
        route.subject.patient = Some(crate::ResearchPatient {
            kind: "anonymous".into(),
            history_available: Some(false),
            ..Default::default()
        });
    }
    route
        .validate()
        .map_err(|error| CommandError::usage(error.to_string()))?;
    Ok(route)
}

pub fn run(args: ResearchOptions, cancellation: Cancellation) -> CommandResult {
    let kind = args.kind.clone();
    let mut result = run_inner(args, cancellation)?;
    if let Some(Value::String(found)) = result.get_mut("kind") {
        if let Some(rest) = found.strip_prefix("rightkit-research") {
            *found = format!("{kind}{rest}");
        }
    }
    Ok(result)
}

fn run_inner(args: ResearchOptions, cancellation: Cancellation) -> CommandResult {
    if let Some(step) = args.step.clone() {
        return run_step(&step, &args, &cancellation);
    }
    let query = args
        .query
        .clone()
        .ok_or_else(|| CommandError::usage("--query is required unless --step is given"))?;
    let route = resolve_route(&query, &args)?;
    let authorization = crate::ResearchAuthorization::full(&route)
        .map_err(|error| CommandError::incomplete(error.to_string()))?;
    let requested_provider_denominator = args
        .provider
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        .max(1);
    if let Some(reason) = route_unavailable_reason(&route, !args.source_records.is_empty()) {
        return terminal_route_unavailable(
            query,
            reason,
            &route,
            &authorization,
            requested_provider_denominator,
        );
    }
    if cancellation.is_cancelled() {
        return terminal_cancelled(
            query,
            &route,
            &authorization,
            requested_provider_denominator,
        );
    }
    let mut records = BTreeMap::<String, Vec<crate::SourceRecord>>::new();
    let mut failures = Vec::new();
    let mut digest_computed_source_ids = Vec::<String>::new();
    for path in &args.source_records {
        if cancellation.is_cancelled() {
            let selected_provider_denominator = if args.provider.is_empty() {
                records.len().max(1)
            } else {
                requested_provider_denominator
            };
            return terminal_cancelled(
                query.clone(),
                &route,
                &authorization,
                selected_provider_denominator,
            );
        }
        let source = match std::fs::read(path).map_err(io_error).and_then(|bytes| {
            crate::SourceRecord::from_host_json(&bytes)
                .map_err(|error| CommandError::usage(format!("invalid source record: {error}")))
        }) {
            Ok((source, computed)) => {
                if computed {
                    digest_computed_source_ids.push(source.source_id.clone());
                }
                source
            }
            Err(error) => {
                failures.push(crate::SourceFailure {
                    provider: path.display().to_string(),
                    stage: crate::WorkflowStage::Discovering,
                    reason: format!("host source record unavailable: {}", error.message),
                });
                continue;
            }
        };
        if let Err(error) = source.validate() {
            failures.push(crate::SourceFailure {
                provider: source.provider.clone(),
                stage: crate::WorkflowStage::Discovering,
                reason: format!("host source record rejected: {error}"),
            });
            continue;
        }
        records
            .entry(source.provider.clone())
            .or_default()
            .push(source);
    }
    if records.is_empty() {
        failures.push(crate::SourceFailure {
            provider: "host-injected-sources".into(),
            stage: crate::WorkflowStage::Discovering,
            reason: "required host-injected source records are unavailable".into(),
        });
        return terminal_unproven(
            query,
            failures,
            0,
            0,
            args.min_independent_providers,
            Value::Null,
            0,
            &route,
            &authorization,
            args.provider
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                .max(1),
        );
    }

    let providers = if args.provider.is_empty() {
        records
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
    } else {
        args.provider
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
    };
    let independence = crate::summarize_independence(
        records
            .iter()
            .filter(|(provider, _)| providers.contains(*provider))
            .flat_map(|(_, sources)| sources.iter()),
    );
    let independent_provider_count = independence.count(args.allow_self_declared_independence);
    let independence_basis =
        independence_basis_json(&independence, args.allow_self_declared_independence);
    if independent_provider_count < args.min_independent_providers {
        failures.push(crate::SourceFailure {
            provider: "provider-denominator".into(),
            stage: crate::WorkflowStage::Discovering,
            reason: "independent source-provider requirement is unmet".into(),
        });
        return terminal_unproven(
            query,
            failures,
            records.values().flatten().count(),
            independent_provider_count,
            args.min_independent_providers,
            independence_basis,
            records
                .values()
                .flatten()
                .filter(|source| source.metadata.contains_key("request_receipt"))
                .count(),
            &route,
            &authorization,
            providers.len(),
        );
    }
    let providers = providers.into_iter().collect::<Vec<_>>();
    let request = crate::WorkflowRequest {
        schema_version: 1,
        query,
        source_providers: providers,
        max_hits_per_provider: args.max_hits,
        max_source_bytes: 1_000_000,
    };
    request
        .validate()
        .map_err(|error| CommandError::usage(error.to_string()))?;
    let workflow_cancellation = cancellation.clone();
    let mut workflow = crate::ResearchWorkflow::new(
        crate::BudgetLimits::default(),
        Instant::now() + Duration::from_secs(30),
        workflow_cancellation,
    );
    for (provider, records) in records {
        workflow
            .register(Arc::new(RecordSource { provider, records }))
            .map_err(|error| CommandError::incomplete(error.to_string()))?;
    }
    let outcome = workflow.run_with_route(request.clone(), route.clone(), authorization.clone());
    let outcome = outcome.map_err(|error| CommandError::incomplete(error.to_string()))?;
    // Source records rejected or unreadable before the workflow ran are failures
    // of this run too. Dropping them would let a run with missing evidence read
    // as complete, so they are reported and the status is at most partial.
    let mut failure_values = Vec::new();
    for failure in failures.iter().chain(outcome.failures.iter()) {
        failure_values.push(serde_json::to_value(failure).map_err(io_error)?);
    }
    let status = if outcome.status == crate::WorkflowStatus::Ok && !failure_values.is_empty() {
        serde_json::to_value(crate::WorkflowStatus::Partial).map_err(io_error)?
    } else {
        serde_json::to_value(outcome.status).map_err(io_error)?
    };
    let incomplete = outcome.status != crate::WorkflowStatus::Ok || !failure_values.is_empty();
    let receipt = crate::ResearchReceipt::from_outcome(&outcome)
        .map_err(|error| CommandError::incomplete(error.to_string()))?;
    let external_requests = receipt.external_requests;
    let evidence = outcome.ledger.records().cloned().collect::<Vec<_>>();
    let claims = outcome.ledger.claims().cloned().collect::<Vec<_>>();
    Ok(json!({
        "schemaVersion": 1,
        "kind": "rightkit-research",
        "status": status,
        "incomplete": incomplete,
        "request": request,
        "route": route,
        "report": outcome.report,
        "receipt": receipt,
        "externalRequests": external_requests,
        "independentProviders": independent_provider_count,
        "requiredIndependentProviders": args.min_independent_providers,
        "independence": independence_basis,
        "digestComputedByRoute": !digest_computed_source_ids.is_empty(),
        "digestComputedSourceIds": digest_computed_source_ids,
        "evidence": evidence,
        "claims": claims,
        "failures": failure_values,
        "budget": outcome.budget,
        "stages": outcome.stages
    }))
}

/// Truthful independence report: count basis, keys, and whether any key is self-declared.
fn independence_basis_json(
    summary: &crate::IndependenceSummary,
    allow_self_declared: bool,
) -> Value {
    json!({
        "basis": "source-identity-registrable-domain",
        "sourceIdentityKeys": summary.source_identity_keys,
        "selfDeclaredKeys": summary.self_declared_keys,
        "selfDeclared": !summary.self_declared_keys.is_empty(),
        "selfDeclaredCounted": allow_self_declared && !summary.self_declared_keys.is_empty(),
    })
}

struct RecordSource {
    provider: String,
    records: Vec<crate::SourceRecord>,
}

impl crate::SourceClient for RecordSource {
    fn provider(&self) -> &str {
        &self.provider
    }

    fn estimated_bytes(&self, hit: &crate::SourceHit) -> u64 {
        self.records
            .iter()
            .find(|record| record.source_id == hit.source_id)
            .map(|record| record.byte_length)
            .unwrap_or(1)
    }

    fn search(
        &self,
        _: &str,
        limit: u32,
        _: Instant,
        cancellation: &crate::Cancellation,
    ) -> Result<Vec<crate::SourceHit>, crate::ResearchError> {
        if cancellation.is_cancelled() {
            return Err(crate::ResearchError::Cancelled);
        }
        Ok(self
            .records
            .iter()
            .take(limit as usize)
            .map(|record| crate::SourceHit {
                source_id: record.source_id.clone(),
                uri: record.uri.clone(),
                title: record.title.clone(),
                provider: record.provider.clone(),
                relevance: None,
            })
            .collect())
    }

    fn open(
        &self,
        hit: &crate::SourceHit,
        _: Instant,
        cancellation: &crate::Cancellation,
    ) -> Result<crate::SourceRecord, crate::ResearchError> {
        if cancellation.is_cancelled() {
            return Err(crate::ResearchError::Cancelled);
        }
        self.records
            .iter()
            .find(|record| record.source_id == hit.source_id)
            .cloned()
            .ok_or_else(|| crate::ResearchError::InvalidSource("source hit is unavailable".into()))
    }
}

/// Dispatches one pure Research step. Every step returns JSON; a status outside
/// `ok` maps to a non-zero exit in the CLI (`failed` 2, `unavailable` 2).
fn run_step(step: &str, args: &ResearchOptions, cancellation: &Cancellation) -> CommandResult {
    match step {
        "ledger-check" => step_ledger_check(args),
        "ledger-render" => step_ledger_render(args),
        "citecheck" => step_citecheck(args),
        "meter" => step_meter(args),
        "retraction" => step_retraction(args, cancellation),
        "issue-patch" => step_issue_patch(args),
        "apply-patch" => step_apply_patch(args),
        other => Err(CommandError::usage(format!(
            "unknown --step {other}; expected ledger-check, ledger-render, citecheck, meter, retraction, issue-patch, or apply-patch"
        ))),
    }
}

fn required_path<'a>(value: &'a Option<PathBuf>, flag: &str) -> Result<&'a PathBuf, CommandError> {
    value
        .as_ref()
        .ok_or_else(|| CommandError::usage(format!("{flag} is required for this --step")))
}

fn read_text(path: &std::path::Path) -> Result<String, CommandError> {
    std::fs::read_to_string(path)
        .map_err(|error| CommandError::usage(format!("cannot read {}: {error}", path.display())))
}

fn read_jsonl(path: &std::path::Path) -> Result<Vec<Value>, CommandError> {
    let text = read_text(path)?;
    ledger::parse_jsonl(&path.display().to_string(), &text).map_err(CommandError::usage)
}

fn read_lines(path: &std::path::Path) -> Result<Vec<String>, CommandError> {
    Ok(read_text(path)?
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect())
}

fn step_envelope(step: &str, status: &str, result: Value) -> Value {
    json!({
        "schemaVersion": 1,
        "kind": "rightkit-research-step",
        "step": step,
        "status": status,
        "incomplete": status != "ok",
        "result": result,
    })
}

fn step_ledger_check(args: &ResearchOptions) -> CommandResult {
    let evidence = read_jsonl(required_path(&args.evidence, "--evidence")?)?;
    let claims = read_jsonl(required_path(&args.claims, "--claims")?)?;
    let checked = ledger::check(&evidence, &claims).map_err(CommandError::usage)?;
    let status = if checked.overall_ok { "ok" } else { "failed" };
    Ok(step_envelope("ledger-check", status, checked.to_json()))
}

fn step_ledger_render(args: &ResearchOptions) -> CommandResult {
    let evidence = read_jsonl(required_path(&args.evidence, "--evidence")?)?;
    let claims = read_jsonl(required_path(&args.claims, "--claims")?)?;
    let out = required_path(&args.out, "--out")?;
    let as_of = crate::research_port::today();
    match ledger::render(&evidence, &claims, &args.title, &as_of) {
        Ok(text) => {
            crate::research_port::atomic_write_text(out, &text).map_err(io_error)?;
            Ok(step_envelope(
                "ledger-render",
                "ok",
                json!({"out": out.display().to_string(), "bytes": text.len()}),
            ))
        }
        Err(reason) => Ok(step_envelope(
            "ledger-render",
            "failed",
            json!({"reason": reason}),
        )),
    }
}

fn step_citecheck(args: &ResearchOptions) -> CommandResult {
    let draft = read_text(required_path(&args.draft, "--draft")?)?;
    let evidence = read_jsonl(required_path(&args.evidence, "--evidence")?)?;
    let report = citecheck::check(&draft, &evidence).map_err(CommandError::usage)?;
    let status = if report["ok"].as_bool() == Some(true) {
        "ok"
    } else {
        "failed"
    };
    Ok(step_envelope("citecheck", status, report))
}

fn state_u64(section: &Value, key: &str) -> Result<u64, CommandError> {
    section.get(key).and_then(Value::as_u64).ok_or_else(|| {
        CommandError::usage(format!(
            "meter state field {key} must be an unsigned integer"
        ))
    })
}

fn step_meter(args: &ResearchOptions) -> CommandResult {
    let state_path = required_path(&args.meter_state, "--meter-state")?;
    let state: Value = serde_json::from_str(&read_text(state_path)?)
        .map_err(|error| CommandError::usage(format!("invalid meter state: {error}")))?;
    let budget_section = state
        .get("budget")
        .ok_or_else(|| CommandError::usage("meter state requires a budget object"))?;
    let usage_section = state
        .get("usage")
        .ok_or_else(|| CommandError::usage("meter state requires a usage object"))?;
    let budget = meter::MeterBudget {
        external_requests: state_u64(budget_section, "external_requests")?,
        workers: state_u64(budget_section, "workers")?,
    };
    let current = meter::MeterUsage {
        external_requests: state_u64(usage_section, "external_requests")?,
        workers_active: state_u64(usage_section, "workers_active")?,
        workers_started: state_u64(usage_section, "workers_started")?,
    };
    let effect = args
        .effect
        .as_deref()
        .ok_or_else(|| CommandError::usage("--effect is required for --step meter"))?;
    let worker_event = args.worker_event.as_deref();
    let outcome = meter::consume(&budget, &current, effect, args.units, worker_event)
        .map_err(CommandError::usage)?;
    let refused = matches!(outcome, meter::MeterOutcome::Refused { .. });
    let mut result = meter::outcome_json(&budget, &outcome, effect, args.units, worker_event);
    if let Value::Object(map) = &mut result {
        map.insert("persisted".to_string(), Value::Bool(false));
    }
    let status = if refused { "failed" } else { "ok" };
    Ok(step_envelope("meter", status, result))
}

fn step_retraction(args: &ResearchOptions, cancellation_token: &Cancellation) -> CommandResult {
    if cancellation_token.is_cancelled() {
        return Ok(step_envelope(
            "retraction",
            "failed",
            json!({
                "dois_checked": 0,
                "results": [],
                "block_brief": true,
                "unknown_count": 0,
                "terminal_failure": true,
                "reason": "research workflow cancelled",
            }),
        ));
    }
    let dois = read_lines(required_path(&args.dois, "--dois")?)?;
    let disclosed = match &args.disclosed {
        Some(path) => read_lines(path)?,
        None => Vec::new(),
    };
    let fixture: Option<serde_json::Map<String, Value>> = match &args.retraction_fixture {
        Some(path) => {
            let value: Value = serde_json::from_str(&read_text(path)?).map_err(|error| {
                CommandError::usage(format!("invalid retraction fixture: {error}"))
            })?;
            match value {
                Value::Object(map) => Some(map),
                _ => {
                    return Err(CommandError::usage(
                        "retraction fixture must be a JSON object",
                    ))
                }
            }
        }
        None => None,
    };
    let use_live_network = fixture.is_none() && !args.offline;
    let thread_token = cancellation_token.clone();
    let allow_unknown = args.allow_unknown;
    let (mut result, budget) = std::thread::Builder::new()
        .name("research-retraction".into())
        .spawn(move || {
            use std::cell::RefCell;
            let cancellation = thread_token;
            let budget = RefCell::new(crate::BudgetAccount::from_timeout(
                crate::BudgetLimits::default(),
                Duration::from_secs(30),
            ));
            let seen_sources = RefCell::new(std::collections::BTreeSet::new());
            let client = use_live_network.then(retraction::live_client);
            let fetch = |verifier, doi: &str| {
                if !use_live_network {
                    return retraction::network_unavailable(verifier, doi);
                }
                let client = match client.as_ref() {
                    Some(Ok(client)) => client,
                    Some(Err(error)) => return Err(error.clone().into()),
                    None => return Err("HTTP client unavailable".into()),
                };
                if !seen_sources.borrow().contains(doi) {
                    budget
                        .borrow_mut()
                        .reserve_source(&cancellation)
                        .map_err(|error| retraction::FetchError {
                            message: error.to_string(),
                            terminal: true,
                        })?;
                    seen_sources.borrow_mut().insert(doi.to_owned());
                }
                retraction::fetch_live(
                    client,
                    verifier,
                    doi,
                    &mut budget.borrow_mut(),
                    &cancellation,
                )
            };
            let result =
                retraction::sweep(&dois, &disclosed, !allow_unknown, fixture.as_ref(), &fetch);
            (result, budget.into_inner().snapshot())
        })
        .map_err(|error| {
            CommandError::internal(format!("cannot start retraction worker: {error}"))
        })?
        .join()
        .map_err(|_| CommandError::internal("retraction worker panicked"))?;
    if let Value::Object(fields) = &mut result {
        fields.insert(
            "budget".into(),
            serde_json::to_value(budget).map_err(io_error)?,
        );
        fields.insert(
            "fetch_mode".into(),
            Value::String(if use_live_network {
                "live".into()
            } else if args.retraction_fixture.is_some() {
                "fixture-offline".into()
            } else {
                "offline".into()
            }),
        );
    }
    let gap = retraction::has_unavailable_gap(&result);
    let blocked = result["block_brief"].as_bool().unwrap_or(true);
    let terminal_failure = retraction::has_terminal_failure(&result);
    let status = if terminal_failure {
        "failed"
    } else if gap {
        "unavailable"
    } else if blocked {
        "failed"
    } else {
        "ok"
    };
    let mut envelope = step_envelope("retraction", status, result);
    if gap {
        if let Value::Object(map) = &mut envelope {
            map.insert(
                "reason".to_string(),
                Value::String(retraction::UNAVAILABLE_NETWORK.to_string()),
            );
        }
    }
    if terminal_failure {
        if let Value::Object(map) = &mut envelope {
            map.insert(
                "reason".to_string(),
                Value::String(
                    "retraction verification stopped on cancellation, deadline, or budget".into(),
                ),
            );
        }
    }
    Ok(envelope)
}

fn step_issue_patch(args: &ResearchOptions) -> CommandResult {
    let draft = required_path(&args.draft, "--draft")?;
    let run_id = args
        .run_id
        .as_deref()
        .ok_or_else(|| CommandError::usage("--run-id is required for --step issue-patch"))?;
    let key_file = required_path(&args.key_file, "--key-file")?;
    let out = required_path(&args.out, "--out")?;
    let receipt = patch_guard::issue_receipt(draft, run_id, key_file)
        .map_err(|error| CommandError::usage(error.to_string()))?;
    let draft_path = receipt["sourced_draft_path"]
        .as_str()
        .ok_or_else(|| CommandError::internal("issued patch receipt lacks draft path"))?;
    let out_target = patch_guard::canonical_output_path(out)
        .map_err(|error| CommandError::usage(error.to_string()))?;
    let key_target = std::fs::canonicalize(key_file).map_err(io_error)?;
    if out_target == *draft_path || out_target == key_target {
        return Err(CommandError::usage(
            "--out must be distinct from --draft and --key-file",
        ));
    }
    let receipt_text = serde_json::to_string_pretty(&receipt).map_err(io_error)? + "\n";
    crate::research_port::atomic_write_text(out, &receipt_text).map_err(io_error)?;
    Ok(step_envelope(
        "issue-patch",
        "ok",
        json!({
            "receipt": out.display().to_string(),
            "receipt_sha256": patch_guard::sha256(receipt_text.as_bytes()),
            "draft_path": draft_path,
            "draft_sha256": receipt["sourced_draft_sha256"],
            "run_id": run_id,
            "assurance": patch_guard::ASSURANCE,
        }),
    ))
}

fn step_apply_patch(args: &ResearchOptions) -> CommandResult {
    let draft = required_path(&args.draft, "--draft")?;
    let run_id = args
        .run_id
        .as_deref()
        .ok_or_else(|| CommandError::usage("--run-id is required for --step apply-patch"))?;
    let key_file = required_path(&args.key_file, "--key-file")?;
    let receipt = required_path(&args.receipt, "--receipt")?;
    let diff = required_path(&args.diff, "--diff")?;
    let result = patcher::apply_file(draft, run_id, key_file, receipt, diff)
        .map_err(|error| CommandError::usage(error.to_string()))?;
    Ok(step_envelope(
        "apply-patch",
        "ok",
        json!({
            "draft_path": result.draft_path,
            "before_sha256": result.before_sha256,
            "after_sha256": result.after_sha256,
            "diff_sha256": result.diff_sha256,
            "applied_hunks": result.applied_hunks,
            "assurance": patch_guard::ASSURANCE,
        }),
    ))
}
