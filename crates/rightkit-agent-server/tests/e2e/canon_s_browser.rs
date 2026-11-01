//! S:A15 browser observations, serial actions, navigation guard & human lease.

use crate::support::e2e_api::*;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

struct LocalPage {
    url: String,
    receiver: mpsc::Receiver<()>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl LocalPage {
    fn start(expected_requests: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local browser page");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let (sender, receiver) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = std::thread::spawn(move || {
            let mut served = 0;
            // Navigation guards also fetch this URL. Keep serving until
            // teardown, rather than closing after an assumed Chrome count.
            while !worker_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(Duration::from_secs(1))).ok();
                        stream.set_write_timeout(Some(Duration::from_secs(1))).ok();
                        let mut request = [0u8; 2048];
                        let bytes = stream.read(&mut request).unwrap_or(0);
                        let request_line = String::from_utf8_lossy(&request[..bytes]);
                        let path = request_line
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap_or("/");
                        let body = "<!doctype html><title>S-A15</title><p id='marker'>stable observation</p>";
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(), body
                        );
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.flush();
                        if path == "/" {
                            served += 1;
                            if served == expected_requests {
                                let _ = sender.send(());
                            }
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            url,
            receiver,
            stop,
            worker: Some(worker),
        }
    }

    fn wait(self) {
        self.receiver
            .recv_timeout(Duration::from_secs(15))
            .expect("browser page requests");
    }
}

impl Drop for LocalPage {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[test]
fn browser_observations_advance_per_action_and_operator_lease_pauses_agent() {
    let page = LocalPage::start(3);
    let rig = Rig::start(vec![]);
    rig.session("s-a15", json!({"permissionMode":"bypass"}));
    let browser_url = page.url.clone();

    let (status, opened) = rig.post(
        "/sessions/s-a15/browser/workspace/open",
        json!({"url":browser_url}),
    );
    assert_eq!(status, 200, "browser open: {opened}");
    let browser_id = opened["sessionId"].as_str().expect("browser session id");

    let navigate = || {
        rig.post(
            "/sessions/s-a15/browser/workspace/navigate",
            json!({"browserSessionId":browser_id, "url":browser_url}),
        )
    };
    let (status, first) = navigate();
    assert_eq!(status, 200, "first browser observation: {first}");
    let first_observation = &first["observation"];
    assert_eq!(first_observation["schemaVersion"], 1, "{first_observation}");
    let first_epoch = first_observation["epoch"]
        .as_u64()
        .expect("observation epoch");
    assert!(
        first_observation["frameIds"].as_array().is_some(),
        "{first_observation}"
    );

    let (status, second) = navigate();
    assert_eq!(status, 200, "second browser observation: {second}");
    let second_observation = &second["observation"];
    let second_epoch = second_observation["epoch"]
        .as_u64()
        .expect("next observation epoch");
    assert!(
        second_epoch > first_epoch,
        "each action must return a fresh observation: {first_observation} -> {second_observation}"
    );

    let (status, control) = rig.post("/sessions/s-a15/browser/workspace/take-control", json!({}));
    assert_eq!(status, 200, "operator take control: {control}");
    assert_eq!(control["control"], "human");
    assert_eq!(control["paused"], true);
    let (status, blocked) = navigate();
    assert_eq!(
        status, 409,
        "agent action must stop while human owns browser: {blocked}"
    );
    assert_eq!(blocked["code"], "browser_paused");

    let (status, released) = rig.post(
        "/sessions/s-a15/browser/workspace/release-control",
        json!({}),
    );
    assert_eq!(status, 200, "release operator lease: {released}");
    assert_eq!(released["control"], "agent");
    assert_eq!(released["paused"], false);
    let (status, closed) = rig.post(
        "/sessions/s-a15/browser/workspace/close",
        json!({"sessionId":browser_id}),
    );
    assert_eq!(status, 200, "browser cleanup: {closed}");
    assert_eq!(closed["closed"], browser_id);
    let (status, workspace) = rig.get("/sessions/s-a15/browser/workspace");
    assert_eq!(status, 200, "workspace after close: {workspace}");
    assert!(
        workspace["sessions"]
            .as_array()
            .expect("browser sessions")
            .is_empty(),
        "closed browser must leave workspace: {workspace}"
    );
    page.wait();

    // On Windows, surviving Chrome helpers keep profile files open and make
    // TempDir cleanup silently fail. Observe cleanup after owner/tree reaping.
    let home_path = rig.home.path().to_path_buf();
    let workspace_path = rig.ws().to_path_buf();
    drop(rig);
    assert!(
        !home_path.exists(),
        "fixture home leaked: {}",
        home_path.display()
    );
    assert!(
        !workspace_path.exists(),
        "fixture workspace leaked: {}",
        workspace_path.display()
    );
}

#[test]
fn browser_navigation_guard_refuses_private_metadata_before_action() {
    let rig = Rig::start(vec![]);
    rig.session("s-a15-guard", json!({"permissionMode":"bypass"}));
    let (status, denied) = rig.post(
        "/sessions/s-a15-guard/browser/workspace/open",
        json!({"url":"http://169.254.169.254/latest/meta-data/"}),
    );
    assert!(
        status == 400 || status == 403,
        "private metadata must be refused: {status} {denied}"
    );
    let (status, workspace) = rig.get("/sessions/s-a15-guard/browser/workspace");
    assert_eq!(
        status, 200,
        "workspace after denied navigation: {workspace}"
    );
    assert_eq!(
        workspace["sessions"].as_array().unwrap().len(),
        0,
        "guard must run before browser launch"
    );
}
