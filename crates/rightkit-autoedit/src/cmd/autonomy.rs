//! Persistent autonomy levels per content type and platform .

use std::collections::VecDeque;
use std::path::Path;
use std::time::Duration;

use crate::rules::ladder::{Final, Ladder, LadderKey, LadderPolicy, Level};
use chrono::{SecondsFormat, Utc};
use rightkit_fs::{FileLock, FsError, LockKind};
use rightkit_video_project::ProjectError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use rightkit_engine_host::env::Env;
use rightkit_engine_host::project::{bad, settings_object, write_json};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    ladder: Ladder,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    recorded_finals: Vec<RecordedFinals>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_override: Option<Override>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordedFinals {
    content_type: String,
    platform: String,
    ids: VecDeque<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Override {
    content_type: String,
    platform: String,
    level: Level,
    by: String,
    set_at: String,
}

fn required<'a>(params: &'a Value, name: &str) -> Result<&'a str, ProjectError> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad(format!("{name} must be a non-empty string")))
}

fn load(path: &Path, policy: LadderPolicy) -> Result<Saved, ProjectError> {
    let mut saved: Saved = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| bad(format!("invalid {}: {e}", path.display())))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Saved {
            ladder: Ladder::new(policy).map_err(|e| bad(e.to_string()))?,
            recorded_finals: Vec::new(),
            last_override: None,
        },
        Err(e) => return Err(ProjectError::io(format!("reading {}", path.display()), e)),
    };
    saved
        .ladder
        .set_policy(policy)
        .map_err(|e| bad(e.to_string()))?;
    Ok(saved)
}

/// `autonomy.status`, `autonomy.record` and `autonomy.override` share a pairing key.
/// Dry-run mutations report the proposed state without creating files or locks.
pub fn handle(method: &str, params: &Value, env: &Env, dry: bool) -> Result<Value, ProjectError> {
    if !matches!(
        method,
        "autonomy.status" | "autonomy.record" | "autonomy.override"
    ) {
        return Err(bad(format!("unknown autonomy method {method}")));
    }
    let key = LadderKey::new(
        required(params, "content_type")?,
        required(params, "platform")?,
    )
    .map_err(|e| bad(e.to_string()))?;
    let result = if method == "autonomy.record" {
        let critic_failed = match params.get("critic_failed") {
            None => false,
            Some(Value::Bool(value)) => *value,
            Some(_) => return Err(bad("critic_failed must be a boolean")),
        };
        Some(match required(params, "result")? {
            "approved" => Final::Approved { critic_failed },
            "rejected" => Final::Rejected,
            _ => return Err(bad("result must be approved or rejected")),
        })
    } else {
        None
    };
    let override_level = if method == "autonomy.override" {
        let level: Level = serde_json::from_value(json!(required(params, "level")?))
            .map_err(|_| bad("level must be reviewed, review_light or autonomous"))?;
        Some((level, required(params, "by")?))
    } else {
        None
    };
    let final_id = if method == "autonomy.record" {
        match params.get("final_id") {
            None | Some(Value::Null) => None,
            Some(_) => Some(required(params, "final_id")?),
        }
    } else {
        None
    };
    let path = env
        .app_root
        .path("autonomy-ladder.json")
        .map_err(|e| bad(e.to_string()))?;
    let mutating = method != "autonomy.status";
    // Keep read-modify-write exclusive across engine processes, including atomic replacement.
    let _lock = if mutating && !dry {
        let lock_path = env
            .app_root
            .path("autonomy-ladder.lock")
            .map_err(|e| bad(e.to_string()))?;
        Some(
            FileLock::try_lock_with(&lock_path, LockKind::Flock, Duration::ZERO).map_err(|e| {
                match e {
                    FsError::Locked { .. } => ProjectError::Locked(lock_path.display().to_string()),
                    other => ProjectError::io(
                        format!("locking {}", lock_path.display()),
                        std::io::Error::other(other),
                    ),
                }
            })?,
        )
    } else {
        None
    };
    let settings = settings_object(
        &env.app_root
            .path("settings.json")
            .map_err(|e| bad(e.to_string()))?,
    )?;
    let policy = match settings.get("autonomy_policy") {
        None | Some(Value::Null) => LadderPolicy::default(),
        Some(value) => LadderPolicy::from_json(&value.to_string())
            .map_err(|e| bad(format!("invalid autonomy_policy: {e}")))?,
    };
    let mut saved = load(&path, policy)?;
    let duplicate = final_id.is_some_and(|id| {
        saved.recorded_finals.iter().any(|recorded| {
            recorded.content_type == key.content_type()
                && recorded.platform == key.platform()
                && recorded.ids.iter().any(|held| held == id)
        })
    });
    if let Some(result) = result.filter(|_| !duplicate) {
        saved.ladder.record(&key, result);
        if let Some(id) = final_id {
            let index = saved
                .recorded_finals
                .iter()
                .position(|recorded| {
                    recorded.content_type == key.content_type()
                        && recorded.platform == key.platform()
                })
                .unwrap_or_else(|| {
                    saved.recorded_finals.push(RecordedFinals {
                        content_type: key.content_type().to_owned(),
                        platform: key.platform().to_owned(),
                        ids: VecDeque::new(),
                    });
                    saved.recorded_finals.len() - 1
                });
            let ids = &mut saved.recorded_finals[index].ids;
            ids.push_back(id.to_owned());
            while ids.len() > 64 {
                ids.pop_front();
            }
        }
    }
    if let Some((level, by)) = override_level {
        saved.ladder.set_level(&key, level);
        saved.last_override = Some(Override {
            content_type: key.content_type().to_owned(),
            platform: key.platform().to_owned(),
            level,
            by: by.to_owned(),
            set_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        });
    }
    let state = saved.ladder.state(&key);
    let mut out = json!({
        "content_type": key.content_type(),
        "platform": key.platform(),
        "level": state.level,
        "streak": state.streak,
        "policy": saved.ladder.policy(),
        "last_override": saved.last_override,
    });
    if duplicate {
        out["duplicate"] = json!(true);
    }
    if dry && mutating {
        return Ok(json!({"dry_run": true, "would": {
            "command": method, "path": path, "state": out,
        }}));
    }
    if mutating && !duplicate {
        write_json(&path, &saved)?;
    }
    Ok(out)
}
