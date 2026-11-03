use crate::error::Result;
use crate::Storage;
use rightkit_agent_engine_protocol::event::GoalStepV1;
use rightkit_db::rusqlite;
use rightkit_db::rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

/// Lifecycle status of a persistent goal (F2). `Achieved` is reachable ONLY via
/// the jury's `arbitrate()` verdict — never self-declared by the continuation
/// loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GoalStatus {
    Active,
    Achieved,
    Blocked,
    Unmet,
    Paused,
    BudgetLimited,
    UsageLimited,
}

impl GoalStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Achieved => "achieved",
            Self::Blocked => "blocked",
            Self::Unmet => "unmet",
            Self::Paused => "paused",
            Self::BudgetLimited => "budget-limited",
            Self::UsageLimited => "usage-limited",
        }
    }

    pub fn from_db(s: &str) -> Self {
        match s {
            "achieved" => Self::Achieved,
            "blocked" => Self::Blocked,
            "unmet" => Self::Unmet,
            "paused" => Self::Paused,
            "budget-limited" => Self::BudgetLimited,
            "usage-limited" => Self::UsageLimited,
            _ => Self::Active,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GoalRow {
    pub id: String,
    pub session_id: String,
    pub text: String,
    pub status: GoalStatus,
    pub budget_usd: f64,
    pub budget_tokens: i64,
    pub tokens_used: i64,
    pub continuation_count: i64,
    pub no_progress_count: i64,
    pub started_at: String,
    pub closed_at: Option<String>,
    pub dismissed_at: Option<String>,
    pub decomposition_state: String,
    pub plan_revision: i64,
    pub now: Option<String>,
    pub next: Option<String>,
    pub steps: Vec<GoalStepV1>,
    pub verification_state: String,
}

impl Storage {
    pub fn upsert_goal(&self, g: &GoalRow) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO goals(id,session_id,text,status,budget_usd,budget_tokens,tokens_used,continuation_count,no_progress_count,started_at,closed_at,dismissed_at,decomposition_state,plan_revision,now_text,next_text,plan_json,verification_state) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
            rusqlite::params![
                g.id,
                g.session_id,
                g.text,
                g.status.as_str(),
                g.budget_usd,
                g.budget_tokens,
                g.tokens_used,
                g.continuation_count,
                g.no_progress_count,
                g.started_at,
                g.closed_at,
                g.dismissed_at,
                g.decomposition_state,
                g.plan_revision,
                g.now,
                g.next,
                serde_json::to_string(&g.steps).unwrap_or_else(|_| "[]".into()),
                g.verification_state,
            ],
        )?;
        Ok(())
    }

    pub fn get_goal(&self, id: &str) -> Result<Option<GoalRow>> {
        let row = self
            .conn
            .query_row(
                "SELECT id,session_id,text,status,budget_usd,budget_tokens,tokens_used,continuation_count,no_progress_count,started_at,closed_at,dismissed_at,decomposition_state,plan_revision,now_text,next_text,plan_json,verification_state \
                 FROM goals WHERE id=?1",
                [id],
                Self::map_goal,
            )
            .optional()?;
        Ok(row)
    }

    /// The active (most recently started, not-yet-closed) goal for a session.
    pub fn active_goal_for_session(&self, session_id: &str) -> Result<Option<GoalRow>> {
        let row = self
            .conn
            .query_row(
                "SELECT id,session_id,text,status,budget_usd,budget_tokens,tokens_used,continuation_count,no_progress_count,started_at,closed_at,dismissed_at,decomposition_state,plan_revision,now_text,next_text,plan_json,verification_state \
                 FROM goals WHERE session_id=?1 AND closed_at IS NULL AND dismissed_at IS NULL ORDER BY started_at DESC LIMIT 1",
                [session_id],
                Self::map_goal,
            )
            .optional()?;
        Ok(row)
    }

    pub fn newest_displayable_goal_for_session(&self, session_id: &str) -> Result<Option<GoalRow>> {
        self.conn.query_row(
            "SELECT id,session_id,text,status,budget_usd,budget_tokens,tokens_used,continuation_count,no_progress_count,started_at,closed_at,dismissed_at,decomposition_state,plan_revision,now_text,next_text,plan_json,verification_state FROM goals WHERE session_id=?1 AND dismissed_at IS NULL ORDER BY started_at DESC LIMIT 1",
            [session_id], Self::map_goal,
        ).optional().map_err(Into::into)
    }

    pub fn dismiss_goal(&self, id: &str, dismissed_at: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE goals SET dismissed_at=?2 WHERE id=?1",
            rusqlite::params![id, dismissed_at],
        )?;
        Ok(())
    }

    pub fn update_goal_status(
        &self,
        id: &str,
        status: GoalStatus,
        closed_at: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE goals SET status=?2, closed_at=?3 WHERE id=?1",
            rusqlite::params![id, status.as_str(), closed_at],
        )?;
        Ok(())
    }

    pub fn add_goal_tokens(&self, id: &str, tokens_used: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE goals SET tokens_used=?2 WHERE id=?1",
            rusqlite::params![id, tokens_used],
        )?;
        Ok(())
    }

    fn map_goal(row: &rusqlite::Row) -> rusqlite::Result<GoalRow> {
        Ok(GoalRow {
            id: row.get(0)?,
            session_id: row.get(1)?,
            text: row.get(2)?,
            status: GoalStatus::from_db(&row.get::<_, String>(3)?),
            budget_usd: row.get(4)?,
            budget_tokens: row.get(5)?,
            tokens_used: row.get(6)?,
            continuation_count: row.get(7)?,
            no_progress_count: row.get(8)?,
            started_at: row.get(9)?,
            closed_at: row.get(10)?,
            dismissed_at: row.get(11)?,
            decomposition_state: row.get(12)?,
            plan_revision: row.get(13)?,
            now: row.get(14)?,
            next: row.get(15)?,
            steps: serde_json::from_str(&row.get::<_, String>(16)?).unwrap_or_default(),
            verification_state: row.get(17)?,
        })
    }
}
