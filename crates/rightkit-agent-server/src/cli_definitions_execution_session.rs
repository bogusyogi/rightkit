#[derive(Subcommand, Debug, PartialEq)]
pub enum ExecutionCommand {
    /// List execution backends and workspace overrides.
    Backends {
        /// Workspace root for workspace-scoped execution settings.
        #[arg(long)]
        workspace_root: Option<String>,
        /// Accepted for parity; backends already emits JSON.
        #[arg(long)]
        json: bool,
    },
    /// Run diagnostic checks for one execution backend.
    Doctor {
        /// Execution backend id, for example local.
        backend_id: String,
        /// Workspace root for workspace-scoped execution settings.
        #[arg(long)]
        workspace_root: Option<String>,
        /// Accepted for parity; doctor already emits JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum SessionCommand {
    /// List active and persisted sessions.
    List {
        /// Filter by status, for example running, dormant, done, interrupted.
        #[arg(long)]
        status: Option<String>,
        /// Filter by tag.
        #[arg(long)]
        tag: Option<String>,
        /// Accepted for parity; session list already emits JSON.
        #[arg(long)]
        json: bool,
    },
    /// Search persisted session transcripts.
    Search {
        /// Search query.
        query: String,
        /// Maximum hits to return.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Delete a session record and stop any live session with that id.
    Delete {
        /// Session ID to delete.
        session_id: String,
    },
    /// Archive a session without deleting its transcript or records.
    Archive {
        /// Session ID to archive.
        session_id: String,
    },
    /// Restore an archived session to dormant state.
    Restore {
        /// Session ID to restore.
        session_id: String,
    },
    /// Update session metadata used by CLI, web, and desktop clients.
    Set {
        /// Session ID to update.
        session_id: String,
        /// New display title.
        #[arg(long)]
        title: Option<String>,
        /// Model slug or local agent alias to use for future turns.
        #[arg(long)]
        model: Option<String>,
        /// Replace session tags with the provided values. May be repeated.
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Clear all session tags.
        #[arg(long)]
        clear_tags: bool,
    },
    /// Export a session transcript.
    Export {
        /// Session ID to export.
        session_id: String,
        /// Emit structured JSON instead of markdown.
        #[arg(long)]
        json: bool,
    },
}
