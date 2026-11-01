/// Daemon-wide settings kept in memory; persistence remains host-owned.
#[derive(Clone)]
pub struct DaemonSettingsStore {
    pub primary_orchestrator_model_id: String,
    pub per_task_max_usd: f64,
    pub per_goal_max_usd: f64,
    pub per_session_max_usd: f64,
    pub per_agent_call_max_usd: f64,
    pub max_retries_per_role: u32,
    /// Sub-agent run-length bound in minutes.
    pub max_wall_clock_minutes: u32,
    /// Root-turn wall clock in minutes; zero means no cap.
    pub turn_max_wall_clock_minutes: u32,
    /// Maximum LLM/tool iterations permitted for one turn.
    pub max_iterations: u32,
    /// Maximum tasks in one conductor batch; zero is unlimited.
    pub max_workers: u32,
    pub goal_continuation_enabled: bool,
    pub keep_daemon_resident: bool,
}

impl Default for DaemonSettingsStore {
    fn default() -> Self {
        Self {
            primary_orchestrator_model_id: "conductor".into(),
            per_task_max_usd: 0.0,
            per_goal_max_usd: 0.0,
            per_session_max_usd: 0.0,
            per_agent_call_max_usd: 0.0,
            max_retries_per_role: 2,
            max_wall_clock_minutes: 10,
            turn_max_wall_clock_minutes: 0,
            max_iterations: 100,
            max_workers: 4,
            goal_continuation_enabled: true,
            keep_daemon_resident: false,
        }
    }
}

#[derive(Clone)]
pub struct EngineConfig {
    pub api_key: String,
    pub model_id: String,
    pub base_url: String,
    pub fallback_models: Vec<String>,
}
