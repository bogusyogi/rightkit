//! Fine-tuning trajectory capture + export.
//!
//! High-quality agent runs are training data. Each qualifying run (a role that
//! produced output the verifier accepted) is recorded as a `(prompt, output)`
//! trajectory in the evolution DB, and exported as OpenAI-style chat fine-tuning
//! JSONL — one `{"messages":[...]}` object per line — filtered by GDS so only
//! the best runs become training examples. This is the export half of the
//! fine-tuning loop the observation layer already counts readiness for.

use crate::evo_db::EvolutionDb;

#[derive(Clone)]
pub struct TrajectoryStore {
    db: EvolutionDb,
}

#[derive(Debug, Clone)]
pub struct Trajectory {
    pub role: String,
    pub model: String,
    pub prompt: String,
    pub output: String,
    pub gds: f64,
    /// The version of the agent that produced this trajectory, if known.
    pub agent_version: Option<String>,
}

impl TrajectoryStore {
    pub fn new() -> Self {
        Self::open(EvolutionDb::open_in_memory())
    }

    pub fn open(db: EvolutionDb) -> Self {
        Self { db }
    }

    /// Record one run's trajectory. Empty outputs are skipped (nothing to learn).
    pub fn record(&self, t: &Trajectory) {
        if t.output.trim().is_empty() {
            return;
        }
        let _ = self.db.lock().execute(
            "INSERT INTO trajectories (role, model, prompt, output, gds, agent_version, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rightkit_db::rusqlite::params![
                t.role,
                t.model,
                t.prompt,
                t.output,
                t.gds,
                &t.agent_version,
                crate::api::now_iso(),
            ],
        );
    }

    /// Count of stored trajectories (diagnostics/tests).
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.db
            .lock()
            .query_row("SELECT count(*) FROM trajectories", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|n| n as usize)
            .unwrap_or(0)
    }

    /// Export qualifying trajectories (gds ≥ `min_gds`) as OpenAI chat
    /// fine-tuning JSONL — one object per line.
    pub fn export_jsonl(&self, min_gds: f64) -> String {
        let conn = self.db.lock();
        let rows: Vec<(String, String, String, String)> = conn
            .prepare(
                "SELECT role, model, prompt, output FROM trajectories \
                 WHERE gds >= ?1 ORDER BY id",
            )
            .and_then(|mut stmt| {
                stmt.query_map([min_gds], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })
                .map(|rows| rows.flatten().collect())
            })
            .unwrap_or_default();
        rows.iter()
            .map(|(role, model, prompt, output)| jsonl_line(role, model, prompt, output))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Default for TrajectoryStore {
    fn default() -> Self {
        Self::new()
    }
}

/// One OpenAI chat-fine-tuning record. The role/model ride along as metadata so
/// a downstream pipeline can split training sets per (role, model).
fn jsonl_line(role: &str, model: &str, prompt: &str, output: &str) -> String {
    serde_json::json!({
        "messages": [
            { "role": "user", "content": prompt },
            { "role": "assistant", "content": output }
        ],
        "metadata": { "role": role, "model": model }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(role: &str, gds: f64, output: &str) -> Trajectory {
        Trajectory {
            role: role.into(),
            model: "deepseek-v4-pro".into(),
            prompt: "do the thing".into(),
            output: output.into(),
            gds,
            agent_version: None,
        }
    }

    #[test]
    fn records_and_exports_only_qualifying() {
        let s = TrajectoryStore::new();
        s.record(&t("coder", 0.95, "great answer"));
        s.record(&t("coder", 0.40, "weak answer"));
        s.record(&t("coder", 0.90, "good answer"));
        assert_eq!(s.len(), 3);
        let jsonl = s.export_jsonl(0.90);
        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), 2, "only gds>=0.90 exported");
        // Each line is a valid OpenAI chat fine-tune record.
        for line in lines {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["messages"][0]["role"], "user");
            assert_eq!(v["messages"][1]["role"], "assistant");
            assert_eq!(v["metadata"]["role"], "coder");
        }
        assert!(jsonl.contains("great answer") && jsonl.contains("good answer"));
        assert!(!jsonl.contains("weak answer"));
    }

    #[test]
    fn skips_empty_output() {
        let s = TrajectoryStore::new();
        s.record(&t("coder", 0.99, "   "));
        assert_eq!(s.len(), 0);
    }
}
