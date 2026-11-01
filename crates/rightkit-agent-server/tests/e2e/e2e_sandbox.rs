//! Windows AppContainer sandbox escalation journey through a real daemon.
//! Proves a denied external read reaches the operator, grants only that path
//! for a payload-bound retry, then revokes the grant after the child exits.

#[cfg(windows)]
use crate::support::daemon_process;
#[cfg(windows)]
use crate::support::e2e_api;
#[cfg(windows)]
use crate::support::scripted_provider;

#[cfg(windows)]
mod windows_e2e {
    use super::e2e_api::*;
    use super::scripted_provider::Step;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    struct LoopbackServer {
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<usize>>,
    }

    impl LoopbackServer {
        fn finish(mut self) -> usize {
            self.stop.store(true, Ordering::Release);
            self.thread
                .take()
                .unwrap()
                .join()
                .expect("loopback server thread")
        }
    }

    impl Drop for LoopbackServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    #[test]
    fn headless_sandbox_denial_returns_tool_error_without_a_grant_question() {
        use crate::support::e2e_tools as t;

        let external = tempfile::tempdir().expect("external fixture");
        let path = external.path().join("headless-read.txt");
        std::fs::write(&path, "ungranted-read-proof").unwrap();
        let command = format!(
            "Get-Content -LiteralPath '{}' -Raw",
            path.to_string_lossy().replace('\'', "''")
        );
        let provider = t::sequence(
            vec![t::call("shell", json!({"command": command}))],
            "denial handled",
        );
        let rig = t::Rig::new_unversioned(&provider, &[]);
        let out = rig.run("read the external fixture");
        let results = t::expect_results(&out, 1);
        assert!(results[0].error, "{}", results[0].text);
        assert!(results[0].text.contains("sandbox-denial:filesystem"));
        assert!(!out.stdout.contains("userQuestionAsked"), "{}", out.stdout);
        assert!(!results[0].text.contains("ungranted-read-proof"));
    }

    #[test]
    fn external_read_denial_is_approved_once_and_retried_with_temporary_grant() {
        let external = tempfile::tempdir().expect("external fixture directory");
        let secret_path = external.path().join("read-me.txt");
        std::fs::write(&secret_path, "granted-read-proof").expect("write external fixture");
        let escaped = secret_path.to_string_lossy().replace('\'', "''");
        let command = format!("Get-Content -LiteralPath '{escaped}' -Raw");

        let rig = Rig::start(vec![
            Step::tool("sandbox-read", "shell", json!({"command": command})),
            Step::text("sandbox retry completed"),
            Step::tool("sandbox-read-again", "shell", json!({"command": command})),
            Step::text("second sandbox read remained blocked"),
        ]);
        rig.session("sandbox-grant", json!({"permissionMode": "bypass"}));
        assert_eq!(
            rig.send("sandbox-grant", "read the external fixture").0,
            200
        );

        let asked = rig.wait_event("sandbox-grant", "userQuestionAsked");
        let payload = asked.get("data").unwrap_or(&asked);
        let questions = payload["questions"]
            .as_array()
            .expect("sandbox question list");
        assert_eq!(questions[0]["header"], "Sandbox");
        assert!(questions[0]["question"]
            .as_str()
            .unwrap()
            .contains("read access"));
        assert!(
            questions[0]["question"]
                .as_str()
                .unwrap()
                .contains(&secret_path.to_string_lossy().to_string()),
            "expected path {} in approval: {payload}",
            secret_path.display()
        );
        assert_eq!(
            rig.provider.request_count(),
            1,
            "retry must wait for operator approval"
        );

        let question_id = payload["questionId"].as_str().expect("question id");
        let (status, answer) = rig.post(
            "/sessions/sandbox-grant/answer-question",
            json!({
                "questionId": question_id,
                "answers": {"Sandbox": "Approve rerun"}
            }),
        );
        assert_eq!(status, 200, "{answer}");
        rig.wait_assistant("sandbox-grant", "sandbox retry completed");

        let requests = rig.provider.requests();
        let messages = requests.last().unwrap()["messages"].as_array().unwrap();
        let results: Vec<_> = messages
            .iter()
            .filter(|message| message["role"] == "tool")
            .collect();
        let denied = results
            .iter()
            .find(|message| message["tool_call_id"] == "sandbox-read")
            .expect("original denial reaches the model");
        assert!(denied["content"]
            .as_str()
            .unwrap()
            .contains("sandbox-denial:filesystem"));
        let retry = results
            .iter()
            .find(|message| message["tool_call_id"] == "sandbox-read.sandbox-grant-retry")
            .expect("one payload-bound retry reaches the model");
        assert!(retry["content"]
            .as_str()
            .unwrap()
            .contains("granted-read-proof"));
        assert_eq!(results.len(), 2, "one denial and one approved retry only");

        // The ACL grant must be a lease tied to the child lifetime, not a
        // session/workspace permission. A later identical call asks again.
        assert_eq!(
            rig.send("sandbox-grant", "read the external fixture again")
                .0,
            200
        );
        let second_question = rig.wait("second sandbox approval request", 60, || {
            let questions: Vec<_> = rig
                .history("sandbox-grant")
                .into_iter()
                .filter(|event| kind_of(event) == "userQuestionAsked")
                .collect();
            (questions.len() >= 2).then(|| questions.last().unwrap().clone())
        });
        let payload = second_question.get("data").unwrap_or(&second_question);
        let second_question_id = payload["questionId"].as_str().expect("second question id");
        let (status, answer) = rig.post(
            "/sessions/sandbox-grant/answer-question",
            json!({
                "questionId": second_question_id,
                "answers": {"Sandbox": "Deny"}
            }),
        );
        assert_eq!(status, 200, "{answer}");
        rig.wait_assistant("sandbox-grant", "second sandbox read remained blocked");
        let requests = rig.provider.requests();
        let messages = requests.last().unwrap()["messages"].as_array().unwrap();
        let second_denial = messages
            .iter()
            .find(|message| message["tool_call_id"] == "sandbox-read-again")
            .expect("later call has its own denial");
        assert!(second_denial["content"]
            .as_str()
            .unwrap()
            .contains("sandbox-denial:filesystem"));
    }

    #[test]
    fn network_bypass_and_one_command_network_grant_are_not_persisted() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("loopback listener");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let mut served = 0;
            // Lifetime follows the fixture, including panic cleanup. Boot &
            // approval latency must not expire the server before the retry.
            while served < 2 && !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                        stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
                        let mut request = [0u8; 2048];
                        let _ = stream.read(&mut request);
                        let body = b"loopback-network-proof";
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        stream.write_all(response.as_bytes()).unwrap();
                        stream.write_all(body).unwrap();
                        served += 1;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(25));
                    }
                    Err(error) => panic!("loopback accept: {error}"),
                }
            }
            served
        });
        let server = LoopbackServer {
            stop,
            thread: Some(thread),
        };
        let command = format!(
            "(Invoke-WebRequest -UseBasicParsing -TimeoutSec 5 -Proxy $null 'http://127.0.0.1:{}/').Content",
            address.port()
        );
        let rig = Rig::start(vec![
            Step::tool("bypass-network", "shell", json!({"command": command})),
            Step::text("bypass network completed"),
            Step::tool("strict-network", "shell", json!({"command": command})),
            Step::text("approved network completed"),
            Step::tool("strict-network-again", "shell", json!({"command": command})),
            Step::text("network grant expired"),
        ]);

        rig.session(
            "sandbox-network-bypass",
            json!({"permissionMode": "bypass"}),
        );
        assert_eq!(rig.send("sandbox-network-bypass", "fetch loopback").0, 200);
        rig.wait_assistant("sandbox-network-bypass", "bypass network completed");
        let bypass_requests = rig.provider.requests();
        let bypass_result = bypass_requests.last().unwrap()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["tool_call_id"] == "bypass-network")
            .expect("Bypass network tool result");
        assert!(
            bypass_result["content"]
                .as_str()
                .unwrap()
                .contains("loopback-network-proof"),
            "bypass result: {bypass_result}"
        );

        rig.session(
            "sandbox-network-strict",
            // CG-B02: Strict is a sandbox tier, not a permission mode.
            // Ask preserves that tier while requiring shell-effect approval.
            json!({"permissionMode": "ask"}),
        );
        assert_eq!(rig.send("sandbox-network-strict", "fetch loopback").0, 200);
        rig.wait_event("sandbox-network-strict", "toolApprovalRequested");
        let (status, approval) = rig.post(
            "/sessions/sandbox-network-strict/tool-approval",
            json!({"toolCallId": "strict-network", "approved": true}),
        );
        assert_eq!(status, 200, "{approval}");
        let first_question = rig.wait_event("sandbox-network-strict", "userQuestionAsked");
        let payload = first_question.get("data").unwrap_or(&first_question);
        assert!(payload["questions"][0]["question"]
            .as_str()
            .unwrap()
            .contains("network"));
        let question_id = payload["questionId"]
            .as_str()
            .expect("network approval question id");
        let (status, answer) = rig.post(
            "/sessions/sandbox-network-strict/answer-question",
            json!({
                "questionId": question_id,
                "answers": {"Sandbox": "Approve rerun"}
            }),
        );
        assert_eq!(status, 200, "{answer}");
        rig.wait_assistant("sandbox-network-strict", "approved network completed");
        let requests = rig.provider.requests();
        let messages = requests.last().unwrap()["messages"].as_array().unwrap();
        let granted = messages
            .iter()
            .find(|message| message["tool_call_id"] == "strict-network.sandbox-grant-retry")
            .expect("strict rerun has command-scoped network grant");
        assert!(granted["content"]
            .as_str()
            .unwrap()
            .contains("loopback-network-proof"));

        assert_eq!(
            rig.send("sandbox-network-strict", "fetch loopback again").0,
            200
        );
        rig.wait("second shell-effect approval", 60, || {
            rig.history("sandbox-network-strict")
                .into_iter()
                .find(|event| {
                    let payload = event.get("data").unwrap_or(event);
                    kind_of(event) == "toolApprovalRequested"
                        && payload["call"]["id"] == "strict-network-again"
                })
        });
        let (status, approval) = rig.post(
            "/sessions/sandbox-network-strict/tool-approval",
            json!({"toolCallId": "strict-network-again", "approved": true}),
        );
        assert_eq!(status, 200, "{approval}");
        let second_question = rig.wait("network grant expiry prompt", 60, || {
            let questions: Vec<_> = rig
                .history("sandbox-network-strict")
                .into_iter()
                .filter(|event| kind_of(event) == "userQuestionAsked")
                .collect();
            (questions.len() >= 2).then(|| questions.last().unwrap().clone())
        });
        let payload = second_question.get("data").unwrap_or(&second_question);
        let second_id = payload["questionId"]
            .as_str()
            .expect("second network question id");
        let (status, answer) = rig.post(
            "/sessions/sandbox-network-strict/answer-question",
            json!({"questionId": second_id, "answers": {"Sandbox": "Deny"}}),
        );
        assert_eq!(status, 200, "{answer}");
        rig.wait_assistant("sandbox-network-strict", "network grant expired");
        let final_messages = rig.provider.requests().last().unwrap()["messages"]
            .as_array()
            .unwrap()
            .clone();
        let second_denial = final_messages
            .iter()
            .find(|message| message["tool_call_id"] == "strict-network-again")
            .expect("later strict call is denied again");
        assert!(second_denial["content"]
            .as_str()
            .unwrap()
            .contains("sandbox-denial:network"));
        assert_eq!(server.finish(), 2);
    }
}
