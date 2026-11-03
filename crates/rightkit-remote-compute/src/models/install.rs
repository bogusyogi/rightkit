//! Native model install on top of `rightkit-models`: the shared store owns the
//! manifest-pinned resumable download (ETag/Range, retries), in-process
//! tar.gz/zip member extraction, disk preflight, atomic activation, rollback,
//! uninstall, progress, pause (keep the partial, resume later) and cancel
//! (discard it). This module keeps what is the host's: the
//! `studio.models.install*` wire (install records in the shared settings
//! table, live progress, honest error codes) and adoption of models already
//! installed in the pre-store flat layout.
//!
//! Records persist in the shared settings table (`models.installs`) so a
//! paused or crashed install resumes honestly; the store keeps `.part`
//! files in its per-revision staging directory until cancel discards them.
//!
//! Bundled sd.cpp profiles do not come through [`Target`]: they run through the
//! local-image installer (`super::sdcpp`), which shares this module's session,
//! record and pause/cancel plumbing via [`launch`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use rightkit_models::{Event, InstallOptions, ModelSpec, Store};
use serde_json::{json, Map, Value};

use rightkit_local_gen::model_store;
use rightkit_media_gen::{store, Context, GenError, GenResult};

const INSTALLS_KEY: &str = "models.installs";

/// What installing a manifest entry means: a one-revision store spec plus the
/// pre-store locations its files may already occupy.
#[derive(Clone)]
pub struct Target {
    pub store_root: PathBuf,
    pub spec: ModelSpec,
    /// `(path inside the revision, pre-store location)` for every file of the
    /// revision on this platform, including ones an install may skip.
    pub legacy: Vec<(String, PathBuf)>,
    /// Pre-store flat directory (removed when empty after an uninstall).
    pub legacy_dir: Option<PathBuf>,
    /// `(kind, dir)` of the shared-runtime root the spec's `shared` files use.
    pub shared_root: Option<(String, PathBuf)>,
}

impl Target {
    fn store(&self) -> Store {
        let store = Store::new(&self.store_root, model_store::STORE_APP);
        let store = match &self.legacy_dir {
            Some(dir) => model_store::with_legacy_alias(store, &self.spec.id, dir),
            None => store,
        };
        match &self.shared_root {
            Some((kind, dir)) => store
                .with_shared_root(kind, dir)
                .expect("valid shared-runtime kind"),
            None => store,
        }
    }
    fn revision(&self) -> &str {
        self.spec
            .revisions
            .last()
            .map(|r| r.revision.as_str())
            .unwrap_or_default()
    }
    fn quant(&self) -> Option<String> {
        self.spec.revisions.last().and_then(|r| r.quant.clone())
    }
}

// ---------- live sessions ------------------------------------------------------

/// In-process install state: progress + pause/cancel flags. Persisted records keep
/// durable truth; this map only tracks the currently-downloading session so a
/// second command call can report progress or cancel.
pub struct Session {
    pub progress: Mutex<Value>,
    pub pause: Arc<AtomicBool>,
    pub cancel: Arc<AtomicBool>,
}

/// What an install's record shows before it runs.
pub struct Plan {
    pub revision: String,
    pub dest: PathBuf,
    pub total: u64,
    /// The record's `files` list (`[]` when the installer reports per-file state elsewhere).
    pub files: Value,
}

fn sessions() -> &'static Mutex<HashMap<String, Arc<Session>>> {
    static S: OnceLock<Mutex<HashMap<String, Arc<Session>>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Live progress of a running install in this process.
pub fn live_progress(id: &str) -> Option<Value> {
    sessions()
        .lock()
        .unwrap()
        .get(id)
        .map(|s| s.progress.lock().unwrap().clone())
}

// ---------- persistence ----------------------------------------------------------

fn load_installs(ctx: &Context) -> GenResult<Value> {
    let conn = store::open(ctx)?;
    Ok(conn
        .read(|c| {
            store::settings()
                .get(c, INSTALLS_KEY)
                .map_err(store::settings_err)
        })
        .map_err(store::je)?
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({})))
}

/// The persisted install record for `id` (`Null` when none).
pub fn install_record(ctx: &Context, id: &str) -> Value {
    load_installs(ctx)
        .map(|i| i[id].clone())
        .unwrap_or(Value::Null)
}

fn save_installs(ctx: &Context, installs: &Value) -> GenResult<()> {
    let conn = store::open(ctx)?;
    conn.write(|c| {
        store::settings()
            .set(c, INSTALLS_KEY, &installs.to_string())
            .map_err(store::settings_err)
    })
    .map_err(store::je)
}

fn record(ctx: &Context, id: &str, update: impl FnOnce(&mut Value)) -> GenResult<Value> {
    let mut installs = load_installs(ctx)?;
    let mut rec = installs[id].clone();
    if !rec.is_object() {
        rec = json!({"id": id, "state": "new", "files": [], "events": []});
    }
    update(&mut rec);
    rec["updated_at"] = json!(store::now());
    installs[id] = rec.clone();
    save_installs(ctx, &installs)?;
    Ok(rec)
}

fn event(rec: &mut Value, action: &str, detail: Value) {
    let e = json!({"at": store::now(), "action": action, "detail": detail});
    if let Some(events) = rec["events"].as_array_mut() {
        events.push(e);
    } else {
        rec["events"] = json!([e]);
    }
}

fn gen_error(e: &rightkit_models::Error) -> GenError {
    GenError::new(&model_store::error_code(e), e.message.clone())
}

// ---------- command handlers -------------------------------------------------------

/// Start an install in a background thread. Returns immediately with
/// `{id, state:"installing", total_bytes}` — poll `install.status`.
pub fn start(ctx: &Context, id: &str, target: Target) -> GenResult<Value> {
    let platform = rightkit_models::Platform::current();
    let rev =
        target.spec.revisions.last().ok_or_else(|| {
            GenError::new("invalid_input", format!("{id}: manifest has no revision"))
        })?;
    let files: Vec<(String, Option<u64>, Option<String>)> = rev
        .files_for(&platform)
        .map(|f| (f.path.clone(), f.bytes, f.sha256.clone()))
        .collect();
    if files.is_empty() {
        return Err(GenError::new(
            "no_files",
            format!("{id}: no files for this platform"),
        ));
    }
    let total: u64 = files.iter().filter_map(|f| f.1).sum();
    let plan = Plan {
        revision: rev.revision.clone(),
        dest: target.store_root.join(id),
        total,
        files: json!(files
            .iter()
            .map(|(p, b, s)| json!({"name": p, "bytes": b, "sha256": s, "state": "pending"}))
            .collect::<Vec<_>>()),
    };
    launch(ctx, id, plan, move |ctx, id, sess| {
        run_install(ctx, id, &target, sess)
    })
}

/// Starts `run` in a background install thread for `id`, recording `plan` as the install's
/// record. `run` gets the session (progress sink and pause/cancel flags) and returns the
/// installed directory. Returns at once with `{id, state:"installing", total_bytes, dest}`.
pub fn launch(
    ctx: &Context,
    id: &str,
    plan: Plan,
    run: impl FnOnce(&Context, &str, &Session) -> GenResult<PathBuf> + Send + 'static,
) -> GenResult<Value> {
    if let Some(sess) = sessions().lock().unwrap().get(id) {
        if !sess.cancel.load(Ordering::Relaxed) && !sess.pause.load(Ordering::Relaxed) {
            let p = sess.progress.lock().unwrap().clone();
            return Ok(json!({"id": id, "state": "installing", "progress": p}));
        }
    }

    let Plan {
        revision,
        dest,
        total,
        files,
    } = plan;
    record(ctx, id, |r| {
        r["state"] = json!("installing");
        r["revision"] = json!(revision);
        r["dest"] = json!(dest);
        r["files"] = files;
        event(
            r,
            "install_start",
            json!({"total_bytes": total, "revision": revision}),
        );
    })?;

    let sess = Arc::new(Session {
        progress: Mutex::new(json!({"bytes_done": 0, "bytes_total": total, "file": null})),
        pause: Arc::new(AtomicBool::new(false)),
        cancel: Arc::new(AtomicBool::new(false)),
    });
    sessions()
        .lock()
        .unwrap()
        .insert(id.to_string(), Arc::clone(&sess));

    let data_dir = ctx.data_dir.clone();
    let id_owned = id.to_string();
    std::thread::Builder::new()
        .name(format!("model-install-{id}"))
        .spawn(move || {
            let ctx = Context::new(data_dir);
            let result = run(&ctx, &id_owned, &*sess);
            match result {
                Ok(dir) => {
                    let _ = record(&ctx, &id_owned, |r| {
                        r["state"] = json!("installed");
                        event(r, "install_done", json!({"dest": dir}));
                    });
                }
                Err(e) => {
                    let state = match e.code.as_str() {
                        "cancelled" => "cancelled",
                        "paused" => "paused",
                        _ => "failed",
                    };
                    let _ = record(&ctx, &id_owned, |r| {
                        r["state"] = json!(state);
                        event(
                            r,
                            "install_end",
                            json!({"code": e.code, "message": e.message}),
                        );
                    });
                }
            }
            sessions().lock().unwrap().remove(&id_owned);
        })
        .map_err(|e| GenError::new("io", format!("spawn install thread: {e}")))?;

    Ok(json!({"id": id, "state": "installing", "total_bytes": total, "dest": dest}))
}

fn run_install(ctx: &Context, id: &str, target: &Target, sess: &Session) -> GenResult<PathBuf> {
    let store = target.store();
    let opts = InstallOptions {
        revision: Some(target.revision().to_string()),
        quant: target.quant(),
        pause: Arc::clone(&sess.pause),
        cancel: Arc::clone(&sess.cancel),
        ..InstallOptions::default()
    };
    // Files already installed in the flat layout are adopted (verified against
    // the manifest pins) instead of being downloaded again.
    let legacy: HashMap<&str, &PathBuf> =
        target.legacy.iter().map(|(p, l)| (p.as_str(), l)).collect();
    let outcome = store.adopt_then_install(
        &target.spec,
        &opts,
        |f| legacy.get(f.path.as_str()).map(|p| (*p).clone()),
        |ev| match ev {
            Event::Progress { path, overall_done, overall_total, .. } => {
                *sess.progress.lock().unwrap() =
                    json!({"bytes_done": overall_done, "bytes_total": overall_total, "file": path});
            }
            Event::FileVerified { path, .. } => {
                let _ = record(ctx, id, |r| {
                    for rf in r["files"].as_array_mut().into_iter().flatten() {
                        if rf["name"].as_str() == Some(path.as_str()) {
                            rf["state"] = json!("installed");
                        }
                    }
                });
            }
            Event::Preflight { needed_bytes, free_bytes } => {
                let _ = record(ctx, id, |r| event(r, "preflight", json!({"needed_bytes": needed_bytes, "free_bytes": free_bytes})));
            }
            _ => {}
        },
    )
    .map_err(|e| gen_error(&e))?;
    Ok(outcome.dir)
}

/// `{id?}` → persisted install records + live progress for running installs.
pub fn status(ctx: &Context, id: Option<&str>) -> GenResult<Value> {
    let installs = load_installs(ctx)?;
    let live: Map<String, Value> = sessions()
        .lock()
        .unwrap()
        .iter()
        .map(|(k, s)| (k.clone(), s.progress.lock().unwrap().clone()))
        .collect();
    match id {
        Some(id) => {
            if installs[id].is_null() && !live.contains_key(id) {
                return Err(GenError::new(
                    "install_not_found",
                    format!("no install record for {id}"),
                ));
            }
            Ok(json!({"install": installs[id], "progress": live.get(id)}))
        }
        None => Ok(json!({"installs": installs, "progress": live})),
    }
}

/// Pause a running install: the store keeps the finished files and `.part`s;
/// `install` again resumes (HTTP range). Without a live session the persisted
/// record is reported as is.
pub fn pause(ctx: &Context, id: &str) -> GenResult<Value> {
    let live = sessions()
        .lock()
        .unwrap()
        .get(id)
        .map(|s| s.pause.store(true, Ordering::Relaxed))
        .is_some();
    let installs = load_installs(ctx)?;
    if !live && installs[id].is_null() {
        return Err(GenError::new(
            "install_not_found",
            format!("no install record for {id}"),
        ));
    }
    let rec = record(ctx, id, |r| {
        if live && r["state"] == "installing" {
            r["state"] = json!("pausing");
        }
        event(r, "pause", Value::Null);
    })?;
    Ok(json!({"id": id, "state": rec["state"]}))
}

/// Cancel an install and discard its partial download: a running one stops
/// and the installer clears its staging; a paused one has its staging discarded
/// through `discard`, which runs only when no session is live.
pub fn cancel(
    ctx: &Context,
    id: &str,
    discard: impl FnOnce() -> GenResult<bool>,
) -> GenResult<Value> {
    let live = sessions()
        .lock()
        .unwrap()
        .get(id)
        .map(|s| s.cancel.store(true, Ordering::Relaxed))
        .is_some();
    let installs = load_installs(ctx)?;
    if !live && installs[id].is_null() {
        return Err(GenError::new(
            "install_not_found",
            format!("no install record for {id}"),
        ));
    }
    let discarded = if live { false } else { discard()? };
    let rec = record(ctx, id, |r| {
        if live && r["state"] == "installing" {
            r["state"] = json!("cancelling");
        } else if discarded || r["state"] == "paused" {
            r["state"] = json!("cancelled");
        }
        event(r, "cancel", json!({"discarded": discarded}));
    })?;
    Ok(json!({"id": id, "state": rec["state"]}))
}

/// Discards the partial downloads a paused install left in a target's store staging.
/// Returns whether there were any.
pub fn discard_partial(id: &str, target: &Target) -> GenResult<bool> {
    let store = target.store();
    let mut discarded = false;
    for p in store.paused(id).map_err(|e| gen_error(&e))? {
        store
            .discard_staging(id, &p.key)
            .map_err(|e| gen_error(&e))?;
        discarded = true;
    }
    Ok(discarded)
}

/// Removes an installed model's files: the store entry (every revision) and any
/// files still in the pre-store layout. Returns the names of the flat files removed.
pub fn remove_target(id: &str, target: &Target) -> GenResult<Vec<String>> {
    let store = target.store();
    store.uninstall(id).map_err(|e| gen_error(&e))?;
    let mut removed = Vec::new();
    for (rel, p) in &target.legacy {
        if p.is_file() {
            std::fs::remove_file(p)
                .map_err(|e| GenError::new("io", format!("remove {}: {e}", p.display())))?;
            removed.push(rel.clone());
        }
        let mut part = p.clone().into_os_string();
        part.push(".part");
        let _ = std::fs::remove_file(part);
    }
    if let Some(d) = &target.legacy_dir {
        let _ = std::fs::remove_dir(d);
    }
    Ok(removed)
}

/// Remove an installed model: `remove` deletes its files (deleting model files is
/// the explicitly requested action of `install.uninstall`). Refused while an
/// install session for `id` is live.
pub fn uninstall(
    ctx: &Context,
    id: &str,
    remove: impl FnOnce() -> GenResult<Vec<String>>,
) -> GenResult<Value> {
    if sessions().lock().unwrap().contains_key(id) {
        return Err(GenError::new(
            "busy",
            "install in progress — pause or cancel it first",
        ));
    }
    let removed = remove()?;
    record(ctx, id, |r| {
        r["state"] = json!("uninstalled");
        event(r, "uninstall", json!({"removed": removed}));
    })?;
    Ok(json!({"id": id, "state": "uninstalled", "removed": removed}))
}

/// Re-point the store's active revision at an earlier one (default: the most
/// recent verified earlier revision). The runtime reads the active revision's
/// folder, so a rollback is real state, not a label.
pub fn rollback(
    ctx: &Context,
    id: &str,
    target: &Target,
    revision: Option<&str>,
) -> GenResult<Value> {
    let store = target.store();
    let state = store.state(id).map_err(|e| gen_error(&e))?;
    let want = match revision {
        Some(rev) => Some(
            state
                .revisions
                .iter()
                .find(|(k, r)| r.revision == rev || k.as_str() == rev)
                .map(|(k, _)| k.clone())
                .ok_or_else(|| {
                    GenError::new("no_rollback", format!("revision {rev} is not installed"))
                })?,
        ),
        None => None,
    };
    let from = state.active.clone().unwrap_or_default();
    let key = store
        .rollback(id, want.as_deref())
        .map_err(|e| gen_error(&e))?;
    let revision = store
        .state(id)
        .ok()
        .and_then(|s| s.revisions.get(&key).map(|r| r.revision.clone()))
        .unwrap_or(key.clone());
    let rec = record(ctx, id, |r| {
        event(r, "rollback", json!({"from": from, "to": key}));
        r["revision"] = json!(revision);
        r["state"] = json!("installed");
    })?;
    Ok(json!({"id": id, "revision": rec["revision"], "state": "installed"}))
}
