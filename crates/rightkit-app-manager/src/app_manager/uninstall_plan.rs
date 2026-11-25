//! Reusable app-uninstall plans kept on disk: a plan is written first (it moves nothing), and a
//! later apply claims it, re-checks it against the live system and runs it exactly once.
//!
//! This follows the cleanup plan conventions of `rightkit-disk-cleanup`: plans live in the host's
//! state folder (here `uninstall-plans/`), an id is minted from the Unix milliseconds
//! (`uninstall-<digits>`), a plan expires after `cleanup_scan::PLAN_EXPIRY_SECS`, and applying
//! claims the plan (an exclusive rename to `<plan>.claimed-<run id>.json`) before any effect and
//! keeps the claim record, with the outcome written into it. The effect itself is
//! [`super::uninstall_planned`], restricted to the plan's paths whose recorded identity still
//! matches the live system.
//!
//! The host owns the command line and its wording of what to run next; the commands named in the
//! refusals are [`PlanWording`].

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rightkit_disk_cleanup::cleanup_scan as cs;
use serde_json::{Value, json};

use super::{PlannedApp, PlannedItem, PlannedOutcome};

/// How many claimed plan records are kept beside the plans.
const CLAIMED_PLANS_KEPT: usize = 200;

/// The host's command names, quoted in what a refusal tells the person to do next.
pub struct PlanWording<'a> {
    /// Makes a plan (`pulse uninstall-plan`).
    pub plan_command: &'a str,
    /// Lists the apps (`pulse apps list`).
    pub list_command: &'a str,
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// `uninstall-<digits>` only; a path, separator or anything the plan command did not mint is
/// refused. Returns the digits.
pub fn plan_digits<'a>(id: &'a str, wording: &PlanWording<'_>) -> Result<&'a str, String> {
    id.strip_prefix("uninstall-")
        .filter(|rest| !rest.is_empty() && rest.len() <= 20)
        .filter(|rest| rest.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(|| format!("not a plan id issued by `{}`: {id}", wording.plan_command))
}

/// The plan file of `id` below the state folder.
pub fn plan_file(state: &Path, id: &str) -> PathBuf {
    state.join("uninstall-plans").join(format!("{id}.json"))
}

/// The kept claim record of `id` claimed by `run`.
pub fn claimed_file(state: &Path, id: &str, run: &str) -> PathBuf {
    state
        .join("uninstall-plans")
        .join(format!("{id}.claimed-{run}.json"))
}

/// Refuse a plan older than the expiry. `plan_digits` has checked the id; its digits are the Unix
/// milliseconds the plan was minted at.
pub fn ensure_fresh(id: &str, wording: &PlanWording<'_>) -> Result<(), String> {
    let made: u128 = plan_digits(id, wording)?
        .parse()
        .map_err(|_| format!("not a plan id issued by `{}`: {id}", wording.plan_command))?;
    let age_ms = unix_millis().saturating_sub(made);
    if age_ms > u128::from(cs::PLAN_EXPIRY_SECS) * 1000 {
        return Err(format!(
            "this plan is more than {} hours old, so it was not applied; \
             make a fresh one with `{}`",
            cs::PLAN_EXPIRY_SECS / 3600,
            wording.plan_command
        ));
    }
    Ok(())
}

/// Keep the newest claimed plan records and drop older ones (best effort).
fn prune_claimed(state: &Path) {
    let Ok(entries) = fs::read_dir(state.join("uninstall-plans")) else {
        return;
    };
    let mut claimed: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().contains(".claimed-"))
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect();
    claimed.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    for (_, path) in claimed.into_iter().skip(CLAIMED_PLANS_KEPT) {
        let _ = fs::remove_file(path);
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    rightkit_fs::atomic_write_with_options(
        path,
        rightkit_fs::AtomicWriteOptions {
            durable: false,
            privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
            ..Default::default()
        },
        |out| std::io::Write::write_all(out, bytes),
    )
    .map_err(|e| e.to_string())
}

/// Write what happened into the kept claim record (best effort), so a second apply can say.
pub fn record_result(claimed: &Path, result: Value) {
    let Ok(text) = fs::read_to_string(claimed) else {
        return;
    };
    let Ok(mut body) = serde_json::from_str::<Value>(&text) else {
        return;
    };
    body["result"] = result;
    if let Ok(bytes) = serde_json::to_vec_pretty(&body) {
        let _ = write_private(claimed, &bytes);
    }
}

/// What to tell a second apply of a plan that was already claimed by `run`.
pub fn already_applied(
    state: &Path,
    id: &str,
    run: Option<String>,
    wording: &PlanWording<'_>,
) -> String {
    let Some(run) = run else {
        return format!(
            "this plan was already claimed by another apply that left no run id; \
             see `{}` and the apps activity log",
            wording.list_command
        );
    };
    let record = fs::read_to_string(claimed_file(state, id, &run))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    match record.as_ref().and_then(|body| body.get("result")) {
        Some(result) => format!("this plan was already applied (run {run}): {result}"),
        None => format!(
            "this plan was claimed by run {run}, which recorded no result (it may have been \
             interrupted); check `{}`, then make a fresh plan with `{}`",
            wording.list_command, wording.plan_command
        ),
    }
}

/// Saves a plan for `app` and `items` made for the account `home`, and returns its id. Writes
/// only the plan file (owner-only) in the state folder; no user file is touched. The id is minted
/// from the Unix milliseconds, bumped until no plan has it.
pub fn save_plan(
    state: &Path,
    home: &Path,
    app: &PlannedApp,
    items: &[PlannedItem],
) -> Result<String, String> {
    let mut made = unix_millis();
    let (id, file) = loop {
        let id = format!("uninstall-{made}");
        let file = plan_file(state, &id);
        if !file.exists() {
            break (id, file);
        }
        made += 1;
    };
    let body = json!({
        "schema_version": 1,
        "id": id,
        "home": home.to_string_lossy(),
        "app": app,
        "items": items,
    });
    fs::create_dir_all(file.parent().expect("plan dir")).map_err(|e| e.to_string())?;
    let plan = serde_json::to_vec_pretty(&body).map_err(|e| e.to_string())?;
    write_private(&file, &plan)?;
    Ok(id)
}

/// Reads plan `id` back: the app and the items. A plan that is gone says whether it was applied;
/// one for another home folder, another schema or a damaged file is refused.
pub fn load_plan(
    state: &Path,
    id: &str,
    home: &Path,
    wording: &PlanWording<'_>,
) -> Result<(PlannedApp, Vec<PlannedItem>), String> {
    plan_digits(id, wording)?;
    let file = plan_file(state, id);
    let text = fs::read_to_string(&file).map_err(|_| match cs::plan_claim_run(&file) {
        Some(run) => already_applied(state, id, Some(run), wording),
        None => format!("no such plan (it may be applied already): {id}"),
    })?;
    let body: Value = serde_json::from_str(&text).map_err(|_| "the plan file is damaged")?;
    if body["schema_version"] != 1 || body["id"] != id {
        return Err("the plan file is not a plan this version issued".into());
    }
    if body["home"].as_str() != Some(home.to_string_lossy().as_ref()) {
        return Err("this plan was made for a different home folder".into());
    }
    let damaged = |_| "the plan file is damaged".to_string();
    let app: PlannedApp = serde_json::from_value(body["app"].clone()).map_err(damaged)?;
    let items: Vec<PlannedItem> = serde_json::from_value(body["items"].clone()).map_err(damaged)?;
    Ok((app, items))
}

/// A claimed plan: the run that holds it and its kept record.
pub struct Claim {
    pub run_id: String,
    pub record: PathBuf,
}

/// Claim plan `id` before any effect: only one apply can take it. `Err` says the plan was already
/// claimed (and by what outcome). Older claim records beyond the newest 200 are pruned.
pub fn claim(state: &Path, id: &str, wording: &PlanWording<'_>) -> Result<Claim, String> {
    let run_id = cs::mint_run_id();
    let claimed = cs::claim_plan(&plan_file(state, id), &run_id)?;
    if let cs::PlanClaim::AlreadyApplied(run) = claimed {
        return Err(already_applied(state, id, run, wording));
    }
    prune_claimed(state);
    Ok(Claim {
        record: claimed_file(state, id, &run_id),
        run_id,
    })
}

/// The report of an applied plan and whether the app itself failed to go: it is in `failed`, or it
/// was attempted and is not in `moved`.
pub fn applied_report(id: &str, app: &PlannedApp, outcome: &PlannedOutcome) -> (Value, bool) {
    let result = &outcome.result;
    let app_moved = result.moved.iter().any(|m| m.path == app.path);
    let app_failed = result.failed.iter().any(|f| f.path == app.path)
        || (!app_moved && outcome.attempted.contains(&app.path));
    let value = json!({
        "dry_run": false,
        "plan_id": id,
        "app": app.name,
        "moved": result.moved,
        "failed": result.failed,
        "skipped": outcome.skipped,
        "bytes_moved": result.bytes_moved,
        "note": "Moved to the Trash: up to bytes_moved on disk. Space is not freed until the Trash is emptied.",
        "activity_id": result.activity_id,
    });
    (value, app_failed)
}

/// The record written into a claim when the apply was refused before anything moved.
pub fn refused_report(error: &str) -> Value {
    json!({"outcome": "refused", "moved": [], "error": error})
}
