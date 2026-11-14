// rightkit-agent-daemon 0.1.1 consumed by exact registry pin (`=0.1.1`), outside the workspace.
// Drives the source-trust gate, the settings DTO, the live-turn fence, the lifecycle parser and one
// lifecycle authority over an in-memory backend. Process memory only; async calls run on a minimal
// std executor.
use std::future::Future;
use std::pin::pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use async_trait::async_trait;
use http::{HeaderMap, HeaderValue, StatusCode};
use rightkit_agent_daemon::{
    enforce_trusted_source_authority, lower_source_trust, parse_lifecycle_command, BackendOutcome,
    DaemonSettingsStore, LaunchMode, LifecycleAuthority, LifecycleBackend, LifecycleCommand,
    LifecycleState, LifecycleSurface, LiveTurnHandle, ProcessEntry, ScheduleCommand,
};

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
        std::thread::yield_now();
    }
}

fn trust_header(value: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-rightkit-agent-source-trust",
        HeaderValue::from_str(value).expect("header value"),
    );
    headers
}

/// In-memory backend: records every call; `restart` fails on demand.
struct MemoryBackend {
    calls: Arc<Mutex<Vec<String>>>,
    fail_restart: bool,
}

impl MemoryBackend {
    fn record(&self, call: &str) -> BackendOutcome {
        self.calls.lock().unwrap().push(call.to_string());
        BackendOutcome {
            detail: call.to_string(),
            ..BackendOutcome::default()
        }
    }
}

#[async_trait]
impl LifecycleBackend for MemoryBackend {
    async fn start(&mut self, mode: LaunchMode) -> Result<BackendOutcome, String> {
        let mut outcome = self.record(&format!("start {mode:?}"));
        outcome.generation = Some(7);
        Ok(outcome)
    }
    async fn attach(&mut self, client_id: &str) -> Result<BackendOutcome, String> {
        Ok(self.record(&format!("attach {client_id}")))
    }
    async fn detach(&mut self, client_id: Option<&str>) -> Result<BackendOutcome, String> {
        Ok(self.record(&format!("detach {client_id:?}")))
    }
    async fn stop(&mut self, force: bool) -> Result<BackendOutcome, String> {
        Ok(self.record(&format!("stop {force}")))
    }
    async fn restart(&mut self) -> Result<BackendOutcome, String> {
        self.record("restart");
        if self.fail_restart {
            return Err("restart refused".to_string());
        }
        Ok(BackendOutcome::default())
    }
    async fn update(&mut self, artifact: &str) -> Result<BackendOutcome, String> {
        Ok(self.record(&format!("update {artifact}")))
    }
    async fn schedule_list(&mut self, include_inactive: bool) -> Result<BackendOutcome, String> {
        Ok(self.record(&format!("schedule list {include_inactive}")))
    }
    async fn schedule_add(
        &mut self,
        session_id: &str,
        schedule: &str,
        prompt: &str,
    ) -> Result<BackendOutcome, String> {
        Ok(self.record(&format!("schedule add {session_id} {schedule} {prompt}")))
    }
    async fn schedule_cancel(&mut self, job_id: &str) -> Result<BackendOutcome, String> {
        Ok(self.record(&format!("schedule cancel {job_id}")))
    }
    async fn process_list(&mut self, reap: bool, force: bool) -> Result<BackendOutcome, String> {
        let mut outcome = self.record(&format!("ps {reap} {force}"));
        outcome.processes = vec![ProcessEntry {
            pid: 42,
            role: "daemon".to_string(),
            state: "running".to_string(),
            owned: true,
        }];
        Ok(outcome)
    }
}

fn main() {
    // Source-trust gate: absent is trusted, lower-trust markers are refused, unknown is ignored.
    check(
        "absent header is trusted",
        enforce_trusted_source_authority(&HeaderMap::new()).is_ok()
            && lower_source_trust(&HeaderMap::new()) == Ok(None),
    );
    check(
        "untrusted marker is refused",
        enforce_trusted_source_authority(&trust_header("Remote_Agent-Untrusted"))
            == Err(StatusCode::FORBIDDEN),
    );
    check(
        "tool-untrusted marker is lowered",
        lower_source_trust(&trust_header("tool-untrusted"))
            .map(|trust| trust.is_some())
            .unwrap_or(false),
    );
    check(
        "unknown marker is ignored",
        enforce_trusted_source_authority(&trust_header("trusted")).is_ok(),
    );

    // Settings defaults: no spend cap, ten-minute sub-agent bound, no root-turn cap.
    let settings = DaemonSettingsStore::default();
    check(
        "settings defaults",
        settings.per_task_max_usd == 0.0
            && settings.max_wall_clock_minutes == 10
            && settings.turn_max_wall_clock_minutes == 0
            && settings.max_workers == 4
            && !settings.keep_daemon_resident,
    );

    // Live-turn fence: a stale guard dropped late does not mark the live turn idle.
    let live = LiveTurnHandle::default();
    check(
        "idle before any turn",
        !live.is_active() && live.current_generation() == 0,
    );
    let first = live.begin();
    let second = live.begin();
    check(
        "newer turn supersedes older",
        !first.is_current() && second.is_current() && second.generation() == 2,
    );
    drop(first);
    check("stale guard leaves session active", live.is_active());
    drop(second);
    check("current guard clears session", !live.is_active());

    // Lifecycle parser.
    check(
        "parse start background",
        parse_lifecycle_command(&["start", "-d"])
            == Ok(LifecycleCommand::Start {
                mode: LaunchMode::Background,
            }),
    );
    check(
        "parse shutdown force",
        parse_lifecycle_command(&["shutdown", "--force"])
            == Ok(LifecycleCommand::Stop { force: true }),
    );
    check(
        "parse schedule add",
        parse_lifecycle_command(&["cron", "add", "s1", "0", "9", "*", "--", "check", "mail"])
            == Ok(LifecycleCommand::Schedule(ScheduleCommand::Add {
                session_id: "s1".to_string(),
                schedule: "0 9 *".to_string(),
                prompt: "check mail".to_string(),
            })),
    );
    check(
        "parse ps reap",
        parse_lifecycle_command(&["ps", "--reap"])
            == Ok(LifecycleCommand::ProcessList {
                reap: true,
                force: false,
            }),
    );
    check(
        "parse refusals",
        parse_lifecycle_command(&[]).is_err()
            && parse_lifecycle_command(&["start", "--bogus"]).is_err()
            && parse_lifecycle_command(&["attach"]).is_err()
            && parse_lifecycle_command(&["schedule", "add", "s1", "--", "x"]).is_err()
            && parse_lifecycle_command(&["teleport"]).is_err(),
    );

    // One authority over an in-memory backend.
    let calls = Arc::new(Mutex::new(Vec::new()));
    let authority = LifecycleAuthority::new(MemoryBackend {
        calls: calls.clone(),
        fail_restart: true,
    });
    let shared = authority.clone();
    check(
        "starts stopped",
        block_on(authority.snapshot()).state == LifecycleState::Stopped,
    );
    check(
        "schedule refused while stopped",
        block_on(
            authority.execute_cli(LifecycleCommand::Schedule(ScheduleCommand::List {
                include_inactive: false,
            })),
        )
        .is_err(),
    );
    let started = block_on(authority.execute_cli(LifecycleCommand::Start {
        mode: LaunchMode::Foreground,
    }))
    .expect("start");
    check(
        "start runs with backend generation",
        started.surface == LifecycleSurface::Cli
            && started.snapshot.state == LifecycleState::Running
            && started.snapshot.generation == 7,
    );
    let again = block_on(shared.execute_desktop(LifecycleCommand::Start {
        mode: LaunchMode::Background,
    }))
    .expect("second start");
    check(
        "second start is idempotent with no backend call",
        again.detail == "daemon already running"
            && calls
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c.starts_with("start"))
                .count()
                == 1,
    );
    check(
        "empty attach refused",
        block_on(authority.execute_tui(LifecycleCommand::Attach {
            client_id: " ".to_string(),
        }))
        .is_err(),
    );
    let attached = block_on(shared.execute_extension(LifecycleCommand::Attach {
        client_id: "ext-1".to_string(),
    }))
    .expect("attach");
    check(
        "attach is shared across clones",
        attached.snapshot.attached_clients == vec!["ext-1".to_string()]
            && block_on(authority.snapshot()).attached_clients == vec!["ext-1".to_string()],
    );
    let ps = block_on(authority.execute_cli(LifecycleCommand::ProcessList {
        reap: false,
        force: false,
    }))
    .expect("ps");
    check(
        "process list from backend",
        ps.processes.len() == 1 && ps.processes[0].pid == 42 && ps.processes[0].owned,
    );
    check(
        "failed restart is an error",
        block_on(authority.execute_cli(LifecycleCommand::Restart))
            == Err("restart refused".to_string()),
    );
    check(
        "failed restart leaves state failed",
        block_on(shared.snapshot()).state == LifecycleState::Failed,
    );
    let stopped =
        block_on(authority.execute_cli(LifecycleCommand::Stop { force: false })).expect("stop");
    check(
        "stop reaches backend and stops",
        stopped.snapshot.state == LifecycleState::Stopped
            && calls.lock().unwrap().last().map(String::as_str) == Some("stop false"),
    );

    println!("OK rightkit-agent-daemon 0.1.1 (registry consumer)");
}
