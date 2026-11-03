//! Rented/remote worker drivers. The lifecycle (register → stage → begin →
//! retrieve → idle/unload → teardown) is owned by `mod.rs`; this file owns the
//! remote side behind [`RemoteDriver`]. Implementations never fabricate GPU
//! work:
//!
//! - [`NullDriver`]: no remote configured — honest `driver_not_configured`.
//! - [`SshDriver`]: real protocol over the system OpenSSH client (`ssh`/`scp`
//!   binaries — native OS tooling, no scripts) for a user-owned or rented
//!   host. `worker.spec.ssh = {host, user, port?, key?, workdir}`.
//! - [`VastDriver`]: Vast.ai REST — offer search, guarded create, poll,
//!   destroy. Create requires an explicit authorized target + budget; it is
//!   unreachable from the generic lifecycle (`provision` gates it) and is
//!   bracketed by the spend ledger in `rental.rs` (reserve before, settle at destroy).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Map, Value};

use rightkit_media_gen::host::http::{Http, HttpError, Request};
use rightkit_media_gen::{credentials, store, Context, GenError, GenResult};

/// Remote side of the rented-worker protocol.
pub trait RemoteDriver {
    fn name(&self) -> &'static str;
    fn stage(&self, worker: &Value) -> GenResult<Value>;
    fn reconnect(&self, worker: &Value) -> GenResult<Value>;
    fn begin(&self, worker: &Value) -> GenResult<Value>;
    /// Copy declared outputs to `dest`, returning sha256-verified records.
    fn retrieve(&self, worker: &Value, dest: &Path) -> GenResult<Vec<Value>>;
    fn teardown(&self, worker: &Value) -> GenResult<Value>;
}

// ---------- no remote configured ----------------------------------------------

pub struct NullDriver;

impl NullDriver {
    fn unconfigured(&self) -> GenError {
        GenError::new(
            "driver_not_configured",
            "no remote interface configured for this worker (register with spec.driver ssh|vast or set a remote driver)",
        )
    }
}

impl RemoteDriver for NullDriver {
    fn name(&self) -> &'static str {
        "none"
    }
    fn stage(&self, _w: &Value) -> GenResult<Value> {
        Err(self.unconfigured())
    }
    fn reconnect(&self, _w: &Value) -> GenResult<Value> {
        Err(self.unconfigured())
    }
    fn begin(&self, _w: &Value) -> GenResult<Value> {
        Err(self.unconfigured())
    }
    fn retrieve(&self, _w: &Value, _dest: &Path) -> GenResult<Vec<Value>> {
        Err(self.unconfigured())
    }
    fn teardown(&self, _w: &Value) -> GenResult<Value> {
        // Safe no-op: nothing was launched, so nothing can keep billing.
        Ok(json!({"released": false, "note": "no remote resources existed"}))
    }
}

// ---------- local filesystem stand-in ------------------------------------------

// ---------- SSH driver (system OpenSSH client) ---------------------------------

/// Real remote protocol over the OS OpenSSH client (`ssh`, `scp`, `sftp` —
/// available on macOS and Windows 10+). `BatchMode=yes` fails instead of
/// prompting, so an unattended call can never hang on a password prompt.
///
/// Worker spec:
/// ```json
/// "spec": {
///   "driver": "ssh",
///   "ssh": {"host": "1.2.3.4", "user": "root", "port": 22, "key": "~/.ssh/id_ed25519",
///           "workdir": "/root/<namespace>"},
///   "inputs": [{"local": "/abs/in.wav", "remote": "inputs/in.wav"}],
///   "remote_command": "python3 run.py --job job.json"
/// }
/// ```
pub struct SshDriver;

fn ssh_cfg(worker: &Value) -> GenResult<(String, Vec<String>, String)> {
    let ssh = &worker["spec"]["ssh"];
    let host = ssh["host"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| GenError::new("invalid_spec", "spec.ssh.host required"))?;
    let user = ssh["user"].as_str().unwrap_or("root");
    let workdir = ssh["workdir"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| format!("~/{}", namespace()));
    let mut args = vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=10".into(),
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
    ];
    if let Some(p) = ssh["port"].as_u64() {
        args.push("-p".into());
        args.push(p.to_string());
    }
    if let Some(k) = ssh["key"].as_str() {
        args.push("-i".into());
        args.push(k.to_string());
    }
    Ok((format!("{user}@{host}"), args, workdir))
}

/// `spec.ssh.shell` wraps each remote command — e.g. `"bash -lc"` reaches WSL
/// on a Windows sshd whose default shell is PowerShell.
fn remote_cmd(worker: &Value, cmd: &str) -> String {
    match worker["spec"]["ssh"]["shell"].as_str() {
        Some(shell) if !shell.trim().is_empty() => {
            format!("{shell} '{}'", cmd.replace('\'', "'\"'\"'"))
        }
        _ => cmd.to_string(),
    }
}

/// scp target for `remote_abs`: `spec.ssh.scp_root` is the filesystem view the
/// sftp server sees of `workdir` (Windows sshd stages under `C:\…` while the
/// `shell` commands see `/mnt/c/…`).
fn scp_dest(worker: &Value, target: &str, workdir: &str, remote_abs: &str) -> String {
    match worker["spec"]["ssh"]["scp_root"].as_str() {
        Some(root) if !root.is_empty() => {
            let rel = remote_abs.strip_prefix(workdir).unwrap_or(remote_abs);
            format!("{target}:{}{}", root.trim_end_matches('/'), rel)
        }
        _ => format!("{target}:{remote_abs}"),
    }
}

fn ssh_run(
    target: &str,
    args: &[String],
    remote: &str,
    stdin: Option<&[u8]>,
) -> GenResult<Vec<u8>> {
    let argv: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .chain([target, remote])
        .collect();
    let options = rightkit_child_process::RunOptions::new(std::time::Duration::from_secs(3600))
        .stdout_cap(256 * 1024 * 1024)
        .stderr_cap(256 * 1024 * 1024);
    let out = match stdin {
        Some(input) => rightkit_child_process::run_with_stdin("ssh", argv, input, &options),
        None => rightkit_child_process::run("ssh", argv, &options),
    }
    .map_err(|e| GenError::new("ssh", format!("ssh: {e}")))?;
    if out.timed_out {
        return Err(GenError::new(
            "ssh",
            "ssh: child exceeded its time limit and was terminated",
        ));
    }
    if !out.status.success() {
        return Err(GenError::new(
            "ssh",
            format!(
                "ssh exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr.bytes)
                    .chars()
                    .take(300)
                    .collect::<String>()
            ),
        ));
    }
    Ok(out.stdout.bytes)
}

fn scp_args(args: &[String]) -> Vec<String> {
    // -p becomes -P for scp.
    args.iter()
        .enumerate()
        .map(|(i, a)| {
            if a == "-p" && i + 1 < args.len() {
                "-P".to_string()
            } else {
                a.clone()
            }
        })
        .collect()
}

fn scp_run(args: &[String], from: &str, to: &str, recursive: bool) -> GenResult<()> {
    let mut argv = scp_args(args);
    if recursive {
        argv.push("-r".to_string());
    }
    argv.push(from.to_string());
    argv.push(to.to_string());
    let options = rightkit_child_process::RunOptions::new(std::time::Duration::from_secs(3600))
        .stdout_cap(256 * 1024 * 1024)
        .stderr_cap(256 * 1024 * 1024);
    let out = rightkit_child_process::run("scp", argv, &options)
        .map_err(|e| GenError::new("ssh_unavailable", format!("spawn scp: {e}")))?;
    if out.timed_out {
        return Err(GenError::new(
            "ssh_unavailable",
            "spawn scp: child exceeded its time limit and was terminated",
        ));
    }
    if !out.status.success() {
        return Err(GenError::new(
            "ssh",
            format!(
                "scp exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr.bytes)
                    .chars()
                    .take(300)
                    .collect::<String>()
            ),
        ));
    }
    Ok(())
}

impl RemoteDriver for SshDriver {
    fn name(&self) -> &'static str {
        "ssh"
    }

    fn stage(&self, worker: &Value) -> GenResult<Value> {
        let (target, args, workdir) = ssh_cfg(worker)?;
        ssh_run(
            &target,
            &args,
            &remote_cmd(
                worker,
                &format!("mkdir -p {workdir}/staging {workdir}/outputs"),
            ),
            None,
        )?;
        let receipt = json!({
            "worker_id": worker["worker_id"], "spec": worker["spec"],
            "driver": self.name(), "staged_at": store::now(),
        });
        ssh_run(
            &target,
            &args,
            &remote_cmd(worker, &format!("cat > {workdir}/receipt.json")),
            Some(receipt.to_string().as_bytes()),
        )?;
        // Stage declared inputs (local paths copied up to workdir/staging).
        let mut staged = Vec::new();
        for input in worker["spec"]["inputs"].as_array().into_iter().flatten() {
            let local = input["local"].as_str().unwrap_or_default();
            let remote = input["remote"].as_str().unwrap_or_default();
            if local.is_empty() || remote.is_empty() {
                continue;
            }
            let remote_abs = if remote.starts_with('/') {
                remote.to_string()
            } else {
                format!("{workdir}/staging/{remote}")
            };
            ssh_run(
                &target,
                &args,
                &remote_cmd(worker, &format!("mkdir -p \"$(dirname '{remote_abs}')\"")),
                None,
            )?;
            let dest = scp_dest(worker, &target, &workdir, &remote_abs);
            scp_run(&args, local, &dest, false)?;
            staged.push(json!({"local": local, "remote": remote_abs}));
        }
        Ok(json!({"receipt": receipt, "staged_inputs": staged}))
    }

    fn reconnect(&self, worker: &Value) -> GenResult<Value> {
        let (target, args, workdir) = ssh_cfg(worker)?;
        let body = ssh_run(
            &target,
            &args,
            &remote_cmd(worker, &format!("cat {workdir}/receipt.json")),
            None,
        )?;
        let receipt: Value = serde_json::from_slice(&body)
            .map_err(|e| GenError::new("bad_receipt", format!("remote receipt: {e}")))?;
        // Also read live run state if the remote command wrote one.
        let state = ssh_run(
            &target,
            &args,
            &remote_cmd(
                worker,
                &format!("cat {workdir}/state.json 2>/dev/null || true"),
            ),
            None,
        )
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        Ok(json!({"receipt": receipt, "remote_state": state}))
    }

    fn begin(&self, worker: &Value) -> GenResult<Value> {
        let (target, args, workdir) = ssh_cfg(worker)?;
        let command = worker["spec"]["remote_command"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                GenError::new(
                    "invalid_spec",
                    "spec.remote_command required — this crate never invents a remote workload",
                )
            })?;
        // Detached: log to run.log, pid echoed back for reconnect/diagnostics.
        // The command travels base64-encoded so arbitrary quoting survives any
        // intermediate shell (`ssh.shell` wrappers like `bash -lc`).
        let b64 = rightkit_crypto::base64_encode(command.as_bytes());
        let out = ssh_run(
            &target,
            &args,
            &remote_cmd(
                worker,
                &format!(
                    "cd {workdir} && echo {b64} | base64 -d > run.sh && (nohup sh run.sh > run.log 2>&1 & disown) && sleep 0.3 && echo launched"
                ),
            ),
            None,
        )?;
        let pid = String::from_utf8_lossy(&out).trim().to_string();
        Ok(json!({"launched": true, "remote_pid": pid, "command": command}))
    }

    fn retrieve(&self, worker: &Value, dest: &Path) -> GenResult<Vec<Value>> {
        let (target, args, workdir) = ssh_cfg(worker)?;
        std::fs::create_dir_all(dest)
            .map_err(|e| GenError::new("io", format!("create {}: {e}", dest.display())))?;
        // Remote manifest: sha256sum (Linux) or shasum -a 256 (macOS/BSD).
        let manifest_raw = ssh_run(
            &target,
            &args,
            &remote_cmd(
                worker,
                &format!("cd {workdir}/outputs && (sha256sum * 2>/dev/null || shasum -a 256 *)"),
            ),
            None,
        )?;
        let manifest: Map<String, Value> = String::from_utf8_lossy(&manifest_raw)
            .lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                let sha = it.next()?;
                let name = it.next()?.trim_start_matches('*');
                Some((name.to_string(), json!(sha)))
            })
            .collect();
        let src = scp_dest(worker, &target, &workdir, &format!("{workdir}/outputs"));
        scp_run(&args, &src, &dest.to_string_lossy(), true)?;
        // scp -r of a directory lands as dest/outputs/.
        let landed = dest.join("outputs");
        let dir = if landed.is_dir() {
            landed
        } else {
            dest.to_path_buf()
        };
        let mut got = Vec::new();
        for entry in std::fs::read_dir(&dir)
            .map_err(|e| GenError::new("io", format!("read {}: {e}", dir.display())))?
            .flatten()
        {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let bytes = std::fs::read(&path)
                .map_err(|e| GenError::new("io", format!("read {}: {e}", path.display())))?;
            let sha = rightkit_fs::digest::sha256_bytes(&bytes);
            let expected = manifest
                .get(&name)
                .and_then(|m| m.as_str())
                .map(String::from);
            let verified = expected
                .as_deref()
                .map(|e| e.eq_ignore_ascii_case(&sha))
                .unwrap_or(false);
            got.push(json!({
                "file": name, "path": path, "bytes": bytes.len(),
                "sha256": sha, "expected_sha256": expected, "hash_verified": verified,
            }));
        }
        if got.is_empty() {
            return Err(GenError::new(
                "no_outputs",
                "remote produced no outputs to retrieve",
            ));
        }
        Ok(got)
    }

    fn teardown(&self, worker: &Value) -> GenResult<Value> {
        let (target, args, workdir) = ssh_cfg(worker)?;
        // Outputs must be retrieved first; teardown removes the whole workdir
        // so nothing keeps occupying remote disk or billing storage.
        ssh_run(
            &target,
            &args,
            &remote_cmd(worker, &format!("rm -rf {workdir}")),
            None,
        )?;
        Ok(json!({"released": true, "driver": self.name(), "removed": workdir}))
    }
}

// ---------- Vast.ai driver ------------------------------------------------------

/// Vast.ai REST driver. Offer search and instance lifecycle only; **create is
/// gated**: `provision` requires `authorized: true` plus budget fields naming
/// the exact target — and is still only reachable through explicit command
/// input, never from automatic flows.
///
/// Spec: `"spec": {"driver": "vast", "vast": {"image": "pytorch/pytorch:...",
/// "disk_gb": 40, "onstart": "..."}, "remote_command": "..."}`.
///
/// The API key is resolved by [`resolve_key`] the way the other providers resolve theirs: the
/// stored credential (OS credential store) first, then `VAST_API_KEY`, then (a create or an offer
/// search only) an `api_key` input. A rental can only be CREATED while a key is stored
/// ([`key_is_stored`]), so the app that rents can always destroy later without an environment
/// variable. The key is never written to a worker record, an event, a log or an error.
pub struct VastDriver {
    base: String,
    ctx: Context,
}

/// The credential-store provider id of the Vast key.
pub const KEY_PROVIDER: &str = "vast";

fn nonempty_trim(s: Option<String>) -> Option<String> {
    s.map(|k| k.trim().to_string()).filter(|k| !k.is_empty())
}

/// The stored Vast key (credential store only; the environment is [`resolve_key`]'s next step).
pub fn stored_key(ctx: &Context) -> GenResult<Option<String>> {
    credentials::get(ctx, KEY_PROVIDER)
}

/// True when a Vast key is held in the credential store AND can be read back by this process
/// without a prompt. The environment variable does not count: enforcement may run in a process
/// that does not have it.
pub fn key_is_stored(ctx: &Context) -> bool {
    credentials::status_source(ctx, KEY_PROVIDER) == Some("keychain")
        && matches!(credentials::get(ctx, KEY_PROVIDER), Ok(Some(k)) if !k.is_empty())
}

/// Save (or, with an empty key, delete) the Vast key in the credential store and read it back.
/// Errors never contain the key.
pub fn store_key(ctx: &Context, key: &str) -> GenResult<()> {
    let key = key.trim();
    credentials::set(ctx, KEY_PROVIDER, key)?;
    if !key.is_empty()
        && !(key_is_stored(ctx)
            && matches!(credentials::get(ctx, KEY_PROVIDER), Ok(Some(ref k)) if k == key))
    {
        return Err(GenError::new(
            "keychain",
            "the Vast key was written but could not be read back from the credential store, so it was not retained; enforcement could not use it",
        ));
    }
    Ok(())
}

/// Resolve the Vast key: stored credential, then `VAST_API_KEY`, then (when `input` is given) its
/// `api_key`. Every Vast call (create, status, find-by-label, destroy) goes through here; only a
/// create and an offer search pass an input.
pub fn resolve_key(ctx: &Context, input: Option<&Value>) -> GenResult<String> {
    if let Ok(Some(k)) = stored_key(ctx) {
        return Ok(k);
    }
    if let Some(k) = nonempty_trim(std::env::var("VAST_API_KEY").ok()) {
        return Ok(k);
    }
    if let Some(k) = input.and_then(|i| nonempty_trim(i["api_key"].as_str().map(String::from))) {
        return Ok(k);
    }
    Err(GenError::new(
        "no_credentials",
        "no Vast API key: save it in the app Settings (worker.action set_vast_key), or set VAST_API_KEY; an api_key input is accepted for a create or an offer search only and is never stored",
    ))
}

impl VastDriver {
    pub fn new(ctx: &Context) -> Self {
        Self {
            base: rightkit_media_gen::providers::base_url_override(
                "VAST_API_BASE",
                "https://console.vast.ai",
            ),
            ctx: ctx.clone(),
        }
    }

    /// The key for one Vast call (see [`resolve_key`]).
    fn key(&self, input: Option<&Value>) -> GenResult<String> {
        resolve_key(&self.ctx, input)
    }

    /// The API base is `VAST_API_BASE` in development builds only (production always uses
    /// `https://console.vast.ai`). A base without a parseable host falls back to the public-web policy.
    /// The Vast API is a public host; loopback (the e2e mock) is already allowed.
    fn http_for(_base: &str) -> Http {
        Http::new(Duration::from_secs(10), Duration::from_secs(30))
    }

    fn api(
        &self,
        method: &str,
        path: &str,
        key: &str,
        body: Option<&Value>,
    ) -> GenResult<(u16, Value)> {
        self.api_raw(method, path, key, body).map_err(|f| f.error)
    }

    /// As `Self::api`, and says whether a failure may have reached Vast (so a billed call that
    /// fails is held, not retried or released). Nothing here retries: `Http::send` sends once.
    fn api_raw(
        &self,
        method: &str,
        path: &str,
        key: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Value), ApiFail> {
        let url = format!("{}{}", self.base, path);
        let http = Self::http_for(&self.base);
        let auth = format!("Bearer {key}");
        let not_sent = |e: HttpError| ApiFail {
            error: GenError::new("transport", redact(&format!("vast api: {e}"), key)),
            delivered: false,
        };
        let base = match method {
            "GET" => Request::get(&url),
            "DELETE" => Request::delete(&url),
            "PUT" => Request::put(&url),
            "POST" => Request::post(&url),
            _ => {
                return Err(ApiFail {
                    error: GenError::new("invalid_input", format!("unsupported method {method}")),
                    delivered: false,
                })
            }
        };
        let req = base.secret_header("Authorization", &auth);
        // Only PUT and POST carry a JSON body; GET and DELETE ignore `body`, as before.
        let req = match (method, body) {
            ("PUT" | "POST", Some(b)) => req.json(b).map_err(not_sent)?,
            _ => req,
        };
        let resp = http.send(req).map_err(|e| ApiFail {
            delivered: rightkit_media_gen::providers::error_may_have_reached(&e),
            error: GenError::new("transport", redact(&format!("vast api: {e}"), key)),
        })?;
        let status = resp.status;
        // The reply is capped at rightkit's default, 32 MiB. A reply that cannot be read was sent.
        let v: Value = resp.json().map_err(|e| ApiFail {
            error: GenError::new("bad_response", format!("vast api non-JSON: {e}")),
            delivered: true,
        })?;
        Ok((status, v))
    }

    /// `studio.models.rental.search` — query offers. Needs an API key (stored credential,
    /// `VAST_API_KEY`, or the input `api_key`); missing key is an honest `no_credentials`,
    /// never a fabricated offer list.
    pub fn search(&self, input: &Value) -> GenResult<Value> {
        let key = self.key(Some(input))?;
        let q = if input["query"].is_object() {
            input["query"].clone()
        } else {
            json!({
                "type": "on-demand", "rentable": {"eq": true}, "num_gpus": {"eq": 1},
                "order": [["dph_total", "asc"]], "limit": 20,
            })
        };
        let (status, v) = self.api(
            "GET",
            &format!("/api/v0/bundles/?q={}", urlencoded(&q.to_string())),
            &key,
            None,
        )?;
        if status != 200 {
            return Err(GenError::new("vast", format!("offer search HTTP {status}")));
        }
        let offers: Vec<Value> = v["offers"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|o| {
                json!({
                    "offer_id": o["id"], "gpu": o["gpu_name"], "num_gpus": o["num_gpus"],
                    "vram_gb": o["gpu_ram"].as_f64().map(|b| b / 1024.0),
                    "dph_total": o["dph_total"], "disk_gb": o["disk_space"],
                    "reliability": o["reliability2"], "verified": o["verified"],
                    "geolocation": o["geolocation"],
                })
            })
            .collect();
        Ok(json!({"offers": offers}))
    }

    /// Validate a rental request without touching the network: `authorized: true`, `offer_id`,
    /// `max_usd_hr`, `budget_usd` (present, finite, positive), `max_duration_min`, and a Vast key
    /// STORED in the credential store (so enforcement can always destroy what is rented). The caller
    /// reserves `budget_usd` in the spend ledger BEFORE [`Self::quote`] and [`Self::create`].
    pub fn rent_params(ctx: &Context, input: &Value) -> GenResult<RentParams> {
        if input["authorized"].as_bool() != Some(true) {
            return Err(GenError::new(
                "not_authorized",
                "billed instance create requires authorized:true with an explicit target and budget",
            ));
        }
        let offer_id = input["offer_id"]
            .as_i64()
            .ok_or_else(|| GenError::new("invalid_input", "offer_id required"))?;
        let max_hr = input["max_usd_hr"]
            .as_f64()
            .ok_or_else(|| GenError::new("invalid_input", "max_usd_hr required"))?;
        let budget = input["budget_usd"]
            .as_f64()
            .ok_or_else(|| GenError::new("invalid_input", "budget_usd required"))?;
        let max_min = input["max_duration_min"]
            .as_f64()
            .ok_or_else(|| GenError::new("invalid_input", "max_duration_min required"))?;
        if !(budget.is_finite() && budget > 0.0) || !(max_min.is_finite() && max_min > 0.0) {
            return Err(GenError::new(
                "invalid_input",
                "budget_usd and max_duration_min must be positive",
            ));
        }
        // A NaN or infinite hourly ceiling would make the quote's `dph > max_hr` check never refuse.
        if !(max_hr.is_finite() && max_hr > 0.0) {
            return Err(GenError::new(
                "invalid_input",
                "max_usd_hr must be a positive finite number",
            ));
        }
        if !key_is_stored(ctx) {
            return Err(GenError::new(
                "no_stored_key",
                "refused before anything was reserved or sent: a rental needs the Vast API key saved in the app first, so the app can always destroy what it rents. Save the Vast key in Settings first (worker.action set_vast_key), then rent again",
            ));
        }
        let key = resolve_key(ctx, Some(input))?;
        // A rental authorisation commits spend with no further approval: leave an audit row (never the key).
        rightkit_media_gen::ledger::audit_setting(
            &store::open(ctx)?,
            "rental.authorized",
            &json!({"offer_id": offer_id, "max_usd_hr": max_hr, "budget_usd": budget, "max_duration_min": max_min}),
        )?;
        Ok(RentParams {
            key,
            offer_id,
            max_hr,
            budget,
            max_min,
        })
    }

    /// Re-quote the offer so the caps are checked against live pricing. Read-only (GET): safe to
    /// release the reservation on any failure here.
    pub fn quote(&self, p: &RentParams) -> GenResult<f64> {
        let (status, offers) = self.api(
            "GET",
            &format!(
                "/api/v0/bundles/?q={}",
                urlencoded(&json!({"id": {"eq": p.offer_id}}).to_string())
            ),
            &p.key,
            None,
        )?;
        if status != 200 {
            return Err(GenError::new(
                "vast",
                format!("offer re-quote HTTP {status}"),
            ));
        }
        let offer = offers["offers"]
            .as_array()
            .and_then(|a| a.first().cloned())
            .ok_or_else(|| GenError::new("vast", "offer no longer available"))?;
        let dph = offer["dph_total"].as_f64().unwrap_or(f64::MAX);
        if dph > p.max_hr {
            return Err(GenError::new(
                "price_cap",
                format!(
                    "offer {} costs ${dph:.3}/hr, above max_usd_hr {}",
                    p.offer_id, p.max_hr
                ),
            ));
        }
        let worst = dph * (p.max_min / 60.0);
        if worst > p.budget {
            return Err(GenError::new(
                "budget_exceeded",
                format!(
                    "{} min at ${dph:.3}/hr ≈ ${worst:.2} exceeds budget_usd {}",
                    p.max_min, p.budget
                ),
            ));
        }
        Ok(dph)
    }

    /// The billed create: ONE PUT, never retried. `delivered` on the error says the request may
    /// have reached Vast (timeout after send, 5xx/408/429, an unreadable or id-less 2xx reply);
    /// the caller then HOLDS the reservation. A refused connection or a definite 4xx is not
    /// delivered and the caller releases.
    pub fn create(&self, worker: &Value, p: &RentParams, dph: f64) -> Result<Value, RentError> {
        let body = json!({
            "client_id": namespace(),
            "image": worker["spec"]["vast"]["image"].as_str().unwrap_or("pytorch/pytorch:latest"),
            "disk": worker["spec"]["vast"]["disk_gb"].as_f64().unwrap_or(20.0),
            "onstart": lease_onstart(worker["spec"]["vast"]["onstart"].as_str(), p.max_min),
            "runtype": "args",
            "label": rental_label(worker),
        });
        let (status, v) = self
            .api_raw(
                "PUT",
                &format!("/api/v0/asks/{}/", p.offer_id),
                &p.key,
                Some(&body),
            )
            .map_err(|f| RentError {
                error: f.error,
                delivered: f.delivered,
            })?;
        if !(200..300).contains(&status) {
            let body = redact(&v.to_string().chars().take(300).collect::<String>(), &p.key);
            return Err(RentError {
                error: GenError::new("vast", format!("create HTTP {status}: {body}")),
                delivered: matches!(status, 408 | 429 | 500..=599),
            });
        }
        if v["new_contract"].as_i64().is_none() {
            return Err(RentError {
                error: GenError::new(
                    "vast",
                    "create answered 2xx without an instance id; an instance may exist",
                ),
                delivered: true,
            });
        }
        Ok(json!({
            "instance_id": v["new_contract"], "offer_id": p.offer_id,
            "dph_total": dph, "budget_usd": p.budget, "max_duration_min": p.max_min,
            "remote_lease_sec": lease_seconds(p.max_min),
            "note": "recorded; ssh/jupyter access details come from instance status",
        }))
    }

    /// Instances on the account whose label is this worker's rental label (read-only GET). Used to
    /// resolve a held rental (adopt the one instance, or confirm there is none), to verify
    /// ownership and to check whether a destroyed instance is gone.
    pub fn find_by_label(&self, worker: &Value) -> GenResult<Vec<Value>> {
        let key = self.key(None)?;
        self.find_by_label_with(worker, &key)
    }

    fn find_by_label_with(&self, worker: &Value, key: &str) -> GenResult<Vec<Value>> {
        let (status, v) = self.api("GET", "/api/v0/instances/", key, None)?;
        if status != 200 {
            return Err(GenError::new(
                "vast",
                format!("instance list HTTP {status}"),
            ));
        }
        let label = rental_label(worker);
        Ok(v["instances"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|i| i["label"].as_str() == Some(label.as_str()))
            .cloned()
            .collect())
    }

    /// Read one instance (GET). `Gone` is the provider's 404; any other non-200 is an error.
    pub fn read_instance(&self, id: &str) -> GenResult<InstanceRead> {
        let key = self.key(None)?;
        self.read_instance_with(id, &key)
    }

    fn read_instance_with(&self, id: &str, key: &str) -> GenResult<InstanceRead> {
        let (status, v) = self.api("GET", &format!("/api/v0/instances/{id}/"), key, None)?;
        match status {
            200 => Ok(InstanceRead::Present(v)),
            404 => Ok(InstanceRead::Gone),
            s => Err(GenError::new("vast", format!("instance read HTTP {s}"))),
        }
    }

    /// Ownership: the instance must carry THIS worker's label. The instance read is used when it
    /// carries a label; otherwise the labelled list must contain the id. Anything else refuses
    /// (`instance_not_owned`): nothing is destroyed on a label the code cannot prove is ours.
    fn verify_owned(&self, worker: &Value, id: &str, seen: &Value, key: &str) -> GenResult<()> {
        let label = rental_label(worker);
        let not_owned = |why: &str| {
            GenError::new(
                "instance_not_owned",
                format!("instance {id} is not proven to carry this worker's label ({label}): {why}; nothing was destroyed"),
            )
        };
        match seen["instances"]["label"].as_str() {
            Some(l) if l == label => Ok(()),
            Some(_) => Err(not_owned("it carries a different label")),
            None => {
                let mine = self.find_by_label_with(worker, key)?;
                let listed = mine.iter().any(|i| {
                    i["id"].as_i64().map(|n| n.to_string()).as_deref() == Some(id)
                        || i["id"].as_str() == Some(id)
                });
                if listed {
                    Ok(())
                } else {
                    Err(not_owned("the provider's reply carries no label for it and it is not in this worker's labelled list"))
                }
            }
        }
    }

    /// Destroy the instance. Success means Vast answered 2xx to the DELETE, nothing less. Before the
    /// DELETE it READS the instance: a 404 is `instance_gone` (nothing sent); a label that is not
    /// this worker's is `instance_not_owned` (nothing sent); otherwise the read supplies the
    /// provider's start time and hourly rate, which the caller settles the rental from. The key is
    /// resolved by [`resolve_key`] (stored credential, then `VAST_API_KEY`).
    pub fn destroy(&self, worker: &Value) -> GenResult<DestroyFacts> {
        let id = self.instance(worker)?;
        let key = self.key(None)?;
        let seen = match self.read_instance_with(&id, &key)? {
            InstanceRead::Gone => {
                return Err(GenError::new(
                    "instance_gone",
                    format!("the provider reports instance {id} does not exist (404)"),
                ));
            }
            InstanceRead::Present(v) => v,
        };
        self.verify_owned(worker, &id, &seen, &key)?;
        let (status, _v) = self.api("DELETE", &format!("/api/v0/instances/{id}/"), &key, None)?;
        if status == 404 {
            return Err(GenError::new(
                "instance_gone",
                format!("the provider answered 404 to the destroy of instance {id}"),
            ));
        }
        if !(200..300).contains(&status) {
            return Err(GenError::new("vast", format!("destroy HTTP {status}")));
        }
        // Read-back: a 2xx DELETE is not proof billing stopped. The instance list must no longer
        // carry the id. Vast documents no enumerated status values and no 404 for an instance read,
        // so absence from the account's instance list is the only read-back used.
        self.confirm_absent(&id, &key)?;
        Ok(DestroyFacts {
            instance_id: id,
            start_epoch: seen["instances"]["start_date"]
                .as_f64()
                .filter(|t| *t > 0.0),
            dph_total: seen["instances"]["dph_total"]
                .as_f64()
                .filter(|d| d.is_finite() && *d >= 0.0),
        })
    }

    /// Fails `destroy_unconfirmed` while the account's instance list still carries `id` (the
    /// rental may still bill), and on any list failure (absence is then unproven).
    fn confirm_absent(&self, id: &str, key: &str) -> GenResult<()> {
        let (status, v) = self.api("GET", "/api/v0/instances/", key, None)?;
        if status != 200 {
            return Err(GenError::new(
                "destroy_unconfirmed",
                format!("DELETE was accepted but the read-back of the instance list answered HTTP {status}; instance {id} is not confirmed gone and may still bill"),
            ));
        }
        let still = v["instances"].as_array().into_iter().flatten().any(|i| {
            i["id"].as_i64().map(|n| n.to_string()).as_deref() == Some(id)
                || i["id"].as_str() == Some(id)
        });
        if still {
            return Err(GenError::new(
                "destroy_unconfirmed",
                format!("DELETE was accepted but instance {id} is still listed on the account; it may still bill"),
            ));
        }
        Ok(())
    }

    /// Gate for every SSH action on a rented instance (stage, begin, retrieve): the stored record's
    /// instance id must exist at Vast, carry THIS worker's label (the lease id), agree with the
    /// worker's rental record, and report the same ssh host and port the record will connect to.
    /// Nothing is sent over SSH unless all of that holds. Returns the worker with `spec.ssh` set.
    fn verified_ssh_worker(&self, worker: &Value) -> GenResult<Value> {
        let id = self.instance(worker)?;
        if let Some(rec) = worker["rental"]["instance_id"].as_i64() {
            if rec.to_string() != id {
                return Err(GenError::new(
                    "instance_record_mismatch",
                    format!("the worker's rental record names instance {rec} but spec.vast.instance_id is {id}; nothing was sent over ssh"),
                ));
            }
        }
        let key = self.key(None)?;
        let seen = match self.read_instance_with(&id, &key)? {
            InstanceRead::Gone => {
                return Err(GenError::new("instance_gone", format!("the provider reports instance {id} does not exist (404); nothing was sent over ssh")));
            }
            InstanceRead::Present(v) => v,
        };
        self.verify_owned(worker, &id, &seen, &key).map_err(|e| {
            GenError::new(
                e.code.clone(),
                e.message
                    .replace("nothing was destroyed", "nothing was sent over ssh"),
            )
        })?;
        let w = with_ssh_target(worker)?;
        let inst = &seen["instances"];
        if let Some(h) = inst["ssh_host"].as_str().filter(|h| !h.is_empty()) {
            if w["spec"]["ssh"]["host"].as_str() != Some(h) {
                return Err(GenError::new("instance_record_mismatch", format!("instance {id} reports a different ssh host than the stored record; nothing was sent over ssh")));
            }
        }
        if let Some(p) = inst["ssh_port"].as_u64() {
            if w["spec"]["ssh"]["port"].as_u64() != Some(p) {
                return Err(GenError::new("instance_record_mismatch", format!("instance {id} reports a different ssh port than the stored record; nothing was sent over ssh")));
            }
        }
        Ok(w)
    }

    fn instance(&self, worker: &Value) -> GenResult<String> {
        worker["spec"]["vast"]["instance_id"]
            .as_i64()
            .map(|i| i.to_string())
            .ok_or_else(|| GenError::new("not_provisioned", "worker has no vast instance_id"))
    }
}

/// A validated rental request (see [`VastDriver::rent_params`]). `key` never leaves the process.
pub struct RentParams {
    key: String,
    pub offer_id: i64,
    pub max_hr: f64,
    pub budget: f64,
    pub max_min: f64,
}

/// A failed billed create and whether the request may have reached Vast.
pub struct RentError {
    pub error: GenError,
    pub delivered: bool,
}

/// The provider's answer to an instance read.
pub enum InstanceRead {
    Present(Value),
    /// 404: the provider says the instance does not exist.
    Gone,
}

/// What the provider said about an instance just before it was destroyed.
pub struct DestroyFacts {
    pub instance_id: String,
    /// Provider-reported start (unix seconds), when the status reply carried one.
    pub start_epoch: Option<f64>,
    /// Provider-reported hourly rate for the instance, when the status reply carried one.
    pub dph_total: Option<f64>,
}

struct ApiFail {
    error: GenError,
    delivered: bool,
}

/// Margin added to the declared duration before the instance-side lease fires, so the local
/// enforcer (which destroys at exactly `max_duration_min` and settles the ledger) normally wins.
pub const LEASE_MARGIN_SEC: u64 = 600;

/// The instance-side lease length: declared duration plus [`LEASE_MARGIN_SEC`].
pub fn lease_seconds(max_min: f64) -> u64 {
    (max_min * 60.0).ceil().max(1.0) as u64 + LEASE_MARGIN_SEC
}

/// The `onstart` script sent with the create: a detached self-destruct deadline that runs ON the
/// instance, so billing stops even when the app is closed or the machine is offline. It destroys
/// the instance with the per-instance `CONTAINER_ID` / `CONTAINER_API_KEY` Vast injects (CLI first,
/// REST DELETE as fallback). Any user `onstart` follows. Whether Vast injects those variables
/// into this image is verified only by a live run.
pub fn lease_onstart(user: Option<&str>, max_min: f64) -> String {
    let secs = lease_seconds(max_min);
    let lease = format!(
        "#!/bin/bash
# RightKit remote-compute lease: self-destruct after {secs}s
(nohup setsid bash -c 'sleep {secs}; vastai destroy instance \"$CONTAINER_ID\" --api-key \"$CONTAINER_API_KEY\" || curl -s -X DELETE -H \"Authorization: Bearer $CONTAINER_API_KEY\" \"https://console.vast.ai/api/v0/instances/$CONTAINER_ID/\"' >/dev/null 2>&1 &)
"
    );
    match user.map(str::trim).filter(|u| !u.is_empty()) {
        Some(u) => format!(
            "{lease}{u}
"
        ),
        None => lease,
    }
}

/// The host settings namespace: the rental label prefix, the Vast `client_id` and the default
/// remote workdir name ([`rightkit_media_gen::host::HostConfig::settings_namespace`]).
pub fn namespace() -> &'static str {
    rightkit_media_gen::host::config()
        .settings_namespace
        .as_str()
}

/// The label every rental of this worker carries, and the key a held rental is resolved by.
pub fn rental_label(worker: &Value) -> String {
    format!(
        "{}-{}",
        namespace(),
        worker["worker_id"].as_str().unwrap_or("worker")
    )
}

/// Copy of the worker record with `spec.ssh` populated from `spec.vast`'s
/// reported instance address; fails closed until an instance exists.
fn with_ssh_target(worker: &Value) -> GenResult<Value> {
    let vast = &worker["spec"]["vast"];
    let host = vast["ssh_host"].as_str().ok_or_else(|| {
        GenError::new(
            "not_provisioned",
            "instance has no ssh_host yet — provision, then refresh status",
        )
    })?;
    let mut w = worker.clone();
    w["spec"]["ssh"] = json!({
        "host": host,
        "user": vast["ssh_user"].as_str().unwrap_or("root"),
        "port": vast["ssh_port"].as_u64().unwrap_or(22),
        "key": vast["ssh_key"].as_str().unwrap_or("~/.ssh/id_ed25519"),
        "workdir": vast["workdir"].as_str().map(str::to_string).unwrap_or_else(|| format!("/root/{}", namespace())),
    });
    Ok(w)
}

/// Replaces the API key in error text (same rule as avatars.rs and voices.rs).
fn redact(s: &str, key: &str) -> String {
    // The owner removes the key and its percent- and JSON-encoded forms (`rightkit_logs::KnownSecrets`); an empty key changes nothing.
    rightkit_logs::KnownSecrets::from_values([key]).redact(s)
}

fn urlencoded(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect()
}

/// The Vast instance exposes SSH; once provisioned the lifecycle runs through
/// the SSH driver pointed at the instance's reported ssh_host/ssh_port.
impl RemoteDriver for VastDriver {
    fn name(&self) -> &'static str {
        "vast"
    }
    fn stage(&self, worker: &Value) -> GenResult<Value> {
        // Delegate to SSH against the provisioned instance address.
        let w = self.verified_ssh_worker(worker)?;
        SshDriver.stage(&w)
    }
    fn reconnect(&self, worker: &Value) -> GenResult<Value> {
        let _ = self.instance(worker)?;
        // Poll instance status through the API (a poll never invents connectivity); the key is
        // resolved like every other Vast call.
        let key = self.key(None)?;
        let id = self.instance(worker)?;
        let (status, v) = self.api("GET", &format!("/api/v0/instances/{id}/"), &key, None)?;
        if status != 200 {
            return Err(GenError::new(
                "vast",
                format!("instance status HTTP {status}"),
            ));
        }
        Ok(json!({"instance": {
            "id": id, "status": v["instances"]["cur_state"],
            "ssh_host": v["instances"]["ssh_host"], "ssh_port": v["instances"]["ssh_port"],
            "dph_total": v["instances"]["dph_total"],
        }}))
    }
    fn begin(&self, worker: &Value) -> GenResult<Value> {
        SshDriver.begin(&self.verified_ssh_worker(worker)?)
    }
    fn retrieve(&self, worker: &Value, dest: &Path) -> GenResult<Vec<Value>> {
        SshDriver.retrieve(&self.verified_ssh_worker(worker)?, dest)
    }
    fn teardown(&self, worker: &Value) -> GenResult<Value> {
        let f = self.destroy(worker)?;
        Ok(json!({"released": true, "driver": self.name(), "instance_id": f.instance_id}))
    }
}

// ---------- selection -----------------------------------------------------------

/// Driver for a worker record: `spec.driver` wins; otherwise the honest null driver.
pub fn driver_for(ctx: &Context, worker: &Value) -> Box<dyn RemoteDriver> {
    match worker["spec"]["driver"].as_str() {
        Some("ssh") => Box::new(SshDriver),
        Some("vast") => Box::new(VastDriver::new(ctx)),
        _ => Box::new(NullDriver),
    }
}

/// Default driver for reads that have no worker record yet (e.g. `workers`).
pub fn default_driver_name() -> &'static str {
    "none"
}
