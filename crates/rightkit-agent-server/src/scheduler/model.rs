use rightkit_agent_engine_protocol::{default_job_mode, ScheduleRunResourceLimits};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::time::default_time_zone;

#[cfg(test)]
pub use crate::api_schedules::{BotBudget, BrowserPolicy, VerifierBinding};
pub use crate::api_schedules::{BotProfileV1, ScheduleLifecycleState};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleRuntimeMetadata {
    #[serde(default)]
    pub bot_id: Option<String>,
    #[serde(default)]
    pub profile: Option<BotProfileV1>,
    #[serde(default)]
    pub trigger: Option<ScheduleSpec>,
    #[serde(default)]
    pub lifecycle: ScheduleLifecycleState,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScheduleEntry {
    pub id: String,
    pub cron_expr: String,
    pub prompt: String,
    #[serde(default = "default_time_zone")]
    pub time_zone: String,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub toolset: Option<String>,
    #[serde(default)]
    pub execution_backend: Option<String>,
    #[serde(default)]
    pub delivery: Vec<DeliveryTarget>,
    #[serde(default)]
    pub script_allowlist: Vec<String>,
    #[serde(default = "default_job_mode")]
    pub mode: String,
    #[serde(default)]
    pub paused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_recipe_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub recipe_params: BTreeMap<String, String>,
    /// Maximum runtime in seconds for scriptOnly schedules. When set, the
    /// script subprocess is killed if it does not complete within this limit.
    /// Agent-mode schedules are not affected (they manage their own lifecycle).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_runtime_seconds: Option<u64>,
}

/// Schedule definition used by BotProfile triggers and import/export. It is
/// deliberately separate from legacy `ScheduleEntry` so old stores retain a
/// byte-compatible shape while new callers can express all trigger kinds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum ScheduleSpec {
    Once {
        at_unix_ms: i64,
    },
    Cron {
        expression: String,
        time_zone: String,
    },
    Interval {
        every_seconds: u64,
        start_unix_ms: Option<i64>,
    },
    Monitor {
        topic: String,
        filter: Option<String>,
    },
}

impl ScheduleSpec {
    pub fn from_legacy(expression: &str, time_zone: &str) -> Result<Self, String> {
        let trimmed = expression.trim();
        if let Some(value) = trimmed.strip_prefix("once:") {
            return Ok(Self::Once {
                at_unix_ms: value
                    .parse()
                    .map_err(|_| "invalid once timestamp".to_string())?,
            });
        }
        if let Some(value) = trimmed.strip_prefix("interval:") {
            let mut parts = value.split(':');
            let every_seconds = parts
                .next()
                .ok_or_else(|| "interval duration missing".to_string())?
                .parse()
                .map_err(|_| "invalid interval duration".to_string())?;
            let start_unix_ms = parts
                .next()
                .map(|raw| {
                    raw.parse()
                        .map_err(|_| "invalid interval start timestamp".to_string())
                })
                .transpose()?;
            return Ok(Self::Interval {
                every_seconds,
                start_unix_ms,
            });
        }
        if let Some(value) = trimmed.strip_prefix("monitor:") {
            let (topic, filter) = value
                .split_once(':')
                .map_or((value, None), |(t, f)| (t, Some(f.to_string())));
            return Ok(Self::Monitor {
                topic: topic.to_string(),
                filter,
            });
        }
        Ok(Self::Cron {
            expression: trimmed.to_string(),
            time_zone: time_zone.to_string(),
        })
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Once { .. } => Ok(()),
            Self::Cron {
                expression,
                time_zone,
            } => {
                if expression.trim().is_empty() || time_zone.trim().is_empty() {
                    Err("cron schedule requires expression and time zone".into())
                } else {
                    Ok(())
                }
            }
            Self::Interval { every_seconds, .. } if *every_seconds == 0 => {
                Err("interval must be positive".into())
            }
            Self::Interval { .. } => Ok(()),
            Self::Monitor { topic, .. } if topic.trim().is_empty() => {
                Err("monitor topic is empty".into())
            }
            Self::Monitor { .. } => Ok(()),
        }
    }

    pub fn next_at(&self, after_unix_ms: i64) -> Option<i64> {
        match self {
            Self::Once { at_unix_ms } => (*at_unix_ms > after_unix_ms).then_some(*at_unix_ms),
            Self::Interval {
                every_seconds,
                start_unix_ms,
            } => {
                let step = (*every_seconds as i64).saturating_mul(1_000);
                if step <= 0 {
                    return None;
                }
                let start = start_unix_ms.unwrap_or(after_unix_ms.saturating_add(step));
                if start > after_unix_ms {
                    Some(start)
                } else {
                    Some(
                        start.saturating_add(
                            ((after_unix_ms - start) / step + 1).saturating_mul(step),
                        ),
                    )
                }
            }
            Self::Cron {
                expression,
                time_zone,
            } => crate::scheduler::time::next_due_epoch_minute_in_time_zone(
                expression,
                time_zone,
                after_unix_ms.div_euclid(60_000),
                527_040,
            )
            .ok()
            .flatten()
            .map(|m| m.saturating_mul(60_000)),
            Self::Monitor { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleLifecycleReceipt {
    pub schedule_id: String,
    pub occurrence_key: String,
    pub decision: String,
    pub reason: String,
    pub trigger_receipt_id: String,
    pub run_receipt_id: Option<String>,
    pub created_at_unix_ms: i64,
}

impl ScheduleLifecycleReceipt {
    pub fn admitted(schedule_id: &str, occurrence_key: &str, trigger_id: &str, now: i64) -> Self {
        Self {
            schedule_id: schedule_id.into(),
            occurrence_key: occurrence_key.into(),
            decision: "admitted".into(),
            reason: "trigger_fired".into(),
            trigger_receipt_id: trigger_id.into(),
            run_receipt_id: None,
            created_at_unix_ms: now,
        }
    }

    pub fn with_run(mut self, run_id: &str) -> Self {
        self.run_receipt_id = Some(run_id.into());
        self
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryTarget {
    pub kind: String,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub allow_private_network: bool,
}

#[derive(Clone, Debug, Default)]
pub(super) struct ScriptOnlyExecutionAudit {
    pub(super) resolved_execution_backend: Option<String>,
    pub(super) execution_workspace_id: Option<String>,
    pub(super) execution_workspace_path: Option<String>,
    pub(super) resource_limits: Option<ScheduleRunResourceLimits>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct DeliveryOutcome {
    pub(super) status: Option<String>,
    pub(super) errors: Vec<String>,
}

#[derive(Clone, Debug)]
pub(super) struct ScheduleFireOutcome {
    pub(super) status: String,
    pub(super) message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct UtcMinute {
    pub(super) minute: u8,
    pub(super) hour: u8,
    pub(super) day: u8,
    pub(super) month: u8,
    pub(super) weekday: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_legacy_parses_once_prefix() {
        let spec = ScheduleSpec::from_legacy("once:1700000000000", "UTC").unwrap();
        assert_eq!(
            spec,
            ScheduleSpec::Once {
                at_unix_ms: 1_700_000_000_000
            }
        );
    }

    #[test]
    fn from_legacy_rejects_invalid_once_timestamp() {
        assert!(ScheduleSpec::from_legacy("once:not-a-number", "UTC").is_err());
    }

    #[test]
    fn from_legacy_parses_interval_with_start() {
        let spec = ScheduleSpec::from_legacy("interval:60:1000", "UTC").unwrap();
        assert_eq!(
            spec,
            ScheduleSpec::Interval {
                every_seconds: 60,
                start_unix_ms: Some(1000),
            }
        );
    }

    #[test]
    fn from_legacy_parses_interval_without_start() {
        let spec = ScheduleSpec::from_legacy("interval:60", "UTC").unwrap();
        assert_eq!(
            spec,
            ScheduleSpec::Interval {
                every_seconds: 60,
                start_unix_ms: None,
            }
        );
    }

    #[test]
    fn from_legacy_parses_monitor_with_filter() {
        let spec = ScheduleSpec::from_legacy("monitor:topic-a:filter-x", "UTC").unwrap();
        assert_eq!(
            spec,
            ScheduleSpec::Monitor {
                topic: "topic-a".to_string(),
                filter: Some("filter-x".to_string()),
            }
        );
    }

    #[test]
    fn from_legacy_parses_monitor_without_filter() {
        let spec = ScheduleSpec::from_legacy("monitor:topic-a", "UTC").unwrap();
        assert_eq!(
            spec,
            ScheduleSpec::Monitor {
                topic: "topic-a".to_string(),
                filter: None,
            }
        );
    }

    #[test]
    fn from_legacy_falls_back_to_cron() {
        let spec = ScheduleSpec::from_legacy("0 9 * * *", "America/New_York").unwrap();
        assert_eq!(
            spec,
            ScheduleSpec::Cron {
                expression: "0 9 * * *".to_string(),
                time_zone: "America/New_York".to_string(),
            }
        );
    }

    #[test]
    fn validate_once_always_ok() {
        assert!(ScheduleSpec::Once { at_unix_ms: 0 }.validate().is_ok());
    }

    #[test]
    fn validate_cron_rejects_empty_expression() {
        let spec = ScheduleSpec::Cron {
            expression: "   ".to_string(),
            time_zone: "UTC".to_string(),
        };
        assert!(spec.validate().is_err());
    }

    #[test]
    fn validate_cron_rejects_empty_time_zone() {
        let spec = ScheduleSpec::Cron {
            expression: "* * * * *".to_string(),
            time_zone: "  ".to_string(),
        };
        assert!(spec.validate().is_err());
    }

    #[test]
    fn validate_cron_accepts_populated_fields() {
        let spec = ScheduleSpec::Cron {
            expression: "* * * * *".to_string(),
            time_zone: "UTC".to_string(),
        };
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn validate_interval_rejects_zero() {
        let spec = ScheduleSpec::Interval {
            every_seconds: 0,
            start_unix_ms: None,
        };
        assert!(spec.validate().is_err());
    }

    #[test]
    fn validate_interval_accepts_one_second() {
        let spec = ScheduleSpec::Interval {
            every_seconds: 1,
            start_unix_ms: None,
        };
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn validate_monitor_rejects_empty_topic() {
        let spec = ScheduleSpec::Monitor {
            topic: "".to_string(),
            filter: None,
        };
        assert!(spec.validate().is_err());
    }

    #[test]
    fn validate_monitor_accepts_nonempty_topic() {
        let spec = ScheduleSpec::Monitor {
            topic: "topic".to_string(),
            filter: None,
        };
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn next_at_once_future_returns_timestamp() {
        let spec = ScheduleSpec::Once { at_unix_ms: 5000 };
        assert_eq!(spec.next_at(1000), Some(5000));
    }

    #[test]
    fn next_at_once_past_or_equal_returns_none() {
        let spec = ScheduleSpec::Once { at_unix_ms: 1000 };
        assert_eq!(spec.next_at(1000), None);
        assert_eq!(spec.next_at(2000), None);
    }

    #[test]
    fn next_at_interval_with_future_start_returns_start() {
        let spec = ScheduleSpec::Interval {
            every_seconds: 60,
            start_unix_ms: Some(10_000),
        };
        assert_eq!(spec.next_at(1_000), Some(10_000));
    }

    #[test]
    fn next_at_interval_advances_past_after_time() {
        let spec = ScheduleSpec::Interval {
            every_seconds: 60,
            start_unix_ms: Some(0),
        };
        // step = 60_000ms; after 90_000ms the next tick strictly after that
        // point is 120_000ms (start + 2 steps).
        let next = spec.next_at(90_000).unwrap();
        assert_eq!(next, 120_000);
        assert!(next > 90_000);
    }

    #[test]
    fn next_at_interval_defaults_start_to_one_step_after_after_time() {
        let spec = ScheduleSpec::Interval {
            every_seconds: 60,
            start_unix_ms: None,
        };
        assert_eq!(spec.next_at(1_000), Some(61_000));
    }

    #[test]
    fn next_at_monitor_is_never_time_driven() {
        let spec = ScheduleSpec::Monitor {
            topic: "t".to_string(),
            filter: None,
        };
        assert_eq!(spec.next_at(0), None);
    }

    #[test]
    fn lifecycle_receipt_admitted_has_no_run_id_until_attached() {
        let receipt = ScheduleLifecycleReceipt::admitted("sched-1", "occ-1", "trig-1", 42);
        assert_eq!(receipt.decision, "admitted");
        assert!(receipt.run_receipt_id.is_none());
        let with_run = receipt.with_run("run-1");
        assert_eq!(with_run.run_receipt_id, Some("run-1".to_string()));
    }
}
