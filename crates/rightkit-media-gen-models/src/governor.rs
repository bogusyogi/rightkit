//! Memory-aware scheduling for local heavy models (sd.cpp image, QTS voice,
//! ONNX music/lipsync, whisper). Before a heavy job starts, the governor
//! checks *free* system memory against the model's measured-or-estimated peak
//! and either admits it, unloads idle loaded models to make room, or returns
//! an honest `queued` state with what is blocking — never silently OOMs the
//! host.
//!
//! Other runtimes register a `LoadedRuntime` so the governor can see and
//! unload their resident models: `register_runtime(Box<dyn LoadedRuntime>)`.
//! Wiring: the local voice runtime registers its warm worker, `local_image`/sd-server
//! registers its server lifecycle (owners apply the small call-site patch).
//!
//! Commands:
//! - `studio.models.loaded`            → resident models + memory summary
//! - `studio.models.governor.status`   → loaded + grants + queue
//! - `studio.models.governor.acquire`  `{job, need_gb?, model_id?, unload_idle?}`
//! - `studio.models.governor.release`  `{grant}`
//! - `studio.models.governor.unload`   `{runtime, model_id?}` — explicit unload

use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use rightkit_media_gen::providers::qts;
use rightkit_media_gen::{store, Context, GenError, GenResult};

const GOVERNOR_KEY: &str = "models.governor";
/// Headroom kept for the OS/other apps even when a model would nominally fit.
const RESERVE_GB: f64 = 1.5;

// ---------- runtime trait ----------------------------------------------------

/// One loaded model a runtime reports.
#[derive(Debug, Clone)]
pub struct LoadedModel {
    pub runtime: &'static str,
    pub model_id: String,
    /// Resident size in GB when measurable; `None` = unknown.
    pub rss_gb: Option<f64>,
    /// True when the runtime knows this model is not mid-job (safe to unload).
    pub idle: bool,
}

/// An engine that keeps models resident (QTS warm worker, sd-server, ONNX
/// sessions, whisper). Register via `register_runtime`.
pub trait LoadedRuntime: Send + Sync {
    fn name(&self) -> &'static str;
    /// Models currently resident, newest first.
    fn loaded(&self, ctx: &Context) -> Vec<LoadedModel>;
    /// Unload one model (or the runtime's only resident model). Must not
    /// fabricate success — return Err when the unload cannot be performed.
    fn unload(&self, ctx: &Context, model_id: &str) -> GenResult<()>;
    /// Estimated peak GB for `model_id` when the runtime has a better number
    /// than the manifest requirements.
    fn peak_gb(&self, _model_id: &str) -> Option<f64> {
        None
    }
}

fn runtimes() -> &'static Mutex<Vec<Box<dyn LoadedRuntime>>> {
    static R: OnceLock<Mutex<Vec<Box<dyn LoadedRuntime>>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Vec::new()))
}

/// Owner lanes register their resident-model runtime here (one call at init).
pub fn register_runtime(rt: Box<dyn LoadedRuntime>) {
    runtimes().lock().unwrap().push(rt);
}

/// Built-in runtime for the bundled QTS warm worker — always present since
/// The voice runtime lives in rightkit-media-gen.
struct QtsRuntime;
impl LoadedRuntime for QtsRuntime {
    fn name(&self) -> &'static str {
        "qts"
    }
    fn loaded(&self, _ctx: &Context) -> Vec<LoadedModel> {
        let Some(pid) = qts::qts_client::worker_pid() else {
            return Vec::new();
        };
        vec![LoadedModel {
            runtime: "qts",
            model_id: "qwen3-tts-0.6b-q8_0".into(),
            rss_gb: rss_gb_of_pid(pid),
            idle: true, // the worker is resident-but-idle between syntheses
        }]
    }
    fn unload(&self, _ctx: &Context, _model_id: &str) -> GenResult<()> {
        qts::qts_client::shutdown();
        Ok(())
    }
}

fn ensure_builtins() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| register_runtime(Box::new(QtsRuntime)));
}

// ---------- memory ------------------------------------------------------------

/// Free physical memory in GB.
pub fn free_mem_gb() -> f64 {
    platform_free_bytes()
        .map(|b| b as f64 / 1_073_741_824.0)
        .unwrap_or(0.0)
}

#[cfg(target_os = "macos")]
fn platform_free_bytes() -> Option<u64> {
    // host_statistics64(HOST_VM_INFO64) — free + inactive pages are usable.
    use std::mem::size_of;
    // Mirrors <mach/vm_statistics.h> vm_statistics64 (natural_t = u32 counters
    // interleaved with u64 event counters); count is in 32-bit words.
    #[repr(C)]
    struct VmStats {
        free_count: u32,
        active_count: u32,
        inactive_count: u32,
        wire_count: u32,
        zero_fill_count: u64,
        reactivations: u64,
        pageins: u64,
        pageouts: u64,
        faults: u64,
        cow_faults: u64,
        lookups: u64,
        hits: u64,
        purges: u64,
        purgeable_count: u32,
        speculative_count: u32,
        decompressions: u64,
        compressions: u64,
        swapins: u64,
        swapouts: u64,
        compressor_page_count: u32,
        throttled_count: u32,
        external_page_count: u32,
        internal_page_count: u32,
        total_uncompressed_pages_in_compressor: u64,
    }
    const HOST_VM_INFO64: i32 = 4;
    extern "C" {
        fn mach_host_self() -> u32;
        fn host_statistics64(host: u32, flavor: i32, info: *mut VmStats, count: *mut u32) -> i32;
        fn getpagesize() -> i32;
    }
    unsafe {
        let mut s: VmStats = std::mem::zeroed();
        let mut count = (size_of::<VmStats>() / size_of::<u32>()) as u32;
        if host_statistics64(mach_host_self(), HOST_VM_INFO64, &mut s, &mut count) != 0 {
            return None;
        }
        let page = getpagesize().max(4096) as u64;
        let pages = s.free_count as u64
            + s.inactive_count as u64
            + s.purgeable_count as u64
            + s.speculative_count as u64;
        Some(pages * page)
    }
}

#[cfg(target_os = "windows")]
fn platform_free_bytes() -> Option<u64> {
    #[repr(C)]
    struct M {
        len: u32,
        load: u32,
        total_phys: u64,
        avail_phys: u64,
        a: u64,
        b: u64,
        c: u64,
        d: u64,
        e: u64,
    }
    extern "system" {
        fn GlobalMemoryStatusEx(m: *mut M) -> i32;
    }
    let mut m: M = unsafe { std::mem::zeroed() };
    m.len = std::mem::size_of::<M>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut m) } == 0 {
        None
    } else {
        Some(m.avail_phys)
    }
}

#[cfg(target_os = "linux")]
fn platform_free_bytes() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kb = |k: &str| {
        s.lines()
            .find(|l| l.starts_with(k))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
    };
    let avail = kb("MemAvailable:");
    Some(
        if avail > 0 {
            avail
        } else {
            kb("MemFree:") + kb("Inactive:")
        } * 1024,
    )
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn platform_free_bytes() -> Option<u64> {
    None
}

/// Resident size of a pid and its descendants in GB (rightkit-child-process `tree_usage`: one process-table
/// snapshot, every member identity-checked), when the platform reports it.
fn rss_gb_of_pid(pid: u32) -> Option<f64> {
    let root = rightkit_child_process::AdoptedIdentity::capture(std::num::NonZeroU32::new(pid)?)
        .ok()??;
    let usage = rightkit_child_process::tree_usage(&root).ok()?;
    (usage.resident_bytes > 0).then(|| usage.resident_bytes as f64 / 1_073_741_824.0)
}

// ---------- persistence --------------------------------------------------------

fn load_gov(ctx: &Context) -> GenResult<Value> {
    store::setting_json(ctx, GOVERNOR_KEY, json!({"grants": [], "events": []}))
}

fn save_gov(ctx: &Context, gov: &Value) -> GenResult<()> {
    store::set_setting_json(ctx, GOVERNOR_KEY, gov)
}

fn gov_event(gov: &mut Value, action: &str, detail: Value) {
    let ev = json!({"at": store::now(), "action": action, "detail": detail});
    match gov["events"].as_array_mut() {
        Some(a) => {
            a.push(ev);
            if a.len() > 200 {
                a.drain(0..a.len() - 200);
            }
        }
        None => gov["events"] = json!([ev]),
    }
}

// ---------- queries ------------------------------------------------------------

fn loaded_all(ctx: &Context) -> Vec<LoadedModel> {
    ensure_builtins();
    let mut out = Vec::new();
    for rt in runtimes().lock().unwrap().iter() {
        out.extend(rt.loaded(ctx));
    }
    out
}

fn loaded_json(ctx: &Context) -> Value {
    let loaded: Vec<Value> = loaded_all(ctx)
        .iter()
        .map(|m| json!({"runtime": m.runtime, "model_id": m.model_id, "rss_gb": m.rss_gb, "idle": m.idle}))
        .collect();
    let resident_gb: f64 = loaded.iter().filter_map(|m| m["rss_gb"].as_f64()).sum();
    json!({
        "loaded": loaded,
        "ram": {
            "free_gb": (free_mem_gb() * 10.0).round() / 10.0 + 0.0,
            "resident_gb": (resident_gb * 10.0).round() / 10.0 + 0.0,
            "reserve_gb": RESERVE_GB,
        },
    })
}

/// Peak estimate for a model id: measured peak for this hardware profile wins,
/// then runtime estimate, then manifest `requirements.memory_gb`.
pub fn peak_estimate_gb(entry: &Value, profile: &str) -> Option<f64> {
    ensure_builtins();
    if let Some(m) = entry["measured"]
        .as_array()
        .and_then(|a| a.iter().find(|m| m["profile"].as_str() == Some(profile)))
    {
        if let Some(p) = m["peak_rss_gb"].as_f64() {
            return Some(p);
        }
    }
    let id = entry["id"].as_str().unwrap_or_default();
    for rt in runtimes().lock().unwrap().iter() {
        if let Some(p) = rt.peak_gb(id) {
            return Some(p);
        }
    }
    entry["requirements"]["memory_gb"].as_f64()
}

// ---------- commands -----------------------------------------------------------

pub fn loaded(ctx: &Context) -> GenResult<Value> {
    Ok(loaded_json(ctx))
}

pub fn status(ctx: &Context) -> GenResult<Value> {
    let mut v = loaded_json(ctx);
    let gov = load_gov(ctx)?;
    v["grants"] = gov["grants"].clone();
    v["events"] = gov["events"].clone();
    Ok(v)
}

/// Admit a heavy job or report it queued. `unload_idle: true` (default)
/// unloads idle resident models until `need_gb + reserve` fits.
pub fn acquire(
    ctx: &Context,
    input: &Value,
    entry: Option<&Value>,
    profile: &str,
) -> GenResult<Value> {
    let job = input["job"].as_str().unwrap_or("job").to_string();
    let need = input["need_gb"]
        .as_f64()
        .or_else(|| entry.and_then(|e| peak_estimate_gb(e, profile)));
    let Some(need_gb) = need else {
        return Err(GenError::new(
            "invalid_input",
            "need_gb or a model_id with requirements is required",
        ));
    };
    let unload_idle = input["unload_idle"].as_bool().unwrap_or(true);

    let mut unloaded: Vec<Value> = Vec::new();
    // Freed-but-still-being-reclaimed memory counts toward the admit decision
    // as an estimate; the recorded event keeps both numbers.
    let mut freed_estimate = 0.0f64;
    let admit = |need_gb: f64, freed: f64| free_mem_gb() + freed >= need_gb + RESERVE_GB;
    if !admit(need_gb, freed_estimate) && unload_idle {
        ensure_builtins();
        for m in loaded_all(ctx).into_iter().filter(|m| m.idle) {
            let rts = runtimes().lock().unwrap();
            let Some(rt) = rts.iter().find(|r| r.name() == m.runtime) else {
                continue;
            };
            match rt.unload(ctx, &m.model_id) {
                Ok(()) => {
                    freed_estimate += m.rss_gb.unwrap_or(0.0);
                    unloaded.push(
                        json!({"runtime": m.runtime, "model_id": m.model_id, "freed_gb": m.rss_gb}),
                    );
                }
                Err(e) => unloaded.push(
                    json!({"runtime": m.runtime, "model_id": m.model_id, "error": e.message}),
                ),
            }
            if admit(need_gb, freed_estimate) {
                break;
            }
        }
    }

    let mut gov = load_gov(ctx)?;
    let free = free_mem_gb();
    if admit(need_gb, freed_estimate) {
        let grant = format!("gov-{}", store::now().replace([':', '-'], ""));
        let grants = gov["grants"].as_array_mut().unwrap();
        grants.push(json!({"id": grant, "job": job, "need_gb": need_gb, "at": store::now()}));
        gov_event(
            &mut gov,
            "acquire",
            json!({"job": job, "need_gb": need_gb, "unloaded": unloaded, "free_gb": free}),
        );
        save_gov(ctx, &gov)?;
        Ok(
            json!({"admitted": true, "grant": grant, "free_gb": free, "need_gb": need_gb, "unloaded": unloaded}),
        )
    } else {
        let blocking: Vec<Value> = loaded_all(ctx)
            .iter()
            .filter(|m| !m.idle)
            .map(|m| json!({"runtime": m.runtime, "model_id": m.model_id, "rss_gb": m.rss_gb}))
            .collect();
        gov_event(
            &mut gov,
            "queued",
            json!({"job": job, "need_gb": need_gb, "free_gb": free, "blocking": blocking}),
        );
        save_gov(ctx, &gov)?;
        Ok(json!({
            "admitted": false, "state": "queued", "free_gb": (free * 10.0).round() / 10.0,
            "need_gb": need_gb, "reserve_gb": RESERVE_GB, "blocking": blocking,
            "unloaded": unloaded,
            "message": "insufficient free memory — retry after blocking jobs finish or lower requirements",
        }))
    }
}

/// Convenience admission for in-crate heavy jobs (whisper, pipelines): the
/// caller passes an explicit peak estimate instead of a manifest model id.
/// Returns the same shape as `acquire` (`admitted`, `grant`, `message`).
/// Call sites are applied by the wave-5 integration patch (whisper/QTS).
#[allow(dead_code)]
pub fn admit(ctx: &Context, job: &str, need_gb: f64) -> GenResult<Value> {
    let hw = super::hardware::detect_hardware();
    let profile = hw["profile"].as_str().unwrap_or_default().to_string();
    acquire(
        ctx,
        &json!({"job": job, "need_gb": need_gb}),
        None,
        &profile,
    )
}

/// Admission for a catalog model: peak comes from measured data for this
/// hardware profile, then the manifest requirements. `model_id` unknown →
/// falls back to `need_gb` when given, else `invalid_input`.
#[allow(dead_code)] // call sites are applied by the wave-5 integration patch
pub fn admit_model(
    ctx: &Context,
    job: &str,
    model_id: &str,
    fallback_gb: Option<f64>,
) -> GenResult<Value> {
    let hw = super::hardware::detect_hardware();
    let profile = hw["profile"].as_str().unwrap_or_default().to_string();
    let entry = super::find_entry(model_id).ok();
    let mut input = json!({"job": job, "model_id": model_id});
    if entry.is_none() {
        if let Some(g) = fallback_gb {
            input["need_gb"] = json!(g);
        }
    }
    acquire(ctx, &input, entry.as_ref(), &profile)
}

pub fn release(ctx: &Context, input: &Value) -> GenResult<Value> {
    let grant = input["grant"].as_str().unwrap_or_default();
    let mut gov = load_gov(ctx)?;
    let before = gov["grants"].as_array().map(|a| a.len()).unwrap_or(0);
    if let Some(a) = gov["grants"].as_array_mut() {
        a.retain(|g| g["id"].as_str() != Some(grant));
    }
    let after = gov["grants"].as_array().map(|a| a.len()).unwrap_or(0);
    if before == after {
        return Err(GenError::new(
            "not_found",
            format!("no active grant {grant}"),
        ));
    }
    gov_event(&mut gov, "release", json!({"grant": grant}));
    save_gov(ctx, &gov)?;
    Ok(json!({"released": grant}))
}

/// Explicit unload by runtime — the Models panel's Unload button calls this.
pub fn unload_runtime(ctx: &Context, input: &Value) -> GenResult<Value> {
    let name = input["runtime"].as_str().unwrap_or_default();
    let model_id = input["model_id"].as_str().unwrap_or_default();
    ensure_builtins();
    let rts = runtimes().lock().unwrap();
    let Some(rt) = rts.iter().find(|r| r.name() == name) else {
        return Err(GenError::new(
            "not_found",
            format!("no registered runtime {name}"),
        ));
    };
    rt.unload(ctx, model_id)?;
    Ok(json!({"unloaded": true, "runtime": name, "model_id": model_id}))
}
