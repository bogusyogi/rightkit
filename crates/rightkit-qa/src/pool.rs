//! The machine-wide scenario pool (ADR-0001 item 2, Sage C2, C5 and C10): a work queue, per-scenario
//! worlds and resource tokens.
//!
//! * **Size.** [`size_pool`] picks the worker count: `[host] workers` when set, else
//!   `min(cores, commit headroom / configured peak per scenario)` capped by a per-OS start value
//!   ([`start_workers`]: Dell 2, Mac 4) until the real peaks are measured. Commit headroom is
//!   `ullAvailPageFile` from `GlobalMemoryStatusEx` on Windows and available memory elsewhere.
//! * **Scenario slots (`World`).** Every scenario gets a [`World`] ([`WorldPlan::world`]): a temp dir,
//!   a port range, a control socket and control token file path, and an env marker, all under the run
//!   root and none shared with another scenario. A slot holds no app data: the app runs on the owner's
//!   real data under the journey run's snapshot and restore ([`crate::snapshot`], rightkit#1437), so
//!   there is no throwaway home or data directory. [`world_env`] exports a slot as environment
//!   variables and [`world_from_env`] reads it back. [`world_workspace`] turns it into the
//!   [`QaWorkspace`] one in-process scenario runs in.
//! * **Tokens.** `focus`, `audio`, `gui-session`, `clipboard` and `credentials` are in-process locks the
//!   pool holds for a scenario that declares them. Two scenarios that name the same token never
//!   overlap; a scenario that names none takes no lock and never waits. A worker takes all of a
//!   scenario's tokens at once or none, so two scenarios cannot deadlock on each other, and a panic
//!   releases them (the claim is released on drop).
//!
//! The pool is generic over what runs one scenario: [`Pool::run`] hands the runner the job index, the
//! job and its world and collects one result per job in order. The suite passes a function that runs
//! the scenario in this process; W2-D replaces it with "write `scenario.json`, spawn the worker,
//! read `outcome.json`" (`crate::worker`) without touching the queue, the worlds or the tokens.
use crate::config::HostConfig;
use crate::util::{err, plain_path, Error, Result};
use crate::worker::{Deadlines, EnvMarker, ScenarioSpec, Token, World, SCHEMA_VERSION};
use crate::workspace::QaWorkspace;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard};

// ------------------------------------------------------------------------------------------ sizing

/// Peak memory of one scenario, in MB, until `[host] peak_mb` or a measurement says otherwise.
pub const DEFAULT_PEAK_MB: u64 = 1024;
/// Start value for the pool size on Windows (the Dell) until peaks are measured (ADR-0001 item 2).
pub const START_WORKERS_WINDOWS: usize = 2;
/// Start value for the pool size on macOS until peaks are measured (ADR-0001 item 2).
pub const START_WORKERS_MACOS: usize = 4;
/// Start value on any other OS.
pub const START_WORKERS_OTHER: usize = 2;

/// The per-OS start value: a ceiling on a computed pool size, not on `[host] workers`.
pub fn start_workers() -> usize {
    if cfg!(windows) {
        START_WORKERS_WINDOWS
    } else if cfg!(target_os = "macos") {
        START_WORKERS_MACOS
    } else {
        START_WORKERS_OTHER
    }
}

/// Everything [`size_pool`] decides from. Separate from the host probe so the rule is testable.
#[derive(Debug, Clone, Copy)]
pub struct SizeInputs<'a> {
    /// `[host] workers`.
    pub configured: Option<usize>,
    pub cores: usize,
    /// Commit headroom in MB; `None` when it could not be read (then memory does not limit).
    pub headroom_mb: Option<u64>,
    /// Configured peak memory of one scenario, in MB.
    pub peak_mb: u64,
    /// Ceiling on a computed size, see [`start_workers`].
    pub start_cap: usize,
    /// Scenarios to run; the pool never has more workers than jobs.
    pub jobs: usize,
    /// A reason the run must be serial unless `[host] workers` says otherwise.
    pub serial: Option<&'a str>,
}

/// The pool size and the rule that gave it, for the run log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolSize {
    pub workers: usize,
    pub reason: String,
}

/// Pick the worker count. An explicit `[host] workers` always wins (clamped to at least 1 and to the
/// job count). Otherwise `min(cores, headroom / peak, start cap)`, or 1 when a serial reason is given.
pub fn size_pool(i: &SizeInputs<'_>) -> PoolSize {
    let jobs = i.jobs.max(1);
    if let Some(n) = i.configured {
        let workers = n.clamp(1, jobs);
        return PoolSize {
            workers,
            reason: format!("[host] workers = {n}"),
        };
    }
    if let Some(why) = i.serial {
        return PoolSize {
            workers: 1,
            reason: format!("serial: {why}"),
        };
    }
    let cores = i.cores.max(1);
    let by_memory = i
        .headroom_mb
        .map(|h| usize::try_from((h / i.peak_mb.max(1)).max(1)).unwrap_or(usize::MAX));
    let mut workers = cores.min(i.start_cap.max(1));
    if let Some(m) = by_memory {
        workers = workers.min(m);
    }
    PoolSize {
        workers: workers.clamp(1, jobs),
        reason: format!(
            "min(cores {cores}, headroom {} / peak {} MB, start value {})",
            i.headroom_mb
                .map(|h| format!("{h} MB"))
                .unwrap_or_else(|| "unknown".into()),
            i.peak_mb,
            i.start_cap
        ),
    }
}

/// The pool size of a run on this host. Scenarios run on the owner's real data (rightkit#1437), where
/// two app instances would share one data folder and one single-instance lock, so a run is always
/// serial: one worker. [`size_pool`] is kept for its sizing rule.
pub fn host_pool_size(host: &HostConfig, jobs: usize, serial: Option<&str>) -> PoolSize {
    let _ = (host, jobs, serial);
    PoolSize {
        workers: 1,
        reason: "serial: scenarios run on the owner's real data, one at a time".into(),
    }
}

/// [`size_pool`] over this host: its core count, its commit headroom and `[host]`.
#[allow(dead_code)]
fn sized_by_host(host: &HostConfig, jobs: usize, serial: Option<&str>) -> PoolSize {
    size_pool(&SizeInputs {
        configured: host.workers,
        cores: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1),
        headroom_mb: commit_headroom_mb(),
        peak_mb: host.peak_mb.unwrap_or(DEFAULT_PEAK_MB),
        start_cap: start_workers(),
        jobs,
        serial,
    })
}

/// Memory the host could still commit, in MB: `ullAvailPageFile` on Windows, available memory
/// elsewhere. `None` when the probe fails; the caller then does not let memory limit the pool.
pub fn commit_headroom_mb() -> Option<u64> {
    probe_headroom_mb()
}

#[cfg(windows)]
fn probe_headroom_mb() -> Option<u64> {
    // `MEMORYSTATUSEX` (sysinfoapi.h). Declared here, with the one function that fills it, so this
    // crate needs no extra `windows-sys` feature for a single call.
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
    }
    let mut m = MemoryStatusEx {
        length: std::mem::size_of::<MemoryStatusEx>() as u32,
        memory_load: 0,
        total_phys: 0,
        avail_phys: 0,
        total_page_file: 0,
        avail_page_file: 0,
        total_virtual: 0,
        avail_virtual: 0,
        avail_extended_virtual: 0,
    };
    // SAFETY: `m` is a fully initialised `MEMORYSTATUSEX` with `dwLength` set, as the call requires;
    // Windows writes only inside it.
    let ok = unsafe { GlobalMemoryStatusEx(&mut m) };
    (ok != 0).then_some(m.avail_page_file / (1024 * 1024))
}

#[cfg(target_os = "macos")]
fn probe_headroom_mb() -> Option<u64> {
    let mut cmd = std::process::Command::new("vm_stat");
    let out = crate::process::output_bounded(&mut cmd, std::time::Duration::from_secs(5))
        .ok()
        .flatten()?;
    parse_vm_stat_available_mb(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn probe_headroom_mb() -> Option<u64> {
    parse_meminfo_available_mb(&std::fs::read_to_string("/proc/meminfo").ok()?)
}

/// `MemAvailable` from `/proc/meminfo` text, in MB.
#[allow(dead_code)]
fn parse_meminfo_available_mb(text: &str) -> Option<u64> {
    let line = text.lines().find(|l| l.starts_with("MemAvailable:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024)
}

/// Free, inactive and speculative pages from `vm_stat` text, in MB. Those are the pages the system
/// hands out without evicting anything that is in use.
#[allow(dead_code)]
fn parse_vm_stat_available_mb(text: &str) -> Option<u64> {
    let first = text.lines().next()?;
    let page: u64 = first
        .split("page size of")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let pages = |label: &str| -> Option<u64> {
        let line = text.lines().find(|l| l.starts_with(label))?;
        line.split(':')
            .nth(1)?
            .trim()
            .trim_end_matches('.')
            .parse()
            .ok()
    };
    let total =
        pages("Pages free")? + pages("Pages inactive")? + pages("Pages speculative").unwrap_or(0);
    Some(total * page / (1024 * 1024))
}

// ------------------------------------------------------------------------------------------ tokens

/// Every resource token, in the order they are listed in messages.
pub const ALL_TOKENS: [Token; 5] = [
    Token::Focus,
    Token::Audio,
    Token::GuiSession,
    Token::Clipboard,
    Token::Credentials,
];

/// The name a scenario or journey writes for `token` (the wire name, kebab case).
pub fn token_name(token: Token) -> &'static str {
    match token {
        Token::Focus => "focus",
        Token::Audio => "audio",
        Token::GuiSession => "gui-session",
        Token::Clipboard => "clipboard",
        Token::Credentials => "credentials",
    }
}

/// Parse one token name. Only the exact lowercase names are accepted.
pub fn parse_token(name: &str) -> Result<Token> {
    ALL_TOKENS
        .iter()
        .copied()
        .find(|t| token_name(*t) == name)
        .ok_or_else(|| {
            let allowed = ALL_TOKENS
                .iter()
                .map(|t| token_name(*t))
                .collect::<Vec<_>>()
                .join(", ");
            Error(if name.trim().is_empty() {
                format!("empty resource token name (allowed: {allowed})")
            } else {
                format!("unknown resource token '{name}' (allowed: {allowed})")
            })
        })
}

/// Parse a declared token list: every name known, none repeated. The result keeps declared order.
pub fn parse_tokens(names: &[String]) -> Result<Vec<Token>> {
    let mut out = Vec::with_capacity(names.len());
    for n in names {
        let t = parse_token(n)?;
        if out.contains(&t) {
            return err(format!("resource token '{n}' is declared twice"));
        }
        out.push(t);
    }
    Ok(out)
}

// ------------------------------------------------------------------------------------------ worlds

/// Name of the environment variable that carries the world marker (D-c). Every process of a world
/// has it, so a same-user scan finds survivors by it.
pub const WORLD_MARKER_ENV: &str = "RIGHTKIT_QA_WORLD";
pub const ENV_WORLD_DIR: &str = "RIGHTKIT_QA_WORLD_DIR";
pub const ENV_WORLD_TEMP: &str = "RIGHTKIT_QA_WORLD_TEMP";
pub const ENV_PORT_START: &str = "RIGHTKIT_QA_PORT_START";
pub const ENV_PORT_END: &str = "RIGHTKIT_QA_PORT_END";
pub const ENV_CONTROL_SOCKET: &str = "RIGHTKIT_QA_CONTROL_SOCKET";
pub const ENV_CONTROL_TOKEN_FILE: &str = "RIGHTKIT_QA_CONTROL_TOKEN_FILE";
/// Comma-separated names of the resource tokens the pool holds for the scenario.
pub const ENV_TOKENS: &str = "RIGHTKIT_QA_TOKENS";

/// First port a world range may start at by default; the range sits above the registered services
/// and below the OS dynamic ports.
pub const DEFAULT_PORT_BASE: u16 = 41000;
/// Ports in each world's range by default.
pub const DEFAULT_PORTS_PER_WORLD: u16 = 16;
/// Longest Unix socket path (rightkit-daemon `transport::MAX_UNIX_SOCKET_PATH`): 104 bytes on macOS
/// including the trailing NUL.
#[cfg(unix)]
const MAX_UNIX_SOCKET_PATH: usize = 103;

/// Last port a world may be given: the top of the registered range, below the dynamic ports.
pub const MAX_WORLD_PORT: u16 = 49151;
/// Teardown deadline recorded in a spec until the budgets of W2-G say otherwise.
pub const DEFAULT_TEARDOWN_MS: u64 = 60_000;

/// Where the worlds of one run live and how ports are handed out. Pure: it builds [`World`] values and
/// touches no disk; [`create_world_dirs`] does that.
#[derive(Debug, Clone)]
pub struct WorldPlan {
    run_id: String,
    run_root: PathBuf,
    port_base: u16,
    ports_per_world: u16,
    jobs: usize,
}

impl WorldPlan {
    /// `run_root` is the directory every world lies under (the run's owned home). Refused when
    /// `jobs` ranges of `ports_per_world` ports starting at `port_base` do not fit below
    /// [`MAX_WORLD_PORT`], so no two worlds ever have to share a port.
    pub fn new(run_id: String, run_root: PathBuf, host: &HostConfig, jobs: usize) -> Result<Self> {
        let port_base = host.port_base.unwrap_or(DEFAULT_PORT_BASE);
        let ports_per_world = host.ports_per_world.unwrap_or(DEFAULT_PORTS_PER_WORLD);
        let need = jobs as u64 * u64::from(ports_per_world);
        if need > 0 && u64::from(port_base) + need - 1 > u64::from(MAX_WORLD_PORT) {
            return err(format!(
                "{jobs} scenario worlds of {ports_per_world} ports from {port_base} do not fit below {MAX_WORLD_PORT}; lower [host] ports_per_world or port_base"
            ));
        }
        if run_id.trim().is_empty() {
            return err("a world plan needs a run id");
        }
        Ok(Self {
            run_id,
            run_root,
            port_base,
            ports_per_world,
            jobs,
        })
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn run_root(&self) -> &Path {
        &self.run_root
    }

    /// The directory that holds every world dir.
    pub fn worlds_dir(&self) -> PathBuf {
        self.run_root.join("worlds")
    }

    /// The world of the `index`-th job (0-based). Directory names are short (`w001`) because a
    /// control socket path under them must stay inside the Unix socket path limit.
    pub fn world(&self, index: usize) -> Result<World> {
        if index >= self.jobs {
            return err(format!(
                "world {index} requested from a plan for {} scenarios",
                self.jobs
            ));
        }
        let n = index + 1;
        let dir = self.worlds_dir().join(format!("w{n:03}"));
        // The slot's temp dir is where the control service falls back to a `rk-<tag>-<hash>.sock`
        // (33 bytes, rightkit-daemon transport.rs) when the suite path is too long. It sits at
        // `wNNN/t`, inside the slot, so that socket fits the 103-byte Unix limit; a run root too deep
        // for it is refused here, before anything launches.
        let temp = dir.join("t");
        #[cfg(unix)]
        {
            let need = temp.as_os_str().len() + 1 + 33;
            if need > MAX_UNIX_SOCKET_PATH {
                return err(format!(
                    "the world temp dir {} leaves a control socket path of {need} bytes, over the {MAX_UNIX_SOCKET_PATH}-byte Unix limit; use a shorter run root",
                    temp.display()
                ));
            }
        }
        let start = u32::from(self.port_base) + index as u32 * u32::from(self.ports_per_world);
        Ok(World {
            temp,
            control_socket: dir.join("control.sock"),
            control_token_file: dir.join("control.token"),
            port_start: start as u16,
            port_end: (start + u32::from(self.ports_per_world) - 1) as u16,
            env_marker: EnvMarker {
                name: WORLD_MARKER_ENV.into(),
                value: format!("{}-w{n:03}", self.run_id),
            },
            dir,
        })
    }
}

/// Create the directories of a world (private to the user). Idempotent.
pub fn create_world_dirs(world: &World) -> Result<()> {
    for p in [&world.dir, &world.temp] {
        rightkit_fs::private::ensure_private_dir(p)
            .map_err(|e| Error(format!("cannot create world dir {}: {e}", p.display())))?;
    }
    Ok(())
}

fn path_key(p: &Path) -> String {
    let s = plain_path(p).to_string_lossy().into_owned();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

/// Refuse a set of worlds that share anything: a path, a marker or a port, or that nest one world's
/// directory inside another's. The pool runs this before it starts any scenario.
pub fn check_distinct(worlds: &[World]) -> Result<()> {
    let mut paths: HashSet<String> = HashSet::new();
    let mut markers: HashSet<&str> = HashSet::new();
    for w in worlds {
        for (label, p) in [
            ("dir", &w.dir),
            ("temp", &w.temp),
            ("control socket", &w.control_socket),
            ("control token file", &w.control_token_file),
        ] {
            if !paths.insert(path_key(p)) {
                return err(format!(
                    "two scenario worlds share a {label}: {}",
                    p.display()
                ));
            }
        }
        if !markers.insert(w.env_marker.value.as_str()) {
            return err(format!(
                "two scenario worlds share the env marker {}",
                w.env_marker.value
            ));
        }
    }
    let dirs: Vec<PathBuf> = worlds
        .iter()
        .map(|w| PathBuf::from(path_key(&w.dir)))
        .collect();
    for (i, a) in dirs.iter().enumerate() {
        for b in &dirs[i + 1..] {
            if a.starts_with(b) || b.starts_with(a) {
                return err(format!(
                    "scenario world {} is nested inside another world",
                    a.display()
                ));
            }
        }
    }
    let mut ranges: Vec<(u16, u16)> = worlds.iter().map(|w| (w.port_start, w.port_end)).collect();
    ranges.sort_unstable();
    for pair in ranges.windows(2) {
        if pair[0].1 >= pair[1].0 {
            return err(format!(
                "two scenario worlds have overlapping port ranges {}-{} and {}-{}",
                pair[0].0, pair[0].1, pair[1].0, pair[1].1
            ));
        }
    }
    Ok(())
}

/// A world as environment variables, to be added to every process the scenario starts. The marker
/// comes first under its own name; the rest describe the world and the tokens held for it.
pub fn world_env(world: &World, tokens: &[Token]) -> Vec<(String, String)> {
    let text = |p: &Path| p.to_string_lossy().into_owned();
    vec![
        (
            world.env_marker.name.clone(),
            world.env_marker.value.clone(),
        ),
        (ENV_WORLD_DIR.into(), text(&world.dir)),
        (ENV_WORLD_TEMP.into(), text(&world.temp)),
        (ENV_PORT_START.into(), world.port_start.to_string()),
        (ENV_PORT_END.into(), world.port_end.to_string()),
        (ENV_CONTROL_SOCKET.into(), text(&world.control_socket)),
        (
            ENV_CONTROL_TOKEN_FILE.into(),
            text(&world.control_token_file),
        ),
        (
            ENV_TOKENS.into(),
            tokens
                .iter()
                .map(|t| token_name(*t))
                .collect::<Vec<_>>()
                .join(","),
        ),
    ]
}

/// Read a world back from the environment a process was started with (the inverse of
/// [`world_env`]); `get` is the lookup, normally `|k| std::env::var(k).ok()`. Every variable must be
/// present. Paths are not re-validated here; `ScenarioSpec::validate` does that against the run root.
pub fn world_from_env(get: &dyn Fn(&str) -> Option<String>) -> Result<World> {
    let need = |k: &str| -> Result<String> {
        get(k).filter(|v| !v.is_empty()).ok_or_else(|| {
            Error(format!(
                "the scenario world is not in this process's env: {k} is not set"
            ))
        })
    };
    let port = |k: &str| -> Result<u16> {
        need(k)?
            .parse()
            .map_err(|_| Error(format!("{k} is not a port number")))
    };
    Ok(World {
        dir: PathBuf::from(need(ENV_WORLD_DIR)?),
        temp: PathBuf::from(need(ENV_WORLD_TEMP)?),
        port_start: port(ENV_PORT_START)?,
        port_end: port(ENV_PORT_END)?,
        control_socket: PathBuf::from(need(ENV_CONTROL_SOCKET)?),
        control_token_file: PathBuf::from(need(ENV_CONTROL_TOKEN_FILE)?),
        env_marker: EnvMarker {
            name: WORLD_MARKER_ENV.into(),
            value: need(WORLD_MARKER_ENV)?,
        },
    })
}

/// What [`build_spec`] needs beyond the plan and the world.
#[derive(Debug, Clone)]
pub struct SpecInputs<'a> {
    pub app: &'a str,
    pub platform: &'a str,
    pub scenario_id: &'a str,
    /// The resolved scenario as the worker will decode it (opaque to the contract).
    pub scenario: Value,
    pub tokens: &'a [Token],
    /// The scenario's wall-time budget.
    pub scenario_ms: u64,
}

/// The `scenario.json` content for one job. The step deadline is the scenario deadline until the
/// per-step budgets of W2-G land; the runner enforces them (W2-D), this only records them.
pub fn build_spec(plan: &WorldPlan, world: &World, i: SpecInputs<'_>) -> ScenarioSpec {
    ScenarioSpec {
        schema_version: SCHEMA_VERSION,
        run_id: plan.run_id.clone(),
        scenario_id: i.scenario_id.to_string(),
        app: i.app.to_string(),
        platform: i.platform.to_string(),
        run_root: plan.run_root.clone(),
        world: world.clone(),
        scenario: i.scenario,
        grants: i.tokens.to_vec(),
        deadlines: Deadlines {
            step_ms: i.scenario_ms,
            scenario_ms: i.scenario_ms,
            teardown_ms: DEFAULT_TEARDOWN_MS,
        },
    }
}

// ---------------------------------------------------------------------- per-scenario workspace

/// The [`QaWorkspace`] one in-process scenario runs in: the run's workspace with the scenario slot's
/// own variables (marker, ports, control endpoint, tokens) added and its control token minted. The
/// scratch home, evidence and capture dirs stay shared, and the app's home, profile and data folders
/// are never moved: the scenario runs on the owner's real data.
pub fn world_workspace(ws: &QaWorkspace, world: &World, tokens: &[Token]) -> Result<QaWorkspace> {
    create_world_dirs(world)?;
    // The slot names its control token file to every launch (`ENV_CONTROL_TOKEN_FILE` is the launch's
    // `token::ENV_AUTH_FILE`), and a launch reads the file it is named, failing closed: the token is
    // minted here, where every runner path (suite and journeys) builds its slot.
    crate::control::token::write_file(&world.control_token_file)?;
    let mut env: BTreeMap<String, String> = ws.env.clone();
    env.extend(world_env(world, tokens));
    Ok(QaWorkspace {
        run_id: ws.run_id.clone(),
        root: ws.root.clone(),
        home: ws.home.clone(),
        evidence_dir: ws.evidence_dir.clone(),
        capture_dir: ws.capture_dir.clone(),
        env,
    })
}

/// Where a world's process tracker records its pids so a crashed run's survivors can be swept:
/// `<run dir>/pids/<world dir name>.json`.
pub fn registry_path(run_dir: &Path, world: &World) -> PathBuf {
    let name = world
        .dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "world".into());
    run_dir.join("pids").join(format!("{name}.json"))
}

/// Kill processes that earlier, finished-or-crashed runs recorded in their per-world registries and
/// that are still alive with the exact command line recorded, like [`crate::process::sweep_registries`]
/// does for the run-level registry. Returns the pids killed.
pub fn sweep_world_registries(runs_root: &Path, skip_run: &str) -> Vec<u32> {
    let mut killed = vec![];
    let Ok(entries) = std::fs::read_dir(runs_root) else {
        return killed;
    };
    for e in entries.flatten() {
        if e.file_name().to_string_lossy() == skip_run {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        if !ft.is_dir() || ft.is_symlink() {
            continue;
        }
        let Some(marker) = crate::storage::read_marker(&e.path().join(crate::storage::RUN_MARKER))
        else {
            continue;
        };
        if marker.run_id != e.file_name().to_string_lossy()
            || crate::storage::marker_active(&marker, crate::util::now_ms())
        {
            continue;
        }
        let Ok(files) = std::fs::read_dir(e.path().join("pids")) else {
            continue;
        };
        for f in files.flatten() {
            let path = f.path();
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let rows: Vec<Value> = serde_json::from_slice(&bytes).unwrap_or_default();
            for r in rows {
                let (Some(pid), Some(cmd)) =
                    (r["pid"].as_u64().map(|p| p as u32), r["command"].as_str())
                else {
                    continue;
                };
                if crate::process::is_alive(pid)
                    && crate::process::fingerprint(pid).as_deref() == Some(cmd)
                {
                    crate::process::kill_tree(pid);
                    killed.push(pid);
                }
            }
            let _ = std::fs::remove_file(&path);
        }
    }
    killed
}

// ---------------------------------------------------------------------------------------- the pool

/// One unit of work for the pool: a name and the tokens it must hold while it runs.
#[derive(Debug, Clone)]
pub struct Job {
    pub id: String,
    pub tokens: Vec<Token>,
}

/// Why a job has no result of its own: its world could not be built, or it panicked. The text says
/// which. The caller turns it into an `errored` outcome; it is never a pass.
pub type JobFailure = String;

fn recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no message".into())
}

struct Queue {
    state: Mutex<QueueState>,
    cv: Condvar,
}

struct QueueState {
    started: Vec<bool>,
    unstarted: usize,
    held: BTreeSet<Token>,
}

/// A job a worker has taken, with the tokens it holds. Dropping it (normal end or panic) releases the
/// tokens and wakes the workers waiting for them.
struct Claim<'q> {
    queue: &'q Queue,
    index: usize,
    tokens: Vec<Token>,
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        let mut st = recover(&self.queue.state);
        for t in &self.tokens {
            st.held.remove(t);
        }
        drop(st);
        self.queue.cv.notify_all();
    }
}

impl Queue {
    fn new(jobs: usize) -> Self {
        Self {
            state: Mutex::new(QueueState {
                started: vec![false; jobs],
                unstarted: jobs,
                held: BTreeSet::new(),
            }),
            cv: Condvar::new(),
        }
    }

    /// Take the first job not yet started whose tokens are all free, and hold them. Waits while every
    /// unstarted job is blocked on a token; returns `None` once every job has been taken. Looking past
    /// a blocked job keeps a worker busy instead of idle behind it.
    fn claim<'q>(&'q self, jobs: &[Job]) -> Option<Claim<'q>> {
        let mut st = recover(&self.state);
        loop {
            if st.unstarted == 0 {
                return None;
            }
            let ready = (0..jobs.len())
                .find(|&i| !st.started[i] && jobs[i].tokens.iter().all(|t| !st.held.contains(t)));
            if let Some(i) = ready {
                st.started[i] = true;
                st.unstarted -= 1;
                st.held.extend(jobs[i].tokens.iter().copied());
                return Some(Claim {
                    queue: self,
                    index: i,
                    tokens: jobs[i].tokens.clone(),
                });
            }
            st = self.cv.wait(st).unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// A fixed number of workers over a queue of jobs.
#[derive(Debug, Clone, Copy)]
pub struct Pool {
    workers: usize,
}

impl Pool {
    pub fn new(workers: usize) -> Self {
        Self {
            workers: workers.max(1),
        }
    }

    pub fn workers(&self) -> usize {
        self.workers
    }

    /// Run every job and return one result per job, in job order. A worker takes a job only when it
    /// can hold all of the job's tokens, builds the job's world directories, then calls
    /// `run_one(index, job, world)`. A panic in `run_one`, or a world that cannot be built, is that
    /// job's `Err` and nothing else: the tokens are released and the other jobs go on.
    ///
    /// Fails before anything runs when the worlds would share a path, marker or port.
    pub fn run<R: Send>(
        &self,
        jobs: &[Job],
        plan: &WorldPlan,
        run_one: impl Fn(usize, &Job, &World) -> R + Sync,
    ) -> Result<Vec<std::result::Result<R, JobFailure>>> {
        let worlds: Vec<World> = (0..jobs.len())
            .map(|i| plan.world(i))
            .collect::<Result<_>>()?;
        check_distinct(&worlds)?;
        if jobs.is_empty() {
            return Ok(vec![]);
        }
        rightkit_fs::private::ensure_private_dir(&plan.worlds_dir()).map_err(|e| {
            Error(format!(
                "cannot create {}: {e}",
                plan.worlds_dir().display()
            ))
        })?;
        let queue = Queue::new(jobs.len());
        let results: Mutex<Vec<Option<std::result::Result<R, JobFailure>>>> =
            Mutex::new((0..jobs.len()).map(|_| None).collect());
        std::thread::scope(|s| {
            for _ in 0..self.workers.min(jobs.len()) {
                s.spawn(|| {
                    while let Some(claim) = queue.claim(jobs) {
                        let i = claim.index;
                        let outcome = match create_world_dirs(&worlds[i]) {
                            Err(e) => Err(e.0),
                            Ok(()) => {
                                catch_unwind(AssertUnwindSafe(|| run_one(i, &jobs[i], &worlds[i])))
                                    .map_err(|p| format!("scenario panicked: {}", panic_text(&*p)))
                            }
                        };
                        recover(&results)[i] = Some(outcome);
                        // `claim` drops here: the tokens are released whether or not it panicked.
                        drop(claim);
                    }
                });
            }
        });
        Ok(results
            .into_inner()
            .unwrap_or_else(|e| e.into_inner())
            .into_iter()
            .map(|r| r.unwrap_or_else(|| Err("job never ran".into())))
            .collect())
    }
}
