#![cfg(feature = "engine")]
use rightkit_child_process::OwnedCommand;
use rightkit_tauri_shell::engine::{
    EngineHost, EnginePolicy, EngineSpec, RestartMode, RestartPolicy,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn host(mode: RestartMode, max_restarts: u32) -> EngineHost {
    let exe = std::env::current_exe().unwrap();
    let fixture = exe
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
        .join(if cfg!(windows) {
            "shell_engine_fixture.exe"
        } else {
            "shell_engine_fixture"
        });
    assert!(
        fixture.is_file(),
        "build fixture with cargo build --features engine --example shell_engine_fixture"
    );
    let mut spec = EngineSpec::new(move || OwnedCommand::new(&fixture));
    spec.hello_timeout = Duration::from_secs(5);
    spec.shutdown_grace = Duration::from_millis(500);
    spec.call_timeout = Some(Duration::from_secs(2));
    let mut policy = EnginePolicy::default()
        .allow_methods(["echo", "slow", "crash", "notify"])
        .method_timeout("slow", Duration::from_millis(20))
        .restart_policy(
            RestartPolicy::new(
                mode,
                max_restarts,
                Duration::from_secs(60),
                Duration::from_millis(5),
                Duration::from_millis(10),
            )
            .unwrap(),
        );
    policy.keep_alive_on_timeout = true;
    EngineHost::with_policy(spec, policy)
}

#[test]
fn concurrent_calls_keep_request_ids_tags_and_method_names_separate() {
    let host = host(RestartMode::Disabled, 0);
    host.start().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    std::thread::scope(|scope| {
        for i in 0..4 {
            let (host, events) = (&host, events.clone());
            scope.spawn(move || {
                let value = host
                    .call_tagged(
                        "echo",
                        json!({"i": i}),
                        Some(format!("tag-{i}")),
                        Arc::new(move |event| events.lock().unwrap().push(event)),
                    )
                    .unwrap();
                assert_eq!(value, json!({"i": i}));
            });
        }
    });
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 4);
    let ids: std::collections::HashSet<_> = events.iter().map(|e| e.id.unwrap()).collect();
    assert_eq!(ids.len(), 4);
    for i in 0..4 {
        assert!(events
            .iter()
            .any(|e| e.call == "echo" && e.tag.as_deref() == Some(format!("tag-{i}").as_str())));
    }
    drop(events);
    host.shutdown();
    assert_eq!(host.lifecycle()["state"], "stopped");
    assert_eq!(host.start().unwrap_err().code, "shutting_down");
}

#[test]
fn method_timeout_can_abandon_wait_without_killing_child() {
    let host = host(RestartMode::Disabled, 0);
    let hello = host.start().unwrap();
    let error = host
        .call_tagged("slow", Value::Null, None, Arc::new(|_| {}))
        .unwrap_err();
    assert_eq!(error.code, "timeout");
    assert_eq!(
        host.call_tagged("echo", json!({"ok": true}), None, Arc::new(|_| {}))
            .unwrap(),
        json!({"ok": true})
    );
    assert_eq!(host.hello().unwrap()["pid"], hello["pid"]);
    host.shutdown();
}

#[test]
fn automatic_restart_spends_budget_and_never_replays_failed_call() {
    let host = host(RestartMode::Automatic, 1);
    let events = host.subscribe_lifecycle().unwrap();
    host.start().unwrap();
    assert!(host
        .call_tagged("crash", Value::Null, None, Arc::new(|_| {}))
        .is_err());
    loop {
        let event = events.recv_timeout(Duration::from_secs(5)).unwrap();
        if event.state == rightkit_job_queue::Lifecycle::Ready && event.generation > 1 {
            break;
        }
    }
    assert_eq!(host.lifecycle()["restarts"], 1);
    assert!(host
        .call_tagged("crash", Value::Null, None, Arc::new(|_| {}))
        .is_err());
    loop {
        let event = events.recv_timeout(Duration::from_secs(5)).unwrap();
        if event.state == rightkit_job_queue::Lifecycle::Down {
            break;
        }
    }
    assert_eq!(host.lifecycle()["state"], "failed");
    assert_eq!(host.lifecycle()["restarts"], 1);
    host.shutdown();
}

#[test]
fn disabled_restart_stays_failed_and_on_demand_waits_for_next_call() {
    for mode in [RestartMode::Disabled, RestartMode::OnDemand] {
        let host = host(mode, 1);
        let events = host.subscribe_lifecycle().unwrap();
        host.start().unwrap();
        assert!(host
            .call_tagged("crash", Value::Null, None, Arc::new(|_| {}))
            .is_err());
        loop {
            let event = events.recv_timeout(Duration::from_secs(5)).unwrap();
            if event.state == rightkit_job_queue::Lifecycle::Down {
                break;
            }
        }
        if mode == RestartMode::Disabled {
            assert_eq!(host.lifecycle()["state"], "failed");
            assert!(host
                .call_tagged("echo", Value::Null, None, Arc::new(|_| {}))
                .is_err());
            assert_eq!(host.lifecycle()["restarts"], 0);
        } else {
            assert_eq!(host.lifecycle()["state"], "restarting");
            assert_eq!(host.lifecycle()["restarts"], 0);
            host.call_tagged("echo", Value::Null, None, Arc::new(|_| {}))
                .unwrap();
            assert_eq!(host.lifecycle()["state"], "ready");
            assert_eq!(host.lifecycle()["restarts"], 1);
        }
        host.shutdown();
    }
}

#[test]
fn js_bridge_exposes_tagged_calls_status_clipboard_item_and_site_grants() {
    // Present in a repository checkout; a packaged crate has no `packages/` and skips.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/tauri-shell/src/index.ts");
    let Ok(ts) = std::fs::read_to_string(&path) else {
        return;
    };
    for needle in [
        "id?: number | null",
        "tag?: string | null",
        "callTagged:",
        "rk_shell_engine_call_tagged",
        "rk_shell_engine_status",
        "item: () => invoke<ClipboardItem>('rk_shell_clipboard_item')",
        "rk_shell_site_grant",
        "rk_shell_site_revoke",
        "details?: unknown",
        // The pre-existing wrappers stay exactly as they were.
        "call: <T = unknown>(method: string, params?: unknown) => invoke<T>('rk_shell_engine_call'",
        "read: async (board?: string)",
    ] {
        assert!(ts.contains(needle), "missing {needle}");
    }
}

#[test]
fn unsolicited_events_are_available_without_a_request_sink() {
    let host = host(RestartMode::Disabled, 0);
    let events = host.subscribe_events().unwrap();
    host.start().unwrap();
    host.call_tagged("notify", Value::Null, None, Arc::new(|_| {}))
        .unwrap();
    assert_eq!(
        events.recv_timeout(Duration::from_secs(2)).unwrap()["event"]["notice"],
        "unsolicited"
    );
    host.shutdown();
}
