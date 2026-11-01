use clap::Subcommand;

#[derive(Subcommand, Debug, PartialEq)]
pub enum ScheduleCommand {
    /// Create a schedule from its daemon API JSON payload.
    Create {
        /// JSON payload path. Secrets are never accepted on argv.
        payload: String,
        #[arg(long)]
        workspace_root: Option<String>,
    },
    /// Show one schedule definition & next-run preview.
    Show {
        id: String,
        #[arg(long)]
        workspace_root: Option<String>,
    },
    /// Edit a schedule from its daemon API JSON payload.
    Edit {
        id: String,
        payload: String,
        #[arg(long)]
        workspace_root: Option<String>,
    },
    /// Delete one schedule.
    Delete {
        id: String,
        #[arg(long)]
        workspace_root: Option<String>,
    },
    /// List workspace schedules.
    List {
        /// Workspace root. Defaults to the current directory.
        #[arg(long)]
        workspace_root: Option<String>,
        /// Accepted for parity; schedule list already emits JSON.
        #[arg(long)]
        json: bool,
    },
    /// List recent runs for one schedule.
    Runs {
        /// Schedule id.
        id: String,
        /// Workspace root. Defaults to the current directory.
        #[arg(long)]
        workspace_root: Option<String>,
    },
    /// List schedule administration audit events.
    Audit {
        /// Workspace root. Defaults to the current directory.
        #[arg(long)]
        workspace_root: Option<String>,
    },
    /// Pause a schedule.
    Pause {
        /// Schedule id.
        id: String,
        /// Workspace root. Defaults to the current directory.
        #[arg(long)]
        workspace_root: Option<String>,
    },
    /// Resume a paused schedule.
    Resume {
        /// Schedule id.
        id: String,
        /// Workspace root. Defaults to the current directory.
        #[arg(long)]
        workspace_root: Option<String>,
    },
    /// Run a schedule now.
    Run {
        /// Schedule id.
        id: String,
        /// Workspace root. Defaults to the current directory.
        #[arg(long)]
        workspace_root: Option<String>,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum SpecCommand {
    /// List feature specs from the durable spec store.
    List {
        /// Filter specs by workspace root.
        #[arg(long)]
        workspace: Option<String>,
        /// Override the spec store path. Defaults beside the daemon DB.
        #[arg(long)]
        store: Option<String>,
        /// Emit structured JSON instead of a text table.
        #[arg(long)]
        json: bool,
    },
    /// Create a new feature spec from a description.
    New {
        /// Workspace root this spec governs. Defaults to the current directory.
        #[arg(long)]
        workspace: Option<String>,
        /// Feature description used as the source prompt.
        #[arg(long)]
        description: String,
        /// Optional title. Defaults to the first sentence of the description.
        #[arg(long)]
        title: Option<String>,
        /// Override the spec store path. Defaults beside the daemon DB.
        #[arg(long)]
        store: Option<String>,
        /// Emit structured JSON instead of a text summary.
        #[arg(long)]
        json: bool,
    },
    /// Show spec status, task count, and latest analysis summary.
    Status {
        /// Spec id.
        spec_id: String,
        /// Override the spec store path. Defaults beside the daemon DB.
        #[arg(long)]
        store: Option<String>,
        /// Emit structured JSON instead of a text summary.
        #[arg(long)]
        json: bool,
    },
    /// Run deterministic coverage analysis for a spec.
    Analyze {
        /// Spec id.
        spec_id: String,
        /// Override the spec store path. Defaults beside the daemon DB.
        #[arg(long)]
        store: Option<String>,
        /// Emit structured JSON instead of a text summary.
        #[arg(long)]
        json: bool,
    },
    /// Print the requirement/task/evidence trace for a spec.
    Trace {
        /// Spec id.
        spec_id: String,
        /// Override the spec store path. Defaults beside the daemon DB.
        #[arg(long)]
        store: Option<String>,
        /// Emit structured JSON instead of a text summary.
        #[arg(long)]
        json: bool,
    },
    /// Ask the daemon for clarification questions for a spec.
    Clarify {
        /// Spec id.
        spec_id: String,
    },
    /// Generate missing implementation and verification tasks for a spec.
    Plan {
        /// Spec id.
        spec_id: String,
        /// Additional planning constraints to stamp into generated tasks.
        #[arg(long)]
        constraints: Option<String>,
        /// Do not generate verification tasks for buildable success criteria.
        #[arg(long = "no-verification")]
        no_verification: bool,
    },
    /// Build a readiness checklist for a spec.
    Checklist {
        /// Spec id.
        spec_id: String,
        /// Optional checklist focus label or area.
        #[arg(long)]
        focus: Option<String>,
    },
    /// Start or dry-run a spec-bound implementation session through the daemon.
    Implement {
        /// Spec id.
        spec_id: String,
        /// Custom implementation session title.
        #[arg(long)]
        title: Option<String>,
        /// Return the implementation prompt and blockers without creating a session.
        #[arg(long = "dry-run")]
        dry_run: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser, Debug, PartialEq)]
    struct ScheduleCli {
        #[command(subcommand)]
        command: ScheduleCommand,
    }

    #[derive(Parser, Debug, PartialEq)]
    struct SpecCli {
        #[command(subcommand)]
        command: SpecCommand,
    }

    #[test]
    fn schedule_list_parses_flags() {
        let cli =
            ScheduleCli::try_parse_from(["t", "list", "--json", "--workspace-root", "/w"]).unwrap();
        assert_eq!(
            cli.command,
            ScheduleCommand::List {
                workspace_root: Some("/w".into()),
                json: true
            }
        );
    }

    #[test]
    fn schedule_pause_requires_id() {
        assert!(ScheduleCli::try_parse_from(["t", "pause"]).is_err());
        let cli = ScheduleCli::try_parse_from(["t", "pause", "s1"]).unwrap();
        assert_eq!(
            cli.command,
            ScheduleCommand::Pause {
                id: "s1".into(),
                workspace_root: None
            }
        );
    }

    #[test]
    fn spec_new_requires_description() {
        assert!(SpecCli::try_parse_from(["t", "new"]).is_err());
        let cli = SpecCli::try_parse_from(["t", "new", "--description", "add x"]).unwrap();
        assert_eq!(
            cli.command,
            SpecCommand::New {
                workspace: None,
                description: "add x".into(),
                title: None,
                store: None,
                json: false
            }
        );
    }

    #[test]
    fn spec_implement_dry_run_flag() {
        let cli = SpecCli::try_parse_from(["t", "implement", "sp1", "--dry-run"]).unwrap();
        assert_eq!(
            cli.command,
            SpecCommand::Implement {
                spec_id: "sp1".into(),
                title: None,
                dry_run: true
            }
        );
    }
}
