#[derive(Subcommand, Debug, PartialEq)]
pub enum SkillCommand {
    /// List installed skills from the running daemon.
    List {
        /// Accepted for parity; list already emits JSON.
        #[arg(long)]
        json: bool,
    },
    /// List durable skill proposals, versions, benchmark verdicts, and governance audit entries.
    Proposals,
    /// Store a proposed skill document in the governance store.
    Propose {
        /// Stable skill slug for the proposed skill.
        #[arg(long)]
        slug: String,
        /// Short proposal summary.
        #[arg(long)]
        summary: String,
        /// Proposed skill markdown file to read.
        #[arg(long = "content-file")]
        content_file: String,
        /// Optional stable proposal id. Defaults to a timestamped proposal id.
        #[arg(long)]
        id: Option<String>,
        /// Optional display title. Defaults to the skill slug.
        #[arg(long)]
        title: Option<String>,
        /// Source session id that produced the proposal.
        #[arg(long = "source-session")]
        source_session_id: Option<String>,
        /// Source task id that produced the proposal.
        #[arg(long = "source-task")]
        source_task_id: Option<String>,
    },
    /// Materialize a proposal as a disabled/quarantined managed skill.
    QuarantineProposal {
        /// Proposal id to quarantine.
        proposal_id: String,
        /// Human reason written to the governance audit log.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Record a benchmark verdict for a skill proposal.
    BenchmarkProposal {
        /// Proposal id to attach the verdict to.
        proposal_id: String,
        /// Benchmark or fixture id.
        #[arg(long = "benchmark")]
        benchmark_id: String,
        /// Gate mode: hard, soft, or mixed.
        #[arg(long = "gate-mode", default_value = "hard")]
        gate_mode: String,
        /// Continuous score for soft/mixed gates. Defaults to 1.0 when passed, else 0.0.
        #[arg(long)]
        score: Option<f64>,
        /// Mark the verdict as passing.
        #[arg(long, conflicts_with = "failed")]
        passed: bool,
        /// Mark the verdict as failing.
        #[arg(long, conflicts_with = "passed")]
        failed: bool,
        /// Override the skill slug associated with the verdict.
        #[arg(long)]
        slug: Option<String>,
        /// Human reason written to the governance audit log.
        #[arg(long)]
        reason: Option<String>,
        /// Optional JSON verdict payload whose hash becomes the evidence hash.
        #[arg(long = "verdict-file")]
        verdict_file: Option<String>,
    },
    /// Run a real RightKit benchmark and record its report as a proposal verdict.
    RunBenchmarkProposal {
        /// Proposal id to benchmark.
        proposal_id: String,
        /// Benchmark runner kind: role or terminalBench.
        #[arg(long, default_value = "role")]
        kind: String,
        /// Session id to use or create as benchmark context.
        #[arg(long = "session")]
        session_id: String,
        /// Agent role for role benchmarks, or fallback agent for the RightKit task pack.
        #[arg(long)]
        role: Option<String>,
        /// Model override for role benchmarks.
        #[arg(long)]
        model: Option<String>,
        /// Goal text for role benchmarks.
        #[arg(long)]
        goal: Option<String>,
        /// Repeat count for role benchmarks.
        #[arg(long)]
        repeats: Option<u32>,
        /// RightKit task-pack id (legacy command name retained for compatibility).
        #[arg(long = "task")]
        task_id: Option<String>,
        /// Agent role for the RightKit task pack.
        #[arg(long)]
        agent: Option<String>,
        /// Benchmark id stored in the proposal verdict.
        #[arg(long = "benchmark")]
        benchmark_id: Option<String>,
        /// Gate mode: hard, soft, or mixed.
        #[arg(long = "gate-mode")]
        gate_mode: Option<String>,
        /// Score threshold for the gate.
        #[arg(long)]
        threshold: Option<f64>,
        /// Human reason written to the governance audit log.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Quarantine an installed managed skill.
    Quarantine {
        /// Skill slug.
        slug: String,
        /// Human reason written to the audit log.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Activate an installed skill after benchmark gates, or with an explicit operator override.
    Activate {
        /// Skill slug.
        slug: String,
        /// Human reason written to the audit log.
        #[arg(long)]
        reason: Option<String>,
        /// Explicitly override the benchmark gate.
        #[arg(long)]
        operator_override: bool,
    },
    /// Retire an installed managed skill.
    Retire {
        /// Skill slug.
        slug: String,
        /// Human reason written to the audit log.
        #[arg(long)]
        reason: Option<String>,
    },
    /// List SkillOpt-style training runs.
    TrainingRuns,
    /// Read one SkillOpt-style training run.
    TrainingRun {
        /// Training run id.
        run_id: String,
    },
    /// Resume a SkillOpt-style training run from an optional checkpoint JSON file.
    ResumeTrainingRun {
        /// Training run id.
        run_id: String,
        /// Optional JSON checkpoint file.
        #[arg(long = "checkpoint-file")]
        checkpoint_file: Option<String>,
        /// Human message written to the run history.
        #[arg(long)]
        message: Option<String>,
    },
    /// Generate a SkillOpt candidate proposal from the current meta-skill.
    ProposeTrainingCandidate {
        /// Training run id.
        run_id: String,
        /// Candidate edit operation: add, delete, replace, or rewrite.
        #[arg(long)]
        operation: Option<String>,
        /// Summary to seed the generated proposal.
        #[arg(long)]
        summary: Option<String>,
        /// Extra instructions for the proposal generator.
        #[arg(long)]
        instructions: Option<String>,
    },
    /// Execute one SkillOpt rollout/reflection/update/evaluate epoch.
    ExecuteTrainingRun {
        /// Training run id.
        run_id: String,
        /// Candidate skill document body to evaluate.
        #[arg(long = "proposed-body-file")]
        proposed_body_file: String,
        /// Held-out validation score produced by the benchmark runner.
        #[arg(long = "validation-score")]
        validation_score: f64,
        /// Candidate edit operation: add, delete, replace, or rewrite.
        #[arg(long, default_value = "replace")]
        operation: String,
        /// Human summary written into candidate history.
        #[arg(long)]
        summary: Option<String>,
        /// Gate mode to apply: hard, soft, or mixed.
        #[arg(long = "gate-mode")]
        gate_mode: Option<String>,
        /// Additional score threshold for this epoch.
        #[arg(long)]
        threshold: Option<f64>,
    },
    /// Cancel a SkillOpt-style training run.
    CancelTrainingRun {
        /// Training run id.
        run_id: String,
        /// Human reason written to the run history.
        #[arg(long)]
        reason: Option<String>,
    },
    /// List a SkillOpt training run's history.
    TrainingRunHistory {
        /// Training run id.
        run_id: String,
    },
    /// List a SkillOpt training run's rejected-edit buffer.
    TrainingRunRejectedEdits {
        /// Training run id.
        run_id: String,
    },
    /// Start a SkillOpt-style training run record with gates and rejected-edit buffer entries.
    StartTrainingRun {
        /// Human-readable run name.
        #[arg(long)]
        name: String,
        /// Target skill id/slug.
        #[arg(long = "skill")]
        skill_id: String,
        /// Optimization objective.
        #[arg(long)]
        objective: String,
        /// Optimizer model or strategy id.
        #[arg(long)]
        optimizer: String,
        /// Target model to evaluate the skill against. Defaults to the active session/default model.
        #[arg(long = "target-model")]
        target_model: Option<String>,
        /// Benchmark gate in name:kind:threshold form. May be repeated.
        #[arg(long = "gate")]
        gates: Vec<String>,
        /// Rejected edit in artifact_id:reason:patch_preview form. May be repeated.
        #[arg(long = "rejected-edit")]
        rejected_edits: Vec<String>,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum LearningCommand {
    /// Approve a learning proposal and promote it through the governed runtime route.
    Approve {
        /// Learning proposal id.
        proposal_id: String,
    },
    /// Reject a learning proposal through the governed runtime route.
    Reject {
        /// Learning proposal id.
        proposal_id: String,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum InsightsCommand {
    /// Mine completed session transcripts for recurring failure reports.
    /// This reads exported evidence and never admits findings to Cortex.
    List {
        /// One or more session IDs whose exported transcripts should be mined.
        #[arg(long = "session", required = true)]
        session_ids: Vec<String>,
        /// Adapt issue scope.
        #[arg(long, default_value = "workspace")]
        scope: String,
        /// Minimum recurrence count for an issue.
        #[arg(long, default_value_t = 2)]
        min_recurrence: u32,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum DiagnosticsCommand {
    /// Read bounded daemon diagnostics once or at a fixed cadence.
    Watch {
        /// Emit one sample then exit.
        #[arg(long)]
        once: bool,
        /// Cadence in milliseconds; minimum 250ms prevents busy polling.
        #[arg(long, default_value_t = 1_000, value_parser = clap::value_parser!(u64).range(250..))]
        interval_ms: u64,
        /// Render each sample as one JSON object.
        #[arg(long)]
        json: bool,
    },
    /// Ask the running daemon to build a redacted support bundle (RightKit ZIP).
    Bundle {
        /// Output path for the support bundle ZIP.
        #[arg(long)]
        output: String,
    },
}
