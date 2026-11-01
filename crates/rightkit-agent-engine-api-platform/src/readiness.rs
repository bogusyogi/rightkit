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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_set_verdict_state_and_fix() {
        let ready = ReadinessRow::ready("db", "Database", "ok");
        assert_eq!(ready.verdict, ReadinessVerdict::Pass);
        assert_eq!(ready.state, ReadinessState::Ready);
        assert!(ready.fix.is_empty());

        let blocked = ReadinessRow::blocked("k", "Key", "missing", "add a key");
        assert_eq!(blocked.verdict, ReadinessVerdict::Block);
        assert_eq!(blocked.state, ReadinessState::Blocked);
        assert_eq!(blocked.fix, "add a key");

        let unavailable = ReadinessRow::unavailable("u", "Util", "offline");
        assert_eq!(unavailable.verdict, ReadinessVerdict::Warn);
        assert_eq!(unavailable.state, ReadinessState::Unavailable);

        let warn = ReadinessRow::warn("w", "Warn", "slow", "retry");
        assert_eq!(warn.verdict, ReadinessVerdict::Warn);
        assert_eq!(warn.state, ReadinessState::Degraded);
        assert_eq!(warn.fix, "retry");
    }

    #[test]
    fn snapshot_serialises_camel_case_enums() {
        let snapshot = ReadinessSnapshot {
            verdict: ReadinessVerdict::Warn,
            rows: vec![ReadinessRow::degraded("d", "Degraded", "partial")],
        };
        let value = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(value["verdict"], "warn");
        assert_eq!(value["rows"][0]["state"], "degraded");
        assert_eq!(value["rows"][0]["id"], "d");
    }
}
