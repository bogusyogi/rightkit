use rightkit_db::rusqlite;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rightkit_agent_engine_protocol::{TaskNode, TaskState};
use rightkit_agent_engine_tasks::TaskGraph;
use rightkit_db::rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::conductor::Worker;
use crate::handoff_brief::HandoffArchiveConfig;
use crate::orchestrator::{run_graph_with_checkpoint_hook, RunOutcome};
use crate::registry::AgentRegistry;
use crate::router::AgentRouter;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database error: {0}")]
    Db(#[from] rightkit_db::DbError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct GraphCheckpointStore {
    path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ResumeGraphRequest {
    pub run_id: String,
    pub goal: String,
    pub max_concurrent_tasks: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GraphCheckpointSnapshot {
    graph: Vec<TaskNode>,
    completed_nodes: Vec<String>,
    pending_ready: Vec<String>,
    outputs: BTreeMap<String, String>,
}

impl GraphCheckpointStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CheckpointError> {
        let store = Self {
            path: path.as_ref().to_path_buf(),
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn save(
        &self,
        run_id: &str,
        graph: &TaskGraph,
        outputs: &BTreeMap<String, String>,
    ) -> Result<(), CheckpointError> {
        let mut pending_ready: Vec<String> = graph
            .ready_tasks()
            .iter()
            .map(|task| task.id.clone())
            .collect();
        pending_ready.sort();

        let mut completed_nodes: Vec<String> = graph
            .tasks()
            .filter(|task| task.state == TaskState::Done)
            .map(|task| task.id.clone())
            .collect();
        completed_nodes.sort();

        let snapshot = GraphCheckpointSnapshot {
            graph: graph.tasks().cloned().collect(),
            completed_nodes,
            pending_ready,
            outputs: outputs.clone(),
        };

        let graph_json = serde_json::to_string(&snapshot.graph)?;
        let completed_json = serde_json::to_string(&snapshot.completed_nodes)?;
        let pending_ready_json = serde_json::to_string(&snapshot.pending_ready)?;
        let outputs_json = serde_json::to_string(&snapshot.outputs)?;

        self.conn()?.execute(
            "INSERT INTO graph_run_checkpoints
                (run_id, graph_json, completed_json, pending_ready_json, outputs_json, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, unixepoch('subsec') * 1000)
             ON CONFLICT(run_id) DO UPDATE SET
                graph_json = excluded.graph_json,
                completed_json = excluded.completed_json,
                pending_ready_json = excluded.pending_ready_json,
                outputs_json = excluded.outputs_json,
                updated_at_ms = excluded.updated_at_ms",
            params![
                run_id,
                graph_json,
                completed_json,
                pending_ready_json,
                outputs_json
            ],
        )?;
        Ok(())
    }

    pub fn restore_graph(
        &self,
        run_id: &str,
        graph: &mut TaskGraph,
    ) -> Result<(), CheckpointError> {
        let Some(snapshot) = self.load(run_id)? else {
            return Ok(());
        };
        for saved in snapshot.graph {
            if let Some(node) = graph.get_mut(&saved.id) {
                *node = saved;
            }
        }
        graph.recompute_states();
        Ok(())
    }

    pub fn outputs(&self, run_id: &str) -> Result<BTreeMap<String, String>, CheckpointError> {
        Ok(self
            .load(run_id)?
            .map(|snapshot| snapshot.outputs)
            .unwrap_or_default())
    }

    pub fn node_output(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<Option<String>, CheckpointError> {
        Ok(self.outputs(run_id)?.remove(task_id))
    }

    pub fn pending_ready(&self, run_id: &str) -> Result<Vec<String>, CheckpointError> {
        Ok(self
            .load(run_id)?
            .map(|snapshot| snapshot.pending_ready)
            .unwrap_or_default())
    }

    pub fn delete(&self, run_id: &str) -> Result<(), CheckpointError> {
        self.conn()?.execute(
            "DELETE FROM graph_run_checkpoints WHERE run_id = ?1",
            params![run_id],
        )?;
        Ok(())
    }

    fn migrate(&self) -> Result<(), CheckpointError> {
        self.conn()?.execute_batch(
            "CREATE TABLE IF NOT EXISTS graph_run_checkpoints (
                run_id TEXT PRIMARY KEY,
                graph_json TEXT NOT NULL,
                completed_json TEXT NOT NULL,
                pending_ready_json TEXT NOT NULL,
                outputs_json TEXT NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );",
        )?;
        Ok(())
    }

    fn load(&self, run_id: &str) -> Result<Option<GraphCheckpointSnapshot>, CheckpointError> {
        let row: Option<(String, String, String, String)> = self
            .conn()?
            .query_row(
                "SELECT graph_json, completed_json, pending_ready_json, outputs_json
                 FROM graph_run_checkpoints
                 WHERE run_id = ?1",
                params![run_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;

        let Some((graph_json, completed_json, pending_ready_json, outputs_json)) = row else {
            return Ok(None);
        };
        Ok(Some(GraphCheckpointSnapshot {
            graph: serde_json::from_str(&graph_json)?,
            completed_nodes: serde_json::from_str(&completed_json)?,
            pending_ready: serde_json::from_str(&pending_ready_json)?,
            outputs: serde_json::from_str(&outputs_json)?,
        }))
    }

    fn conn(&self) -> Result<Connection, CheckpointError> {
        Ok(rightkit_db::DbOptions::new(&self.path).open_writer()?)
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_graph_resumable(
    run_id: &str,
    goal: &str,
    graph: &mut TaskGraph,
    registry: &AgentRegistry,
    router: &AgentRouter,
    worker: Arc<dyn Worker>,
    max_concurrent_tasks: usize,
    store: &GraphCheckpointStore,
    on_update: impl FnMut(&TaskGraph),
) -> Result<RunOutcome, CheckpointError> {
    run_graph_resumable_with_conductor_and_handoff_archive(
        run_id,
        goal,
        graph,
        registry,
        router,
        worker,
        max_concurrent_tasks,
        store,
        on_update,
        None,
        None,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn run_graph_resumable_with_conductor_and_handoff_archive(
    run_id: &str,
    goal: &str,
    graph: &mut TaskGraph,
    registry: &AgentRegistry,
    router: &AgentRouter,
    worker: Arc<dyn Worker>,
    max_concurrent_tasks: usize,
    store: &GraphCheckpointStore,
    on_update: impl FnMut(&TaskGraph),
    conductor_identity: Option<String>,
    conductor_day: Option<String>,
    handoff_archive: Option<HandoffArchiveConfig>,
) -> Result<RunOutcome, CheckpointError> {
    store.restore_graph(run_id, graph)?;
    let mut outputs = store.outputs(run_id)?;
    let restored_results = outputs
        .iter()
        .filter_map(|(task_id, output)| {
            let node = graph.get(task_id)?;
            (node.state == TaskState::Done).then(|| crate::conductor::WorkerResult {
                task_id: task_id.clone(),
                agent: node.assigned_role.clone(),
                success: true,
                output: output.clone(),
                input_tokens: 0,
                mop_fired: false,
                failure_kind: None,
                agent_version: None,
                rollback_step: None,
                outcome: crate::conductor::WorkerOutcome::Complete,
            })
        })
        .collect();

    let outcome = run_graph_with_checkpoint_hook(
        goal,
        graph,
        registry,
        router,
        worker,
        max_concurrent_tasks,
        on_update,
        conductor_identity,
        conductor_day,
        handoff_archive.as_ref(),
        restored_results,
        |graph, results| {
            for result in results {
                outputs.insert(result.task_id.clone(), result.output.clone());
            }
            store.save(run_id, graph, &outputs)
        },
    )
    .await?;

    Ok(outcome)
}

#[allow(clippy::too_many_arguments)]
pub async fn resume_graph(
    run_id: &str,
    goal: &str,
    graph: &mut TaskGraph,
    registry: &AgentRegistry,
    router: &AgentRouter,
    worker: Arc<dyn Worker>,
    max_concurrent_tasks: usize,
    store: &GraphCheckpointStore,
    on_update: impl FnMut(&TaskGraph),
) -> Result<RunOutcome, CheckpointError> {
    store.restore_graph(run_id, graph)?;
    run_graph_resumable(
        run_id,
        goal,
        graph,
        registry,
        router,
        worker,
        max_concurrent_tasks,
        store,
        on_update,
    )
    .await
}
