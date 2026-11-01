use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ReadinessVerdict {
    Pass,
    Warn,
    Block,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ReadinessState {
    Ready,
    Degraded,
    Unavailable,
    Blocked,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessRow {
    pub id: String,
    pub label: String,
    pub verdict: ReadinessVerdict,
    pub state: ReadinessState,
    pub detail: String,
    pub fix: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessSnapshot {
    pub verdict: ReadinessVerdict,
    pub rows: Vec<ReadinessRow>,
}

impl ReadinessRow {
    pub fn ready(id: &str, label: &str, detail: impl Into<String>) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            verdict: ReadinessVerdict::Pass,
            state: ReadinessState::Ready,
            detail: detail.into(),
            fix: String::new(),
        }
    }

    pub fn blocked(
        id: &str,
        label: &str,
        detail: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            verdict: ReadinessVerdict::Block,
            state: ReadinessState::Blocked,
            detail: detail.into(),
            fix: fix.into(),
        }
    }

    pub fn unavailable(id: &str, label: &str, detail: impl Into<String>) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            verdict: ReadinessVerdict::Warn,
            state: ReadinessState::Unavailable,
            detail: detail.into(),
            fix: String::new(),
        }
    }

    pub fn degraded(id: &str, label: &str, detail: impl Into<String>) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            verdict: ReadinessVerdict::Warn,
            state: ReadinessState::Degraded,
            detail: detail.into(),
            fix: String::new(),
        }
    }

    pub fn warn(id: &str, label: &str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            verdict: ReadinessVerdict::Warn,
            state: ReadinessState::Degraded,
            detail: detail.into(),
            fix: fix.into(),
        }
    }
}
