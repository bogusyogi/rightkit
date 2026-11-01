//! studio.models — hardware-aware model management, model install, and the
//! rented-worker lifecycle (docs/MODEL-MANAGEMENT.md). One manifest of model
//! entries with revision/quantization/capabilities, joined at query time with
//! real on-disk state: bundled sd.cpp image profiles
//! (`assets/local-image/profiles.json`), the QTS voice model folder, the
//! provider API models, and rented-GPU route candidates.
//!
//! Commands (all `studio.models.<verb>`):
//! - `hardware`  → detected host profile `{os, arch, memory_gb, gpus, profile}`.
//!   `RIGHTKIT_HW_JSON` injects a profile (tests, remote reports).
//! - `catalog`   → unified entries joined with readiness + fit summary.
//! - `describe`  `{id}` → one entry: revision, quantization, capabilities,
//!   requirements, measured vs estimated fit evidence, licence.
//! - `probe`     `{id}` → safe live readiness check (filesystem, worker
//!   discovery, credential presence only — never key material).
//! - `estimate`  `{id, workload?}` → `{fit, required_gb, available_gb, reasons}`.
//! - `install`   `{id, force?}` → starts a real resumable, sha256-verified
//!   `rightkit-models` install in a background thread for engines with a public download route
//!   (sd.cpp profiles, QTS); other engines report their honest route.
//! - `install.status` `{id?}`, `install.pause` `{id}` (keeps the partial; install again resumes),
//!   `install.cancel` `{id}` (discards the partial),
//!   `install.uninstall` `{id}` (deletes files — explicit user action),
//!   `install.rollback` `{id, revision?}`.
//! - `route.get` / `route.set` `{capability, route, model?, revision?}` →
//!   persisted per-capability route choice in the shared settings table.
//! - `unload`    `{id}` → real unload: QTS cancel/quiesce, local worker
//!   shutdown; bundled engines report their owning service.
//! - `workers`   → persisted rented-worker records incl. budget state.
//! - `rental.search` `{query?, api_key?}` → Vast offer search. The key is resolved like every Vast
//!   call: the stored credential, then `VAST_API_KEY`, then the per-call `api_key` (never persisted).
//!   `no_credentials` without one.
//! - `worker.action` `{worker_id?|spec, action}` → register / stage /
//!   reconnect / begin / retrieve (hash-checked) / idle / unload / teardown /
//!   provision (Vast create — requires `authorized:true` + offer_id +
//!   max_usd_hr + budget_usd + max_duration_min). Drivers: `spec.driver`
//!   `ssh` | `vast` | `local-fs` | none→NullDriver (`driver_not_configured`).
//!   No action launches a billed instance without the explicit provision call.
//!   `provision` reserves its `budget_usd` in the spend ledger first and `teardown` settles it
//!   (see `rental.rs`); `resolve_rental` resolves a create whose outcome is unknown;
//!   `resolve_destroy` re-checks a `destroy_unconfirmed` rental (confirmed-gone settles with
//!   `estimated_end`). `set_vast_key` `{api_key}` saves the Vast key in the OS credential store
//!   (empty deletes it) and `vast_key_status` reports whether one is stored; neither returns the key.
//!   `provision` is refused (`no_stored_key`) unless a key is stored, so a rental can never exist
//!   that the app cannot destroy. A destroy only goes to an instance carrying this worker's label
//!   (`instance_not_owned` otherwise), and `register` takes no `spec.vast.instance_id`.
//!   `teardown` of a worker that is `running` is refused unless `force:true` (it interrupts the
//!   work), then follows the same confirmed-destroy and settlement path.
//! - `rentals` → every Vast rental with elapsed, accrued and time left on its duration, budget and
//!   idle limits, plus warnings. `rentals_enforce` → destroys and settles the rentals past a limit
//!   and reports what it did (`rental.rs`).
//!   Enforcement also resolves `renting`/`outcome_unknown` creates by label (adopt one, release after
//!   none twice a minute apart, report several, never destroy automatically on several).
//!   HONEST LIMIT: enforcement runs only while the host app runs (sidecar start, every 60 s while the app
//!   is open, the start of each CLI command touching workers or generation, and `rentals_enforce`).
//!   With neither the app nor a CLI command running, nothing enforces anything and a rented instance
//!   bills until the next engine start or until it is destroyed at the provider. An instance-side
//!   maximum lifetime in the create request would be the real fix; it needs the provider's documented
//!   field name from a fetched source and none is sent today.

use std::path::PathBuf;

use serde_json::{json, Value};

use rightkit_media_gen::providers::qts;
use rightkit_media_gen::providers::qts::qts_models;
use rightkit_media_gen::{credentials, store, Context, GenError, GenResult};

#[allow(unused_imports)]
use crate::{
    get_json, load_workers, new_worker_id, provision_rental, save_workers, set_json,
    teardown_legacy_vast, teardown_rental, worker_event, WORKERS_KEY,
};
use crate::{remote, rental};

pub mod blender;
pub mod governor;
pub mod hardware;
pub mod install;
pub mod sdcpp;
pub mod wan2gp;

// Runtime owners (local_image, qts, future ONNX lanes) register their
// resident-model hooks through this public surface.
pub use governor::{register_runtime, LoadedModel, LoadedRuntime};

pub const COMMANDS: &[&str] = &[
    "studio.models.hardware",
    "studio.models.catalog",
    "studio.models.describe",
    "studio.models.probe",
    "studio.models.estimate",
    "studio.models.install",
    "studio.models.install.status",
    "studio.models.install.pause",
    "studio.models.install.cancel",
    "studio.models.install.uninstall",
    "studio.models.install.rollback",
    "studio.models.route.get",
    "studio.models.route.set",
    "studio.models.unload",
    "studio.models.loaded",
    "studio.models.governor.status",
    "studio.models.governor.acquire",
    "studio.models.governor.release",
    "studio.models.governor.unload",
    "studio.models.workers",
    "studio.models.rental.search",
    "studio.models.worker.action",
    "studio.models.rentals",
    "studio.models.rentals_enforce",
];

// Unified model manifest. `source_manifest` entries are hydrated from the
// existing packaged manifests rather than duplicated here.
const MANIFEST_JSON: &str = r#"{
  "schema_version": "1.0.0",
  "entries": [
    {
      "id": "qwen3-tts-0.6b-q8_0",
      "label": "Qwen3-TTS 0.6B (QTS, x-vector cloning)",
      "modality": "voice",
      "engine": "qts",
      "engine_kind": "native-bundled",
      "revision": "qts-1",
      "quantization": "q8_0",
      "backends": ["metal", "vulkan"],
      "requirements": {"memory_gb": 8, "disk_gb": 2},
      "capabilities": ["text_to_speech", "voice_clone"],
      "license": {"name": "Apache-2.0 code; check upstream weights licence", "commercial": true},
      "download": {"hf_repo": "dsh0416/Qwen3-TTS-12Hz-0.6B-Base-QTS", "hf_revision": "main",
        "files": [
          {"name": "qwen3-tts-0.6b-q8_0.gguf", "bytes": 1325190016, "sha256": "101b17e49b8042ffb07b189e4cbdc18797c879c2a2868bc72306c44ba2bd4ebf"},
          {"name": "qwen3-tts-vocoder.onnx", "bytes": 459514304, "sha256": "b219a025f6d43a4c0f16764fd038ad159650863e7565b9f0b2f24cea5625077e"},
          {"name": "onnxruntime-linux-x64-1.24.4.tgz",
            "url": "https://github.com/microsoft/onnxruntime/releases/download/v1.24.4/onnxruntime-linux-x64-1.24.4.tgz",
            "platforms": ["x86_64-unknown-linux-gnu"],
            "extract_member": "onnxruntime-linux-x64-1.24.4/lib/libonnxruntime.so.1.24.4",
            "dest_name": "libonnxruntime.so", "dest_dir": "runtime/qts"},
          {"name": "onnxruntime-linux-aarch64-1.24.4.tgz",
            "url": "https://github.com/microsoft/onnxruntime/releases/download/v1.24.4/onnxruntime-linux-aarch64-1.24.4.tgz",
            "platforms": ["aarch64-unknown-linux-gnu"],
            "extract_member": "onnxruntime-linux-aarch64-1.24.4/lib/libonnxruntime.so.1.24.4",
            "dest_name": "libonnxruntime.so", "dest_dir": "runtime/qts"},
          {"name": "onnxruntime-win-x64-1.24.4.zip",
            "url": "https://github.com/microsoft/onnxruntime/releases/download/v1.24.4/onnxruntime-win-x64-1.24.4.zip",
            "platforms": ["x86_64-pc-windows-msvc"],
            "bytes": 74442783, "sha256": "d2319fddfb6ea4db99ccc4b60c85c517bcd855721f5daa6a06d40d7cb2ee2357",
            "extract_member": "onnxruntime-win-x64-1.24.4/lib/onnxruntime.dll",
            "dest_name": "onnxruntime.dll", "dest_dir": "runtime/qts",
            "installed_sha256": "b95efb2113b603bbbf3f191061c5516a871ed546893c820e4f3b7b6c358dbf2a",
            "installed_bytes": 14203464}
        ]},
      "measured": [
        {"profile": "macos-24gb-unified", "gpu": "Apple M6", "memory_gb": 24, "backend": "metal",
         "at": "2026-10-06", "peak_rss_gb": 2.3, "cold_wall_s": 79.1, "warm_wall_s": 6.7,
         "output": "88364 B wav, ~8-word line",
         "notes": "cold includes one-time Metal pipeline compile; warm run reuses resident worker"}
      ]
    },
    {
      "id": "wan2.2-ti2v-5b",
      "label": "Wan2.2 TI2V-5B (rented GPU route)",
      "modality": "video",
      "engine": "rented",
      "engine_kind": "rented-remote",
      "revision": "wan2.2",
      "requirements": {"vram_gb": 24, "disk_gb": 40},
      "capabilities": ["text_to_video", "image_to_video"],
      "license": {"name": "Apache-2.0", "commercial": true},
      "measured": []
    },
    {
      "id": "soulx-flashhead",
      "label": "SoulX-FlashHead avatar (local/rented experiment)",
      "modality": "avatar",
      "engine": "rented",
      "engine_kind": "experimental",
      "revision": "flashhead-1",
      "requirements": {"vram_gb": 8, "disk_gb": 20},
      "capabilities": ["talking_head", "lip_sync"],
      "license": {"name": "check upstream SoulX-FlashHead licence", "commercial": false},
      "measured": [
        {"profile": "rtx4070-8gb", "peak_gb": 7.8, "warm_rtf": 1.25,
         "notes": "prior experiment report: executed but output quality rejected (portrait crop, mouth/head motion); not qualified"}
      ]
    },
    {
      "id": "ace-step-1.5",
      "label": "ACE-Step 1.5 music (candidate)",
      "modality": "music",
      "engine": "rented",
      "engine_kind": "rented-remote",
      "revision": "1.5",
      "requirements": {"vram_gb": 8, "disk_gb": 20},
      "capabilities": ["music"],
      "license": {"name": "check upstream ACE-Step licence", "commercial": false},
      "measured": []
    }
  ]
}"#;

const LOCAL_IMAGE_PROFILES: &str = include_str!("../../data/local-image-profiles.json");

const ROUTES_KEY: &str = "models.routes";

const ROUTES: &[&str] = &["auto", "local", "api", "rented"];
const CAPABILITIES: &[&str] = &[
    "image",
    "video",
    "voice",
    "avatar",
    "music",
    "sfx",
    "upscale",
    "lipsync",
    "transcribe",
];

// ---------- manifest --------------------------------------------------------

fn manifest_entries() -> Vec<Value> {
    let doc: Value = serde_json::from_str(MANIFEST_JSON).expect("embedded model manifest");
    let mut entries: Vec<Value> = doc["entries"].as_array().cloned().unwrap_or_default();
    // Hydrate the sd.cpp image profiles from the packaged manifest instead of
    // duplicating revision/component/hash data.
    if let Ok(profiles) = serde_json::from_str::<Value>(LOCAL_IMAGE_PROFILES) {
        for p in profiles["profiles"].as_array().into_iter().flatten() {
            let total_bytes: i64 = p["components"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|c| c["required"].as_bool().unwrap_or(true))
                .filter_map(|c| c["bytes"].as_i64())
                .sum();
            let files: Vec<Value> = p["components"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|c| c["required"].as_bool().unwrap_or(true))
                .map(|c| {
                    json!({
                        "name": c["file"],
                        "url": format!("https://huggingface.co/{}/resolve/{}/{}",
                            c["source"]["repo"].as_str().unwrap_or_default(),
                            c["source"]["revision"].as_str().unwrap_or_default(),
                            c["source"]["file"].as_str().unwrap_or_default()),
                        "bytes": c["bytes"],
                        "sha256": c["sha256"],
                        "sha256_source": c["sha256_source"],
                    })
                })
                .collect();
            entries.push(json!({
                "id": p["id"],
                "label": p["label"],
                "modality": "image",
                "engine": "sdcpp",
                "engine_kind": "native-bundled",
                "revision": p["revision"],
                "quantization": p["id"].as_str().map(|id| {
                    if id.contains("int8") { "int8" } else if id.contains("q6") { "Q6_K" } else { "gguf" }
                }),
                "role": p["role"],
                "backends": ["cpu", "metal", "vulkan"],
                // ESTIMATE (unmeasured): the resident peak of the local image memory mode this
                // machine would use (`auto` picks from total memory), see
                // rightkit_local_gen::memory.
                "requirements": {"memory_gb": rightkit_media_gen::providers::local_image::memory_estimate_gb(), "disk_gb": (total_bytes as f64 / 1e9 * 10.0).round() / 10.0},
                "capabilities": p["tasks"],
                "license": p["license"],
                "components": p["components"].as_array().map(|a| a.len()),
                "download": {"files": files},
                "source_manifest": "local-image",
                "measured": p["measured"],
            }));
        }
    }
    // External workers are catalogued from their provider's public model list
    // so readiness stays joined to the runtime actually used by generation.
    entries.extend(blender::manifest_entries());
    // Test-only seam, compiled out of release builds: extra manifest entries from a
    // JSON file, so e2e scenarios can install small served files with real pins.
    if let Some(extra) = std::env::var_os("RIGHTKIT_TEST_MODEL_ENTRIES")
        .filter(|_| cfg!(debug_assertions))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<Vec<Value>>(&t).ok())
    {
        entries.extend(extra);
    }
    entries
}

fn find_entry(id: &str) -> GenResult<Value> {
    manifest_entries()
        .into_iter()
        .find(|e| e["id"].as_str() == Some(id))
        .ok_or_else(|| GenError::new("model_not_found", format!("no manifest entry: {id}")))
}

/// True when the model id belongs to a provider adapter (API route).
fn provider_model(id: &str) -> Option<Value> {
    rightkit_media_gen::providers::registry()
        .iter()
        .flat_map(|p| p.models().iter().map(move |m| (p.id(), m)))
        .find(|(_, m)| m.model == id)
        .map(|(pid, m)| {
            json!({
                "provider": pid, "model": m.model,
                "kind": serde_json::to_value(m.kind).unwrap_or(Value::Null),
                "label": m.label, "est_usd": m.est_usd,
            })
        })
}

// ---------- install-state probes (no secrets) --------------------------------

/// QTS model dir: the store's active revision, else the pre-store flat folder
/// (`RIGHTKIT_QTS_MODEL_DIR` or `<data>/models/qts`).
fn qts_model_dir(_ctx: &Context) -> PathBuf {
    qts_models::model_dir()
}

fn qts_files_present(dir: &std::path::Path) -> bool {
    qts_models::files_present(dir)
}

fn sdcpp_profile_state(root: &std::path::Path, id: &str, revision: i64) -> (String, Vec<String>) {
    let dir = rightkit_local_gen::model_store::profile_dir(root, id, revision);
    let Ok(profiles) = serde_json::from_str::<Value>(LOCAL_IMAGE_PROFILES) else {
        return (
            "missing".into(),
            vec!["packaged profile manifest unreadable".into()],
        );
    };
    let Some(spec) = profiles["profiles"]
        .as_array()
        .and_then(|a| a.iter().find(|p| p["id"].as_str() == Some(id)))
    else {
        return ("missing".into(), vec!["unknown local-image profile".into()]);
    };
    let missing: Vec<String> = spec["components"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["required"].as_bool().unwrap_or(true))
        .filter(|c| !dir.join(c["file"].as_str().unwrap_or_default()).is_file())
        .filter_map(|c| c["file"].as_str().map(String::from))
        .collect();
    if missing.is_empty() {
        ("native_ready".into(), vec![])
    } else {
        ("missing".into(), missing)
    }
}

/// Readiness classes distinguish bundled engines from external runtimes:
/// `native_ready`, `external_available`, `external_unavailable`, `missing`,
/// `api`, `rental`, and `unsupported_backend`.
fn readiness(ctx: &Context, entry: &Value) -> Value {
    let engine = entry["engine"].as_str().unwrap_or_default();
    let id = entry["id"].as_str().unwrap_or_default();
    match engine {
        "sdcpp" => {
            let rev = entry["revision"].as_i64().unwrap_or(0);
            let root = sdcpp::runtime_paths(ctx).model_root;
            let (state, missing) = sdcpp_profile_state(&root, id, rev);
            json!({"state": state, "engine": "sdcpp", "missing_files": missing,
                   "install_via": "studio.models.install"})
        }
        "qts" => {
            let dir = qts_model_dir(ctx);
            if qts_files_present(&dir) {
                json!({"state": "native_ready", "engine": "qts", "model_dir": dir})
            } else {
                json!({"state": "missing", "engine": "qts", "model_dir": dir,
                       "missing_files": ["qwen3-tts-0.6b-q8_0.gguf", "qwen3-tts-vocoder.onnx"],
                       "install_via": "studio.models.install"})
            }
        }
        "rented" => json!({"state": "rental", "engine": "rented",
                          "detail": "route candidate; execute through a staged rented worker"}),
        "local-blender" => blender::readiness(),
        _ => json!({"state": "unqualified", "engine": engine}),
    }
}

// ---------- fit estimation ---------------------------------------------------

/// Fit of a catalog entry on this hardware (the shared `rightkit-models` fit;
/// rented/experimental engines are `rental_only` via `remote_only`).
fn fit(entry: &Value, hw: &Value) -> Value {
    if entry["engine"].as_str() == Some("local-blender") {
        return json!({
            "fit": "unqualified",
            "profile": hw["profile"],
            "reasons": ["external Blender route has no recorded qualification for this hardware profile"],
        });
    }
    hardware::local_fit(entry, hw)
}

// ---------- persisted state --------------------------------------------------

// ---------- install spec --------------------------------------------------------

/// What `studio.models.install` installs for an entry, as a `rightkit-models`
/// manifest. Engines with no public download route return `None` and
/// `install` reports the honest guidance.
fn install_target(ctx: &Context, entry: &Value) -> Option<install::Target> {
    use rightkit_models::{Extract, FileSpec, ModelSpec, RevisionSpec};
    let id = entry["id"].as_str()?;
    // Bundled sd.cpp profiles have no target here: they install through `sdcpp`.
    let (store_root, legacy_dir, revision, quant) = match entry["engine"].as_str()? {
        "qts" => (
            qts_models::store_root(),
            qts_models::legacy_model_dir(),
            qts_models::REVISION.to_string(),
            qts_models::QUANT.to_string(),
        ),
        _ => return None,
    };
    let mut files: Vec<FileSpec> = Vec::new();
    let mut legacy: Vec<(String, PathBuf)> = Vec::new();
    let mut shared_root: Option<(String, PathBuf)> = None;
    let platform = rightkit_models::Platform::current();
    for f in entry["download"]["files"].as_array()? {
        let name = f["name"].as_str()?.to_string();
        let url = f["url"].as_str().map(String::from).or_else(|| {
            let repo = entry["download"]["hf_repo"].as_str()?;
            let rev = entry["download"]["hf_revision"].as_str().unwrap_or("main");
            Some(format!(
                "https://huggingface.co/{repo}/resolve/{rev}/{name}"
            ))
        })?;
        let dest_name = f["dest_name"].as_str().unwrap_or(&name).to_string();
        let dest_dir = f["dest_dir"].as_str().map(|d| {
            let p = PathBuf::from(d);
            if p.is_absolute() {
                p
            } else {
                ctx.data_dir.join(p)
            }
        });
        // A file with its own destination (the ONNX Runtime library) is a
        // shared runtime: it lives in that directory, outside the revision.
        let shared = dest_dir
            .is_some()
            .then(|| qts_models::ORT_SHARED_KIND.to_string());
        let path = dest_name.clone();
        let spec = FileSpec {
            path: path.clone(),
            url,
            mirrors: Vec::new(),
            bytes: f["bytes"].as_u64(),
            sha256: f["sha256"].as_str().map(String::from),
            platforms: f["platforms"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            extract: f["extract_member"]
                .as_str()
                .map(|m| Extract::Member(m.to_string())),
            installed_bytes: f["installed_bytes"].as_u64(),
            installed_sha256: f["installed_sha256"].as_str().map(String::from),
            shared,
        };
        if platform.allows(&spec.platforms) {
            match dest_dir {
                Some(dir) => shared_root = Some((qts_models::ORT_SHARED_KIND.to_string(), dir)),
                None => legacy.push((path, legacy_dir.join(&dest_name))),
            }
        }
        files.push(spec);
    }
    if files.is_empty() {
        return None;
    }
    let spec = ModelSpec::new(
        id.to_string(),
        vec![RevisionSpec {
            revision,
            quant: Some(quant),
            files,
            requirements: Default::default(),
            measured: Vec::new(),
        }],
    )
    .with_label(entry["label"].as_str().unwrap_or_default().to_string());
    Some(install::Target {
        store_root,
        spec,
        legacy,
        legacy_dir: Some(legacy_dir),
        shared_root,
    })
}

/// Whether `studio.models.install` can install this entry: a bundled sd.cpp profile (through the
/// local-image installer) or an entry with a download target.
fn installable(ctx: &Context, entry: &Value) -> bool {
    entry["engine"].as_str() == Some("sdcpp") || install_target(ctx, entry).is_some()
}

/// True when this platform has an ONNX Runtime library for QTS. macOS arm64 ships it inside the
/// app bundle (`Contents/Resources/runtime`, signed with the app); it is never downloaded there.
/// Elsewhere the QTS manifest lists the run-time download.
pub fn qts_ort_supported() -> bool {
    if cfg!(target_os = "macos") {
        return cfg!(target_arch = "aarch64");
    }
    let platform = rightkit_models::Platform::current();
    find_entry(qts_models::MODEL_ID)
        .map(|e| {
            e["download"]["files"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|f| {
                    f["extract_member"].is_string()
                        && f["platforms"]
                            .as_array()
                            .map(|p| {
                                platform.allows(
                                    &p.iter()
                                        .filter_map(|t| t.as_str().map(String::from))
                                        .collect::<Vec<_>>(),
                                )
                            })
                            .unwrap_or(true)
                })
        })
        .unwrap_or(false)
}

/// Start the QTS install (weights, plus the ONNX Runtime download where the platform has one;
/// macOS bundles it). The runtime library is skipped when one is already resolvable.
pub fn start_qts_install(ctx: &Context) -> GenResult<Value> {
    let entry = find_entry(qts_models::MODEL_ID)?;
    let mut target = install_target(ctx, &entry)
        .ok_or_else(|| GenError::new("unsupported", "no install route for the QTS model"))?;
    if qts_models::find_onnxruntime().is_some() {
        if let Some(rev) = target.spec.revisions.last_mut() {
            rev.files.retain(|f| f.shared.is_none());
        }
    }
    install::start(ctx, qts_models::MODEL_ID, target)
}

// ---------- command handlers --------------------------------------------------

fn catalog(ctx: &Context) -> GenResult<Value> {
    let hw = hardware::detect_hardware();
    let entries: Vec<Value> = manifest_entries()
        .iter()
        .map(|e| {
            let mut v = e.clone();
            v["readiness"] = readiness(ctx, e);
            v["fit"] = fit(e, &hw);
            v["installable"] = json!(installable(ctx, e));
            v
        })
        .collect();
    let routes = get_json(ctx, ROUTES_KEY, json!({}))?;
    let installs = install::status(ctx, None)?;
    Ok(json!({"hardware": hw, "entries": entries, "routes": routes,
              "installs": installs["installs"], "capabilities": CAPABILITIES}))
}

fn describe(ctx: &Context, input: &Value) -> GenResult<Value> {
    let id = input["id"].as_str().unwrap_or_default();
    if let Ok(entry) = find_entry(id) {
        let hw = hardware::detect_hardware();
        return Ok(json!({"entry": entry, "readiness": readiness(ctx, &entry),
                         "fit": fit(&entry, &hw),
                         "installable": installable(ctx, &entry),
                         "install": install::status(ctx, Some(id)).ok().and_then(|s| s["install"].as_object().cloned()).map(Value::Object)}));
    }
    if let Some(m) = provider_model(id) {
        let configured =
            credentials::status_source(ctx, m["provider"].as_str().unwrap_or_default()).is_some();
        return Ok(json!({"entry": m, "readiness": {"state": "api", "configured": configured}}));
    }
    Err(GenError::new(
        "model_not_found",
        format!("no such model: {id}"),
    ))
}

fn probe(ctx: &Context, input: &Value) -> GenResult<Value> {
    let id = input["id"].as_str().unwrap_or_default();
    if let Ok(entry) = find_entry(id) {
        let r = readiness(ctx, &entry);
        return Ok(
            json!({"id": id, "readiness": r, "fit": fit(&entry, &hardware::detect_hardware())}),
        );
    }
    if let Some(m) = provider_model(id) {
        let configured =
            credentials::status_source(ctx, m["provider"].as_str().unwrap_or_default()).is_some();
        return Ok(json!({"id": id, "readiness": {"state": "api", "configured": configured}}));
    }
    Err(GenError::new(
        "model_not_found",
        format!("no such model: {id}"),
    ))
}

fn estimate(ctx: &Context, input: &Value) -> GenResult<Value> {
    let id = input["id"].as_str().unwrap_or_default();
    let hw = hardware::detect_hardware();
    if let Ok(entry) = find_entry(id) {
        return Ok(json!({"id": id, "workload": input["workload"],
                         "fit": fit(&entry, &hw), "readiness": readiness(ctx, &entry)}));
    }
    if let Some(m) = provider_model(id) {
        return Ok(json!({"id": id, "fit": hardware::api_fit(&m["est_usd"])}));
    }
    Err(GenError::new(
        "model_not_found",
        format!("no such model: {id}"),
    ))
}

/// Real install for engines with a download spec (resumable + sha256-verified
/// background download). Engines whose install lives elsewhere report the
/// honest route; nothing reports a download it did not run.
fn install_cmd(ctx: &Context, input: &Value) -> GenResult<Value> {
    let id = input["id"].as_str().unwrap_or_default();
    let entry = find_entry(id)?;
    let r = readiness(ctx, &entry);
    let force = input["force"].as_bool().unwrap_or(false);
    if r["state"] == "native_ready" && !force {
        return Ok(json!({"id": id, "state": "native_ready"}));
    }
    if entry["id"].as_str() == Some(qts_models::MODEL_ID) {
        return start_qts_install(ctx);
    }
    if entry["engine"].as_str() == Some("sdcpp") {
        return sdcpp::start(ctx, id);
    }
    if let Some(target) = install_target(ctx, &entry) {
        return install::start(ctx, id, target);
    }
    match entry["engine"].as_str().unwrap_or_default() {
        "local-blender" => Ok(blender::install_guidance(id)),
        "rented" => Ok(json!({
            "id": id, "state": "rental",
            "message": "no local install; register and stage a rented worker (studio.models.worker.action)",
        })),
        other => Err(GenError::new(
            "unsupported",
            format!("no install route for engine {other}"),
        )),
    }
}

fn route_get(ctx: &Context) -> GenResult<Value> {
    Ok(json!({"routes": get_json(ctx, ROUTES_KEY, json!({}))?,
              "capabilities": CAPABILITIES, "valid_routes": ROUTES}))
}

fn route_set(ctx: &Context, input: &Value) -> GenResult<Value> {
    let cap = input["capability"].as_str().unwrap_or_default();
    if !CAPABILITIES.contains(&cap) {
        return Err(GenError::new(
            "invalid_input",
            format!("unknown capability: {cap}"),
        ));
    }
    let route = input["route"].as_str().unwrap_or_default();
    if !ROUTES.contains(&route) {
        return Err(GenError::new(
            "invalid_input",
            format!("route must be one of {ROUTES:?}"),
        ));
    }
    let mut routes = get_json(ctx, ROUTES_KEY, json!({}))?;
    routes[cap] = json!({
        "route": route,
        "model": input["model"].as_str(),
        "revision": input["revision"].as_str(),
        "set_at": store::now(),
    });
    set_json(ctx, ROUTES_KEY, &routes)?;
    Ok(json!({"routes": routes}))
}

/// Real unload: stop a running QTS synthesis and ask the Python worker to
/// exit. Bundled engines own their own lifecycle; the local-image runtime is
/// unloaded through its service, not here.
fn unload(_ctx: &Context, input: &Value) -> GenResult<Value> {
    let id = input["id"].as_str().unwrap_or_default();
    let entry = find_entry(id)?;
    match entry["engine"].as_str().unwrap_or_default() {
        "qts" => {
            qts::cancel_active();
            Ok(
                json!({"id": id, "unloaded": true, "note": "active QTS synthesis cancelled; worker exits on idle"}),
            )
        }
        "sdcpp" => Ok(json!({"id": id, "unloaded": false,
                             "note": "the local-image runtime owns the sd.cpp server lifecycle"})),
        "rented" => Err(GenError::new(
            "invalid_input",
            "use studio.models.worker.action unload/teardown for rented workers",
        )),
        other => Err(GenError::new(
            "unsupported",
            format!("cannot unload engine {other}"),
        )),
    }
}

/// Every worker-record read-modify-write runs under the rentals lock, so the CLI, the sidecar and
/// the enforcement pass cannot overwrite each other's changes to the one records array.
fn worker_action(ctx: &Context, input: &Value) -> GenResult<Value> {
    let _lock = rental::RentalLock::acquire(ctx, std::time::Duration::from_secs(90))?;
    worker_action_locked(ctx, input)
}

fn worker_action_locked(ctx: &Context, input: &Value) -> GenResult<Value> {
    let action = input["action"].as_str().unwrap_or_default();
    let mut workers = load_workers(ctx)?;

    // The Vast key lives in the OS credential store, never in a worker record. Neither action
    // returns, logs or echoes the key.
    if action == "set_vast_key" {
        let key = input["api_key"].as_str().unwrap_or("");
        remote::store_key(ctx, key)?;
        return Ok(json!({"ok": true, "vast_key_stored": remote::key_is_stored(ctx)}));
    }
    if action == "vast_key_status" {
        return Ok(json!({"vast_key_stored": remote::key_is_stored(ctx)}));
    }

    if action == "register" {
        let spec = input.get("spec").cloned().unwrap_or_else(|| json!({}));
        // A Vast worker is minted with a fresh id, so no existing instance can carry its label: an
        // instance id supplied here could only point at someone else's instance (and a later teardown
        // would destroy it). Adoption goes through provision then resolve_rental, which match by label.
        if spec["driver"].as_str() == Some("vast") && !spec["vast"]["instance_id"].is_null() {
            return Err(GenError::new(
                "instance_not_owned",
                "a Vast worker cannot be registered with an instance_id: no existing instance carries the label of a worker that does not exist yet. Provision it (or resolve_rental) so the instance is matched by this worker's label",
            ));
        }
        let id = new_worker_id();
        let budget = input["budget_usd"].as_f64().unwrap_or(0.0);
        let w = json!({
            "worker_id": id,
            "label": input["label"].as_str().unwrap_or("rented worker"),
            "spec": spec,
            "driver": remote::driver_for(ctx, &json!({"spec": spec})).name(),
            "state": "configured",
            "budget_usd": budget,
            "accrued_usd": 0.0,
            "outputs": [],
            "events": [{"at": store::now(), "action": "register", "from": null, "to": "configured", "detail": spec.clone()}],
            "created_at": store::now(),
            "updated_at": store::now(),
        });
        workers.push(w.clone());
        save_workers(ctx, &workers)?;
        return Ok(w);
    }

    let id = input["worker_id"].as_str().unwrap_or_default();
    let idx = workers
        .iter()
        .position(|w| w["worker_id"].as_str() == Some(id))
        .ok_or_else(|| GenError::new("worker_not_found", format!("no such worker: {id}")))?;
    let mut w = workers[idx].clone();
    let state = w["state"].as_str().unwrap_or_default().to_string();
    let drv = remote::driver_for(ctx, &w);

    let accrue = |w: &mut Value, input: &Value| {
        if let Some(c) = input["cost_usd"].as_f64() {
            let cur = w["accrued_usd"].as_f64().unwrap_or(0.0);
            w["accrued_usd"] = json!(cur + c);
        }
    };

    // Budget gate: refuse billable transitions once the recorded spend reaches
    // the worker's cap (0 = uncapped record-keeping only).
    let over_budget = |w: &Value| {
        let b = w["budget_usd"].as_f64().unwrap_or(0.0);
        b > 0.0 && w["accrued_usd"].as_f64().unwrap_or(0.0) >= b
    };

    match action {
        "stage" => {
            if !matches!(state.as_str(), "configured" | "error") {
                return Err(GenError::new(
                    "bad_state",
                    format!("cannot stage from {state}"),
                ));
            }
            if over_budget(&w) {
                return Err(GenError::new("budget_exceeded", "worker budget reached"));
            }
            worker_event(&mut w, "stage", &state, "staging", Value::Null);
            match drv.stage(&w) {
                Ok(receipt) => {
                    accrue(&mut w, input);
                    worker_event(&mut w, "stage", "staging", "staged", receipt);
                }
                Err(e) => {
                    worker_event(
                        &mut w,
                        "stage",
                        "staging",
                        "error",
                        json!({"code": e.code, "message": e.message}),
                    );
                    workers[idx] = w;
                    save_workers(ctx, &workers)?;
                    return Err(e);
                }
            }
        }
        "reconnect" => {
            if matches!(state.as_str(), "terminated") {
                return Err(GenError::new("bad_state", "worker is terminated"));
            }
            match drv.reconnect(&w) {
                Ok(r) => worker_event(&mut w, "reconnect", &state, &state, r),
                Err(e) => {
                    worker_event(
                        &mut w,
                        "reconnect",
                        &state,
                        "error",
                        json!({"code": e.code, "message": e.message}),
                    );
                    workers[idx] = w;
                    save_workers(ctx, &workers)?;
                    return Err(e);
                }
            }
        }
        // `begin` marks compute starting on an already-staged worker. It never
        // provisions a billed instance — that is the separately authorized
        // `provision` action below.
        "begin" => {
            if state != "staged" && state != "idle" {
                return Err(GenError::new(
                    "bad_state",
                    format!("cannot begin from {state}"),
                ));
            }
            if over_budget(&w) {
                return Err(GenError::new("budget_exceeded", "worker budget reached"));
            }
            match drv.begin(&w) {
                Ok(r) => {
                    accrue(&mut w, input);
                    if let Some(o) = w.as_object_mut() {
                        o.remove("idle_since");
                        o.remove("idle_since_epoch");
                    }
                    worker_event(&mut w, "begin", &state, "running", r);
                }
                Err(e) => {
                    worker_event(
                        &mut w,
                        "begin",
                        &state,
                        "error",
                        json!({"code": e.code, "message": e.message}),
                    );
                    workers[idx] = w;
                    save_workers(ctx, &workers)?;
                    return Err(e);
                }
            }
        }
        "retrieve" => {
            if !matches!(state.as_str(), "running" | "idle" | "staged") {
                return Err(GenError::new(
                    "bad_state",
                    format!("cannot retrieve from {state}"),
                ));
            }
            worker_event(&mut w, "retrieve", &state, "retrieving", Value::Null);
            let dest = ctx.library_dir().join("rented").join(id);
            match drv.retrieve(&w, &dest) {
                Ok(files) => {
                    let count = files.len();
                    w["outputs"] = json!(files);
                    worker_event(
                        &mut w,
                        "retrieve",
                        "retrieving",
                        "retrieved",
                        json!({"dest": dest, "files": count}),
                    );
                }
                Err(e) => {
                    worker_event(
                        &mut w,
                        "retrieve",
                        "retrieving",
                        "error",
                        json!({"code": e.code, "message": e.message}),
                    );
                    workers[idx] = w;
                    save_workers(ctx, &workers)?;
                    return Err(e);
                }
            }
        }
        "idle" => {
            // A provisioned rental that is staged and waiting bills exactly like an idle running one,
            // so it may be marked idle too (and then falls under `idle_limit_min`).
            let rental_waiting =
                state == "staged" && w["rental"]["status"].as_str() == Some("active");
            if state != "running" && !rental_waiting {
                return Err(GenError::new(
                    "bad_state",
                    format!("cannot idle from {state}"),
                ));
            }
            let limit = input["idle_limit_min"].as_i64().unwrap_or(30);
            w["idle_since"] = json!(store::now());
            w["idle_since_epoch"] = json!(rental::now_epoch());
            w["idle_limit_min"] = json!(limit);
            worker_event(
                &mut w,
                "idle",
                &state,
                "idle",
                json!({"idle_limit_min": limit}),
            );
        }
        "unload" => {
            // Unload model weights but keep the worker reachable (staged).
            if !matches!(state.as_str(), "running" | "idle" | "retrieved") {
                return Err(GenError::new(
                    "bad_state",
                    format!("cannot unload from {state}"),
                ));
            }
            if let Some(o) = w.as_object_mut() {
                o.remove("idle_since");
                o.remove("idle_since_epoch");
            }
            worker_event(&mut w, "unload", &state, "staged", Value::Null);
        }
        // Vast instance create: billed — requires explicit authorized target + budget in the
        // input. The budget is RESERVED in the spend ledger first (daily cap, monthly cap, device
        // allowance); see `rental.rs` for the whole reserve / hold / settle lifecycle.
        "provision" => {
            if !matches!(state.as_str(), "configured" | "staged" | "error") {
                return Err(GenError::new(
                    "bad_state",
                    format!("cannot provision from {state}"),
                ));
            }
            if w["spec"]["driver"].as_str() != Some("vast") {
                return Err(GenError::new(
                    "invalid_input",
                    "provision applies to vast workers only",
                ));
            }
            // A worker that holds, or may hold, an instance is refused without touching its state.
            if let Some(e) = rental::blocks_new_rental(&w) {
                return Err(e);
            }
            match provision_rental(ctx, &mut workers, idx, &mut w, input) {
                Ok(v) => {
                    let inst = v["instance_id"].clone();
                    w["spec"]["vast"]["instance_id"] = inst;
                    worker_event(&mut w, "provision", &state, "staged", v);
                }
                Err(e) => {
                    worker_event(
                        &mut w,
                        "provision",
                        &state,
                        "error",
                        json!({"code": e.code, "message": e.message}),
                    );
                    workers[idx] = w;
                    save_workers(ctx, &workers)?;
                    return Err(e);
                }
            }
        }
        // Resolve a rental whose create may have been delivered: adopt the one instance carrying
        // this worker's label, or release the reservation once the user confirms none exists.
        "resolve_rental" => {
            if !matches!(
                w["rental"]["status"].as_str(),
                Some("renting" | "outcome_unknown")
            ) {
                return Err(GenError::new("bad_state", "no held rental to resolve"));
            }
            let found = remote::VastDriver::new(ctx).find_by_label(&w)?;
            match found.len() {
                1 => {
                    rental::adopt(&mut w, &found[0]);
                    worker_event(
                        &mut w,
                        "resolve_rental",
                        &state,
                        "staged",
                        json!({"adopted_instance": found[0]["id"]}),
                    );
                }
                0 if input["confirm_none"].as_bool() == Some(true) => {
                    if let Some(msg) = rental::release(
                        ctx,
                        &mut w,
                        "no instance carries the label; confirmed by the user",
                    ) {
                        return Err(GenError::new(
                            "spend_ledger",
                            format!("could not release the reservation: {msg}"),
                        ));
                    }
                    worker_event(
                        &mut w,
                        "resolve_rental",
                        &state,
                        "configured",
                        json!({"released": true}),
                    );
                }
                0 => {
                    return Err(GenError::new(
                        "rental_unresolved",
                        "no instance carries this worker's label; pass confirm_none:true only when sure the create never reached Vast (the reservation stays open until then)",
                    ));
                }
                n => {
                    let ids: Vec<Value> = found.iter().map(|i| i["id"].clone()).collect();
                    return Err(GenError::new(
                        "rental_unresolved",
                        format!("{n} instances carry this worker's label ({}); destroy the extras on Vast, then resolve again", Value::Array(ids)),
                    ));
                }
            }
        }
        // Re-check a destroy the provider did not confirm: if it says the instance does not exist
        // (404 on the instance read, or absent from the label list on two reads a minute apart) the
        // destroy counts as confirmed-gone and settles with `estimated_end`. Never sends a DELETE.
        "resolve_destroy" => {
            if w["rental"]["status"].as_str() != Some("destroy_unconfirmed") {
                return Err(GenError::new(
                    "bad_state",
                    "no unconfirmed destroy to resolve",
                ));
            }
            let drv = remote::VastDriver::new(ctx);
            match rental::check_gone(&drv, &mut w, None) {
                rental::GoneVerdict::Gone(how) => {
                    let iid = w["rental"]["instance_id"]
                        .as_i64()
                        .map(|n| n.to_string())
                        .or_else(|| {
                            w["spec"]["vast"]["instance_id"]
                                .as_i64()
                                .map(|n| n.to_string())
                        })
                        .unwrap_or_default();
                    rental::destroyed_gone(&mut w, &iid, how);
                    let settlement = rental::settle(ctx, &mut w)?;
                    worker_event(
                        &mut w,
                        "resolve_destroy",
                        &state,
                        "terminated",
                        json!({"confirmed_gone": how, "settlement": settlement}),
                    );
                }
                other => {
                    let (result, why) = match &other {
                        rental::GoneVerdict::StillPresent => ("still_exists", "the provider still lists the instance: it may still be billing; teardown retries the destroy".to_string()),
                        rental::GoneVerdict::PendingSecondRead => ("pending_second_read", "absent from the label list once; read again at least a minute later".to_string()),
                        rental::GoneVerdict::Unknown(m) => ("unknown", m.clone()),
                        rental::GoneVerdict::Gone(_) => ("gone", String::new()),
                    };
                    w["rental"]["resolve_destroy"] =
                        json!({"result": result, "detail": why, "at": store::now()});
                    worker_event(
                        &mut w,
                        "resolve_destroy",
                        &state,
                        &state,
                        json!({"result": result, "detail": why}),
                    );
                }
            }
        }
        "teardown" => {
            let vast_rental_live = w["spec"]["driver"].as_str() == Some("vast")
                && matches!(
                    w["rental"]["status"].as_str(),
                    Some("active" | "destroy_unconfirmed" | "destroyed_unsettled" | "destroying")
                );
            let force = input["force"].as_bool() == Some(true);
            let forced = state == "running" && vast_rental_live && force;
            if matches!(state.as_str(), "running" | "staging" | "retrieving") && !forced {
                let message = if vast_rental_live && state == "running" {
                    "the rental is running work and is billing; teardown would interrupt it. Pass \"force\": true to destroy it anyway (the destroy is confirmed by the provider and the spend is settled as usual)".to_string()
                } else {
                    format!("finish or cancel in-flight work before teardown (state {state})")
                };
                return Err(GenError::new("bad_state", message));
            }
            if matches!(
                w["rental"]["status"].as_str(),
                Some("renting" | "outcome_unknown")
            ) {
                return Err(GenError::new(
                    "rental_unresolved",
                    "a Vast create may have been delivered and may be billing; run resolve_rental first",
                ));
            }
            if rental::destroy_in_progress(&w, rental::now_epoch()) {
                return Err(GenError::new(
                    "destroy_in_progress",
                    "an automatic destroy of this rental is in flight; read the worker again in a moment",
                ));
            }
            // A rental the provider already confirmed destroyed and the ledger already settled has
            // nothing left to destroy: never send a second DELETE for it (an unrelated instance
            // could carry a reused id), and never touch its settlement.
            let vast_rental_settled = w["spec"]["driver"].as_str() == Some("vast")
                && w["rental"]["status"].as_str() == Some("settled");
            let result = if vast_rental_live {
                teardown_rental(ctx, &mut w)
            } else if vast_rental_settled {
                Ok(
                    json!({"released": true, "driver": "vast", "instance_id": w["rental"]["instance_id"], "already_settled": true}),
                )
            } else if drv.name() == "vast" && !w["rental"].is_object() {
                teardown_legacy_vast(ctx, &mut w)
            } else {
                drv.teardown(&w)
            };
            match result {
                Ok(mut r) => {
                    if forced {
                        r["forced"] = json!(true);
                        r["interrupted"] = json!(rental::FORCE_NOTE);
                        w["rental"]["forced_teardown"] = json!({"at": store::now(), "from_state": state, "note": rental::FORCE_NOTE});
                    }
                    worker_event(&mut w, "teardown", &state, "terminated", r)
                }
                Err(e) => {
                    worker_event(
                        &mut w,
                        "teardown",
                        &state,
                        "error",
                        json!({"code": e.code, "message": e.message}),
                    );
                    workers[idx] = w;
                    save_workers(ctx, &workers)?;
                    return Err(e);
                }
            }
        }
        "status" => {}
        other => {
            return Err(GenError::new(
                "invalid_input",
                format!("unknown worker action: {other}"),
            ))
        }
    }
    workers[idx] = w.clone();
    save_workers(ctx, &workers)?;
    Ok(w)
}

pub fn handle(ctx: &Context, name: &str, input: &Value) -> GenResult<Value> {
    match name {
        "studio.models.hardware" => Ok(hardware::detect_hardware()),
        "studio.models.catalog" => catalog(ctx),
        "studio.models.describe" => describe(ctx, input),
        "studio.models.probe" => probe(ctx, input),
        "studio.models.estimate" => estimate(ctx, input),
        "studio.models.install" => install_cmd(ctx, input),
        "studio.models.install.status" => install::status(ctx, input["id"].as_str()),
        "studio.models.install.pause" => {
            let id = input["id"].as_str().unwrap_or_default();
            install::pause(ctx, id)
        }
        "studio.models.install.cancel" => {
            let id = input["id"].as_str().unwrap_or_default();
            match find_entry(id) {
                Ok(entry) if entry["engine"].as_str() == Some("sdcpp") => sdcpp::cancel(ctx, id),
                entry => {
                    let target = entry.ok().and_then(|e| install_target(ctx, &e));
                    install::cancel(ctx, id, || match &target {
                        Some(t) => install::discard_partial(id, t),
                        None => Ok(false),
                    })
                }
            }
        }
        "studio.models.install.uninstall" => {
            let id = input["id"].as_str().unwrap_or_default();
            let entry = find_entry(id)?;
            if entry["engine"].as_str() == Some("sdcpp") {
                sdcpp::uninstall(ctx, id)
            } else {
                let target = install_target(ctx, &entry).ok_or_else(|| {
                    GenError::new("unsupported", format!("no install record layout for {id}"))
                })?;
                install::uninstall(ctx, id, || install::remove_target(id, &target))
            }
        }
        "studio.models.install.rollback" => {
            let id = input["id"].as_str().unwrap_or_default();
            let entry = find_entry(id)?;
            if entry["engine"].as_str() == Some("sdcpp") {
                sdcpp::rollback(id)
            } else {
                let target = install_target(ctx, &entry).ok_or_else(|| {
                    GenError::new("unsupported", format!("no install record layout for {id}"))
                })?;
                install::rollback(ctx, id, &target, input["revision"].as_str())
            }
        }
        "studio.models.route.get" => route_get(ctx),
        "studio.models.route.set" => route_set(ctx, input),
        "studio.models.unload" => unload(ctx, input),
        "studio.models.loaded" => governor::loaded(ctx),
        "studio.models.governor.status" => governor::status(ctx),
        "studio.models.governor.acquire" => {
            let hw = hardware::detect_hardware();
            let profile = hw["profile"].as_str().unwrap_or_default().to_string();
            let entry = input["model_id"]
                .as_str()
                .and_then(|id| find_entry(id).ok());
            governor::acquire(ctx, input, entry.as_ref(), &profile)
        }
        "studio.models.governor.release" => governor::release(ctx, input),
        "studio.models.governor.unload" => governor::unload_runtime(ctx, input),
        "studio.models.workers" => Ok(json!({"workers": load_workers(ctx)?,
                                            "driver": remote::default_driver_name()})),
        "studio.models.rental.search" => remote::VastDriver::new(ctx).search(input),
        "studio.models.worker.action" => worker_action(ctx, input),
        "studio.models.rentals" => rental::list(ctx),
        "studio.models.rentals_enforce" => rental::enforce_rentals(ctx),
        _ => Err(GenError::new(
            "unknown_command",
            format!("unknown command: {name}"),
        )),
    }
}
