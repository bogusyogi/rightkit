//! The machine-wide scenario pool (ADR-0001 item 2, Sage C2, C5 and C10): a work queue, per-scenario
//! worlds and resource tokens.
//!
//! * **Size.** [`size_pool`] picks the worker count: `[host] workers` when set, else
//!   `min(cores, commit headroom / configured peak per scenario)` capped by a per-OS start value
//!   ([`start_workers`]: Dell 2, Mac 4) until the real peaks are measured. Commit headroom is
//!   `ullAvailPageFile` from `GlobalMemoryStatusEx` on Windows and available memory elsewhere.
//! * **Worlds.** Every scenario gets a [`World`] ([`WorldPlan::world`]): a data dir, home and temp
//!   dir, a port range, a control socket and control token file path, and an env marker, all under
//!   the run root and none shared with another scenario. [`world_env`] exports a world as
//!   environment variables and [`world_from_env`] reads it back, so a child finds its world in its own
//!   env. [`world_workspace`] turns a world into the [`QaWorkspace`] one in-process scenario runs in.
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

/// [`size_pool`] over this host: its core count, its commit headroom and `[host]`.
pub fn host_pool_size(host: &HostConfig, jobs: usize, serial: Option<&str>) -> PoolSize {
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
pub const ENV_WORLD_DATA: &str = "RIGHTKIT_QA_WORLD_DATA";
pub const ENV_WORLD_HOME: &str = "RIGHTKIT_QA_WORLD_HOME";
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
        let home = dir.join("home");
        let start = u32::from(self.port_base) + index as u32 * u32::from(self.ports_per_world);
        Ok(World {
            data_dir: home.join("data"),
            temp: home.join("tmp"),
            control_socket: dir.join("control.sock"),
            control_token_file: dir.join("control.token"),
            port_start: start as u16,
            port_end: (start + u32::from(self.ports_per_world) - 1) as u16,
            env_marker: EnvMarker {
                name: WORLD_MARKER_ENV.into(),
                value: format!("{}-w{n:03}", self.run_id),
            },
            home,
            dir,
        })
    }
}

/// Create the directories of a world (private to the user). Idempotent. Everything else a scenario
/// needs under the home is the runner's to add, see [`world_workspace`].
pub fn create_world_dirs(world: &World) -> Result<()> {
    for p in [&world.dir, &world.home, &world.data_dir, &world.temp] {
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
            ("data dir", &w.data_dir),
            ("home", &w.home),
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
        (ENV_WORLD_DATA.into(), text(&world.data_dir)),
        (ENV_WORLD_HOME.into(), text(&world.home)),
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
        data_dir: PathBuf::from(need(ENV_WORLD_DATA)?),
        home: PathBuf::from(need(ENV_WORLD_HOME)?),
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

/// The runner's binding file name in a QA home. `storage` keeps its own copy private.
const BINDING_FILE: &str = ".rightkit-app.json";

/// The shape of a fresh QA home (its profile directories and its runner binding), captured before
/// any scenario runs so each world home can be built the same way without reading a home that other
/// scenarios are writing into.
#[derive(Debug, Clone)]
pub struct HomeTemplate {
    dirs: Vec<PathBuf>,
    binding: Vec<u8>,
}

impl HomeTemplate {
    /// Capture `home`: every directory under it (not the `worlds` tree) and its binding file.
    pub fn capture(home: &Path) -> Result<Self> {
        let binding = std::fs::read(home.join(BINDING_FILE)).map_err(|e| {
            Error(format!(
                "{}: no readable runner binding to copy into scenario worlds: {e}",
                home.display()
            ))
        })?;
        let mut dirs = vec![];
        let mut stack = vec![PathBuf::new()];
        while let Some(rel) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(home.join(&rel)) else {
                continue;
            };
            for e in rd.flatten() {
                let Ok(ft) = e.file_type() else { continue };
                if !ft.is_dir() || ft.is_symlink() {
                    continue;
                }
                let child = rel.join(e.file_name());
                if child == Path::new("worlds") {
                    continue;
                }
                dirs.push(child.clone());
                stack.push(child);
            }
        }
        dirs.sort();
        Ok(Self { dirs, binding })
    }

    fn apply(&self, world_home: &Path) -> Result<()> {
        for rel in &self.dirs {
            std::fs::create_dir_all(world_home.join(rel))?;
        }
        rightkit_fs::atomic_write(&world_home.join(BINDING_FILE), &self.binding).map_err(|e| {
            Error(format!(
                "cannot bind scenario home {}: {}",
                world_home.display(),
                std::io::Error::from(e)
            ))
        })
    }
}

/// The [`QaWorkspace`] one in-process scenario runs in: the run's workspace with its home, data dir
/// and every environment path moved into `world`, the world's own variables added, and the evidence
/// and capture dirs left shared. Two scenarios built this way share no profile directory, so nothing
/// one of them writes, or tears down, is in the other's.
pub fn world_workspace(
    ws: &QaWorkspace,
    template: &HomeTemplate,
    world: &World,
    tokens: &[Token],
) -> Result<QaWorkspace> {
    create_world_dirs(world)?;
    template.apply(&world.home)?;
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in &ws.env {
        let moved = match Path::new(v).strip_prefix(&ws.home) {
            Ok(rel) if rel.as_os_str().is_empty() => world.home.to_string_lossy().into_owned(),
            Ok(rel) => world.home.join(rel).to_string_lossy().into_owned(),
            Err(_) => v.clone(),
        };
        env.insert(k.clone(), moved);
    }
    // Every directory an exported variable points at must exist (the run workspace does the same).
    for v in env.values() {
        let p = Path::new(v);
        if p.starts_with(&world.home) {
            std::fs::create_dir_all(p)?;
        }
    }
    env.extend(world_env(world, tokens));
    Ok(QaWorkspace {
        run_id: ws.run_id.clone(),
        root: ws.root.clone(),
        home: world.home.clone(),
        data_dir: world.data_dir.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::SCENARIO_FILE;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    fn root(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rkqa-pool-{tag}-{}", crate::util::new_id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn plan(root: &Path, jobs: usize) -> WorldPlan {
        WorldPlan::new(
            "run-1".into(),
            root.to_path_buf(),
            &HostConfig::default(),
            jobs,
        )
        .unwrap()
    }

    fn jobs(spec: &[(&str, &[Token])]) -> Vec<Job> {
        spec.iter()
            .map(|(id, t)| Job {
                id: (*id).into(),
                tokens: t.to_vec(),
            })
            .collect()
    }

    // ---------------------------------------------------------------- tokens and sizing

    #[test]
    fn token_names_round_trip_and_unknown_repeated_or_empty_names_are_refused() {
        for t in ALL_TOKENS {
            assert_eq!(parse_token(token_name(t)).unwrap(), t);
            // The pool's names are the contract's wire names.
            assert_eq!(
                serde_json::to_value(t).unwrap(),
                Value::String(token_name(t).into())
            );
        }
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            parse_tokens(&names(&["clipboard", "focus"])).unwrap(),
            vec![Token::Clipboard, Token::Focus]
        );
        assert!(parse_tokens(&[]).unwrap().is_empty());
        for bad in [
            &["fcus"][..],
            &[""],
            &["Focus"],
            &["focus", "audio", "focus"],
        ] {
            let e = parse_tokens(&names(bad)).unwrap_err();
            assert!(e.0.contains("token"), "{bad:?}: {e}");
        }
    }

    fn inputs() -> SizeInputs<'static> {
        SizeInputs {
            configured: None,
            cores: 16,
            headroom_mb: Some(64 * 1024),
            peak_mb: 1024,
            start_cap: 4,
            jobs: 200,
            serial: None,
        }
    }

    #[test]
    fn pool_size_is_min_of_cores_headroom_and_the_start_value() {
        // Start value is the ceiling.
        assert_eq!(size_pool(&inputs()).workers, 4);
        // Cores limit.
        let i = SizeInputs {
            cores: 2,
            ..inputs()
        };
        assert_eq!(size_pool(&i).workers, 2);
        // Commit headroom / peak limits: 3.5 GB / 1 GB = 3.
        let i = SizeInputs {
            headroom_mb: Some(3500),
            ..inputs()
        };
        assert_eq!(size_pool(&i).workers, 3);
        // Not even one scenario's headroom still runs one worker.
        let i = SizeInputs {
            headroom_mb: Some(10),
            ..inputs()
        };
        assert_eq!(size_pool(&i).workers, 1);
        // An unreadable probe does not limit.
        let i = SizeInputs {
            headroom_mb: None,
            ..inputs()
        };
        assert_eq!(size_pool(&i).workers, 4);
        // Never more workers than jobs.
        let i = SizeInputs {
            jobs: 2,
            ..inputs()
        };
        assert_eq!(size_pool(&i).workers, 2);
        let i = SizeInputs {
            jobs: 0,
            ..inputs()
        };
        assert_eq!(size_pool(&i).workers, 1);
    }

    #[test]
    fn host_workers_overrides_the_computed_size_and_a_serial_reason_only_the_default() {
        let i = SizeInputs {
            configured: Some(8),
            headroom_mb: Some(10),
            ..inputs()
        };
        let s = size_pool(&i);
        assert_eq!(s.workers, 8);
        assert!(s.reason.contains("[host] workers"), "{}", s.reason);
        // Clamped to the job count and to at least one.
        let i = SizeInputs {
            configured: Some(8),
            jobs: 3,
            ..inputs()
        };
        assert_eq!(size_pool(&i).workers, 3);
        // A serial reason forces one worker...
        let i = SizeInputs {
            serial: Some("shared bundle path"),
            ..inputs()
        };
        let s = size_pool(&i);
        assert_eq!(s.workers, 1);
        assert!(s.reason.contains("shared bundle path"));
        // ...unless the config names a size.
        let i = SizeInputs {
            serial: Some("shared bundle path"),
            configured: Some(2),
            ..inputs()
        };
        assert_eq!(size_pool(&i).workers, 2);
    }

    #[test]
    fn the_per_os_start_values_are_the_adr_values() {
        assert_eq!(START_WORKERS_WINDOWS, 2);
        assert_eq!(START_WORKERS_MACOS, 4);
        assert!(start_workers() >= 1);
    }

    #[test]
    fn memory_probes_parse_their_text_and_the_host_probe_answers_or_declines() {
        assert_eq!(
            parse_meminfo_available_mb("MemTotal: 16000000 kB\nMemAvailable:    2097152 kB\n"),
            Some(2048)
        );
        assert_eq!(parse_meminfo_available_mb("MemTotal: 1 kB\n"), None);
        let vm = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free:                               1000.\nPages active:                             5000.\nPages inactive:                           3000.\nPages speculative:                         120.\n";
        assert_eq!(
            parse_vm_stat_available_mb(vm),
            Some(4120 * 16384 / (1024 * 1024))
        );
        assert_eq!(parse_vm_stat_available_mb("nonsense"), None);
        if let Some(mb) = commit_headroom_mb() {
            assert!(mb > 0);
        }
    }

    // ---------------------------------------------------------------- worlds

    #[test]
    fn eight_worlds_share_no_path_port_or_marker_and_validate_against_the_run_root() {
        let r = root("worlds");
        let p = plan(&r, 8);
        let worlds: Vec<World> = (0..8).map(|i| p.world(i).unwrap()).collect();
        check_distinct(&worlds).unwrap();
        for w in &worlds {
            w.validate(&r).unwrap();
            assert!(w.home.starts_with(&w.dir));
            assert_eq!(w.data_dir, w.home.join("data"));
            assert_eq!(w.temp, w.home.join("tmp"));
            assert_eq!(w.env_marker.name, WORLD_MARKER_ENV);
            assert!(w.port_start >= DEFAULT_PORT_BASE && w.port_end <= MAX_WORLD_PORT);
            assert_eq!(w.port_end - w.port_start + 1, DEFAULT_PORTS_PER_WORLD);
        }
        // Past the plan is refused, not wrapped around onto another world's ports.
        assert!(p.world(8).is_err());
    }

    #[test]
    fn a_plan_that_cannot_give_every_world_its_own_ports_is_refused() {
        let r = root("ports");
        let host = HostConfig {
            port_base: Some(49000),
            ports_per_world: Some(16),
            ..Default::default()
        };
        // 9 x 16 = 144 ports from 49000 ends at 49143: fits; 10 x 16 ends at 49159: does not.
        assert!(WorldPlan::new("r".into(), r.clone(), &host, 9).is_ok());
        let e = WorldPlan::new("r".into(), r, &host, 10).unwrap_err();
        assert!(e.0.contains("do not fit"), "{e}");
    }

    #[test]
    fn check_distinct_refuses_shared_paths_markers_ports_and_nesting() {
        let r = root("distinct");
        let p = plan(&r, 3);
        let a = p.world(0).unwrap();
        let b = p.world(1).unwrap();

        let mut b2 = b.clone();
        b2.data_dir = a.data_dir.clone();
        assert!(check_distinct(&[a.clone(), b2])
            .unwrap_err()
            .0
            .contains("data dir"));

        let mut b2 = b.clone();
        b2.env_marker.value = a.env_marker.value.clone();
        assert!(check_distinct(&[a.clone(), b2])
            .unwrap_err()
            .0
            .contains("marker"));

        let mut b2 = b.clone();
        b2.port_start = a.port_end;
        assert!(check_distinct(&[a.clone(), b2])
            .unwrap_err()
            .0
            .contains("overlapping"));

        let mut b2 = b.clone();
        b2.dir = a.dir.join("inner");
        assert!(check_distinct(&[a.clone(), b2])
            .unwrap_err()
            .0
            .contains("nested"));
    }

    #[test]
    fn a_world_is_read_back_from_the_environment_it_was_exported_to() {
        let r = root("env");
        let w = plan(&r, 2).world(1).unwrap();
        let pairs: BTreeMap<String, String> = world_env(&w, &[Token::Focus, Token::Audio])
            .into_iter()
            .collect();
        assert_eq!(pairs[WORLD_MARKER_ENV], w.env_marker.value);
        assert_eq!(pairs[ENV_TOKENS], "focus,audio");
        let back = world_from_env(&|k| pairs.get(k).cloned()).unwrap();
        assert_eq!(back, w);
        // A variable missing from the env is named.
        let e = world_from_env(&|k| (k != ENV_PORT_END).then(|| pairs[k].clone())).unwrap_err();
        assert!(e.0.contains(ENV_PORT_END), "{e}");
        let e = world_from_env(&|k| {
            (k == ENV_PORT_START)
                .then(|| "x".to_string())
                .or_else(|| pairs.get(k).cloned())
        })
        .unwrap_err();
        assert!(e.0.contains("port"), "{e}");
    }

    #[test]
    fn a_spec_built_for_a_world_passes_the_worker_contract_and_refuses_a_foreign_world() {
        let r = root("spec");
        let p = plan(&r, 2);
        let w = p.world(0).unwrap();
        create_world_dirs(&w).unwrap();
        let spec = build_spec(
            &p,
            &w,
            SpecInputs {
                app: "demo",
                platform: "windows",
                scenario_id: "login",
                scenario: serde_json::json!({"name": "login"}),
                tokens: &[Token::Focus],
                scenario_ms: 600_000,
            },
        );
        spec.validate().unwrap();
        assert_eq!(spec.grants, vec![Token::Focus]);
        crate::worker::write_scenario(&spec).unwrap();
        assert!(w.dir.join(SCENARIO_FILE).is_file());
        assert_eq!(crate::worker::read_scenario(&w.dir).unwrap(), spec);
        // A world outside the run root is refused by the contract the spec is checked against.
        let mut foreign = spec.clone();
        foreign.world.temp = root("foreign").join("tmp");
        assert!(foreign.validate().is_err());
    }

    #[test]
    fn world_workspace_moves_every_profile_path_into_the_world_and_shares_none() {
        let r = root("wsworld");
        let ws = crate::workspace::create(&r.join("managed"), None, "pooltest").unwrap();
        let template = HomeTemplate::capture(&ws.home).unwrap();
        let p = WorldPlan::new(
            ws.run_id.clone(),
            ws.home.clone(),
            &HostConfig::default(),
            2,
        )
        .unwrap();
        let (wa, wb) = (p.world(0).unwrap(), p.world(1).unwrap());
        let a = world_workspace(&ws, &template, &wa, &[Token::Focus]).unwrap();
        let b = world_workspace(&ws, &template, &wb, &[]).unwrap();

        for (w, world) in [(&a, &wa), (&b, &wb)] {
            assert_eq!(w.home, world.home);
            assert_eq!(w.data_dir, world.data_dir);
            for (k, v) in &w.env {
                // Nothing the app is pointed at is still in the run's own home. The world is its
                // whole directory (`worlds/wNNN`): its home, data and temp dirs, and the control
                // socket and token file beside them; never another world's directory.
                assert!(
                    !Path::new(v).starts_with(&ws.home) || Path::new(v).starts_with(&world.dir),
                    "{k}={v} was left outside the world"
                );
            }
            assert_eq!(w.env["HOME"], world.home.to_string_lossy());
            assert_eq!(w.env["TMP"], world.temp.to_string_lossy());
            assert_eq!(w.env[WORLD_MARKER_ENV], world.env_marker.value);
            assert!(world.home.join(BINDING_FILE).is_file());
            // A scenario dir is created under this world's data dir and bound to the run.
            let d = w.scenario_dir("s").unwrap();
            assert!(d.starts_with(&world.data_dir));
        }
        assert_ne!(a.env[WORLD_MARKER_ENV], b.env[WORLD_MARKER_ENV]);
        assert_eq!(a.env[ENV_TOKENS], "focus");
        assert_eq!(b.env[ENV_TOKENS], "");

        // Tearing one world down leaves the other whole.
        let kept = b.scenario_dir("keep").unwrap();
        std::fs::write(kept.join("state"), b"x").unwrap();
        std::fs::remove_dir_all(&wa.dir).unwrap();
        assert!(kept.join("state").is_file());
        let _ = crate::workspace::dispose(&ws, false, 3, 10);
        let _ = std::fs::remove_dir_all(&r);
    }

    // ---------------------------------------------------------------- the pool

    #[test]
    fn eight_no_op_scenarios_complete_in_parallel_with_distinct_worlds() {
        let r = root("eight");
        let p = plan(&r, 8);
        let js: Vec<Job> = (0..8)
            .map(|i| Job {
                id: format!("s{i}"),
                tokens: vec![],
            })
            .collect();
        let live = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let seen: Mutex<Vec<World>> = Mutex::new(vec![]);
        let results = Pool::new(4)
            .run(&js, &p, |i, job, world| {
                let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                // The world exists on disk, and is this job's.
                assert!(world.home.is_dir() && world.temp.is_dir() && world.data_dir.is_dir());
                assert_eq!(world.env_marker.value, format!("run-1-w{:03}", i + 1));
                std::fs::write(world.temp.join("mine"), job.id.as_bytes()).unwrap();
                std::thread::sleep(Duration::from_millis(60));
                assert_eq!(
                    std::fs::read(world.temp.join("mine")).unwrap(),
                    job.id.as_bytes()
                );
                recover(&seen).push(world.clone());
                live.fetch_sub(1, Ordering::SeqCst);
                i
            })
            .unwrap();
        assert_eq!(results.len(), 8);
        for (i, res) in results.iter().enumerate() {
            assert_eq!(res.as_ref().unwrap(), &i, "results come back in job order");
        }
        let worlds = recover(&seen).clone();
        assert_eq!(worlds.len(), 8);
        check_distinct(&worlds).unwrap();
        assert!(
            peak.load(Ordering::SeqCst) >= 2,
            "no two scenarios ever ran together"
        );
        assert!(
            peak.load(Ordering::SeqCst) <= 4,
            "more than the pool size ran together"
        );
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn scenarios_sharing_a_token_never_overlap_and_ungrouped_ones_never_wait() {
        let r = root("tokens");
        let js = jobs(&[
            ("focus-a", &[Token::Focus]),
            ("plain-1", &[]),
            ("focus-b", &[Token::Focus]),
            ("audio-focus", &[Token::Audio, Token::Focus]),
            ("plain-2", &[]),
            ("audio", &[Token::Audio]),
            ("focus-c", &[Token::Focus]),
            ("plain-3", &[]),
        ]);
        let p = plan(&r, js.len());
        let base = Instant::now();
        let spans: Mutex<Vec<(usize, Duration, Duration)>> = Mutex::new(vec![]);
        let results = Pool::new(4)
            .run(&js, &p, |i, _job, _world| {
                let start = base.elapsed();
                std::thread::sleep(Duration::from_millis(80));
                recover(&spans).push((i, start, base.elapsed()));
            })
            .unwrap();
        assert!(results.iter().all(|r| r.is_ok()));
        let spans = recover(&spans).clone();
        assert_eq!(spans.len(), js.len());
        let span = |i: usize| spans.iter().find(|s| s.0 == i).copied().unwrap();
        let mut shared_pairs = 0;
        for a in 0..js.len() {
            for b in (a + 1)..js.len() {
                let shares = js[a].tokens.iter().any(|t| js[b].tokens.contains(t));
                if !shares {
                    continue;
                }
                shared_pairs += 1;
                let (_, s1, e1) = span(a);
                let (_, s2, e2) = span(b);
                assert!(
                    e1 <= s2 || e2 <= s1,
                    "{} and {} share a token but overlapped",
                    js[a].id,
                    js[b].id
                );
            }
        }
        assert!(shared_pairs >= 6, "the fixture lost its contended pairs");
        // The three ungrouped scenarios ran together with something else: they took no lock.
        let plain: Vec<usize> = (0..js.len()).filter(|i| js[*i].tokens.is_empty()).collect();
        let overlaps_other = plain.iter().any(|&i| {
            let (_, s, e) = span(i);
            (0..js.len()).any(|j| {
                j != i && {
                    let (_, s2, e2) = span(j);
                    s < e2 && s2 < e
                }
            })
        });
        assert!(
            overlaps_other,
            "ungrouped scenarios did not run alongside others"
        );
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn a_panic_releases_its_tokens_and_fails_only_its_own_scenario() {
        let r = root("panic");
        let js = jobs(&[
            ("boom", &[Token::Focus, Token::Audio]),
            ("after", &[Token::Focus]),
            ("other", &[Token::Audio]),
            ("plain", &[]),
        ]);
        let p = plan(&r, js.len());
        // Two workers: "after" and "other" wait on tokens "boom" holds. If the panic leaked them
        // this call would never return.
        let results = Pool::new(2)
            .run(&js, &p, |i, job, _| {
                if job.id == "boom" {
                    panic!("boom at {i}");
                }
                i
            })
            .unwrap();
        let e = results[0].as_ref().unwrap_err();
        assert!(e.contains("boom at 0"), "{e}");
        assert_eq!(results[1].as_ref().unwrap(), &1);
        assert_eq!(results[2].as_ref().unwrap(), &2);
        assert_eq!(results[3].as_ref().unwrap(), &3);
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn tearing_down_one_world_while_another_runs_never_touches_it() {
        let r = root("teardown");
        let js = jobs(&[("quick", &[]), ("slow", &[])]);
        let p = plan(&r, 2);
        let slow_written = std::sync::Barrier::new(2);
        let quick_gone = std::sync::Barrier::new(2);
        let results = Pool::new(2)
            .run(&js, &p, |_, job, world| {
                if job.id == "slow" {
                    std::fs::write(world.data_dir.join("state"), b"keep").unwrap();
                    slow_written.wait();
                    quick_gone.wait();
                    // The other world was removed entirely while this one was live.
                    std::fs::read(world.data_dir.join("state")).unwrap() == b"keep"
                        && world.home.is_dir()
                } else {
                    slow_written.wait();
                    std::fs::remove_dir_all(&world.dir).unwrap();
                    quick_gone.wait();
                    !world.dir.exists()
                }
            })
            .unwrap();
        assert!(results.iter().all(|r| *r.as_ref().unwrap()));
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn a_plan_with_colliding_worlds_runs_nothing() {
        // `Pool::run` checks the worlds it builds; this is the check, on a hand-made collision.
        let r = root("collide");
        let p = plan(&r, 2);
        let a = p.world(0).unwrap();
        let mut b = p.world(1).unwrap();
        b.control_token_file = a.control_token_file.clone();
        assert!(check_distinct(&[a, b])
            .unwrap_err()
            .0
            .contains("control token file"));
    }

    #[test]
    fn registry_paths_are_per_world() {
        let r = root("registry");
        let p = plan(&r, 2);
        let (a, b) = (p.world(0).unwrap(), p.world(1).unwrap());
        let run_dir = r.join("run");
        assert_ne!(registry_path(&run_dir, &a), registry_path(&run_dir, &b));
        assert!(registry_path(&run_dir, &a).starts_with(run_dir.join("pids")));
        // Nothing to sweep under a missing or empty root.
        assert!(sweep_world_registries(&r.join("none"), "x").is_empty());
        assert!(sweep_world_registries(&r, "x").is_empty());
    }
}
