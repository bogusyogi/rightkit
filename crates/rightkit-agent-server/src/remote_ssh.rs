//! `{app} ssh <destination>`: use {app} running on another machine
//! from this one, over SSH. Available on a host that turns `host_options::remote_ssh` on (the
//! remote machine's `{app} web` prints the URL, so the remote needs a browser client too).
//!
//! 1. `ssh <destination> <remote-bin> web` starts (or finds) the remote daemon
//!    and prints its browser-client URL, `http://127.0.0.1:<rport>/web#token=…`.
//! 2. `ssh -N -L <lport>:127.0.0.1:<rport> <destination>` forwards a local port
//!    to the remote daemon's loopback port; the daemon never listens on the
//!    network and the token never crosses it in clear text (SSH carries both).
//! 3. The local URL `http://127.0.0.1:<lport>/web#token=…` is printed (and
//!    opened with `--open`); the forward runs until Ctrl-C.
//!
//! Authentication is the user's own SSH setup (keys, agent, `~/.ssh/config`);
//! nothing is stored. Every `ssh` runs with `BatchMode=yes` and the host-key policy the user's
//! own configuration sets: this module never turns host-key checking off, so an unknown or
//! changed host key fails the call instead of prompting. Arguments reach `ssh` as an argument
//! vector (no local shell); the destination follows `--`. For the terminal UI,
//! `ssh -t <destination> {app}` already works: the TUI is a terminal program.

use std::process::Stdio;

/// What the remote `{app} web` printed: its loopback port and token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteWeb {
    pub(crate) port: u16,
    pub(crate) token: String,
}

/// Parse `http://127.0.0.1:<port>/web#token=<token>` (the last such line).
pub(crate) fn parse_remote_web_url(output: &str) -> Option<RemoteWeb> {
    output.lines().rev().find_map(|line| {
        let rest = line.trim().strip_prefix("http://127.0.0.1:")?;
        let (port, rest) = rest.split_once('/')?;
        let token = rest.strip_prefix("web#token=")?;
        let port = port.parse().ok()?;
        (!token.is_empty()).then(|| RemoteWeb {
            port,
            token: token.to_string(),
        })
    })
}

fn ssh_base(ssh_port: Option<u16>, ssh_options: &[String]) -> Vec<String> {
    // BatchMode: never prompt (a password prompt would hang a script); the
    // user's keys or agent authenticate.
    let mut args = vec!["-o".to_string(), "BatchMode=yes".to_string()];
    if let Some(port) = ssh_port {
        args.push("-p".into());
        args.push(port.to_string());
    }
    for option in ssh_options {
        args.push("-o".into());
        args.push(option.clone());
    }
    args
}

fn free_local_port() -> Result<u16, String> {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .map(|addr| addr.port())
        .map_err(|error| format!("no free local port: {error}"))
}

/// GET `/health` (unauthenticated, no token) on the forwarded port and check the reply
/// is this app's daemon's.
async fn forward_answers_as_daemon(local_port: u16) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let probe = async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", local_port))
            .await
            .ok()?;
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .await
            .ok()?;
        let mut reply = Vec::new();
        stream.take(16 * 1024).read_to_end(&mut reply).await.ok()?;
        Some(reply)
    };
    match tokio::time::timeout(std::time::Duration::from_secs(3), probe).await {
        Ok(Some(reply)) => health_reply_is_daemon(&reply),
        _ => false,
    }
}

/// True when a raw HTTP reply is a 200 whose JSON body says `ok` from `{app}-daemon`.
fn health_reply_is_daemon(reply: &[u8]) -> bool {
    let text = String::from_utf8_lossy(reply);
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return false;
    };
    let status_ok = head
        .lines()
        .next()
        .is_some_and(|line| line.split_whitespace().nth(1) == Some("200"));
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body.trim()) else {
        return false;
    };
    status_ok
        && json.get("ok") == Some(&serde_json::Value::Bool(true))
        && json.get("service").and_then(serde_json::Value::as_str)
            == Some(crate::host_identity::prefixed("daemon").as_str())
}

/// End the forward. A failure to kill is noted: the child is also killed on drop.
async fn stop_forward(forward: &mut tokio::process::Child) {
    if let Err(error) = forward.kill().await {
        tracing::debug!(%error, "the SSH forward could not be killed; it is killed on drop");
    }
}

pub(crate) struct RemoteArgs {
    pub(crate) destination: String,
    pub(crate) local_port: Option<u16>,
    pub(crate) ssh_port: Option<u16>,
    pub(crate) ssh_options: Vec<String>,
    pub(crate) remote_bin: String,
    pub(crate) open: bool,
}

/// Why `ssh` must not be handed this destination or option, if it must not: an option-like
/// destination, a blank or whitespace or control character in a destination, a `-o` option that
/// is blank, starts with `-` or carries a control character (a newline would start a second
/// directive).
fn refused_argument(args: &RemoteArgs) -> Option<String> {
    let destination = args.destination.as_str();
    if destination.starts_with('-') {
        return Some("the destination must be a host, not an option".to_string());
    }
    if destination.is_empty()
        || destination
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Some("the destination must be a host name or address".to_string());
    }
    args.ssh_options
        .iter()
        .find(|option| {
            option.trim().is_empty()
                || option.starts_with('-')
                || option.chars().any(char::is_control)
        })
        .map(|option| format!("--ssh-option {option:?} is not an ssh `-o` option"))
}

pub(crate) async fn run(args: RemoteArgs) -> i32 {
    if let Some(message) = refused_argument(&args) {
        eprintln!("error: {message}");
        return 2;
    }
    let base = ssh_base(args.ssh_port, &args.ssh_options);
    // 1. Start or find the remote daemon and read its URL.
    let probe = rightkit_child_process::tokio_support::command("ssh")
        .args(&base)
        .arg("--")
        .arg(&args.destination)
        .arg(format!("{} web", args.remote_bin))
        .stdin(Stdio::null())
        .output()
        .await;
    let probe = match probe {
        Ok(output) => output,
        Err(error) => {
            eprintln!("error: could not run ssh: {error}");
            return 1;
        }
    };
    if !probe.status.success() {
        eprintln!(
            "error: `{} web` on {} failed: {}",
            args.remote_bin,
            args.destination,
            String::from_utf8_lossy(&probe.stderr).trim()
        );
        return 1;
    }
    let Some(remote) = parse_remote_web_url(&String::from_utf8_lossy(&probe.stdout)) else {
        eprintln!(
            "error: {} did not print a {} web URL (is `{}` the {} binary there?)",
            args.destination,
            crate::host_identity::app(),
            args.remote_bin,
            crate::host_identity::app()
        );
        return 1;
    };
    // 2. Forward a local port to the remote daemon's loopback port.
    let local_port = match args.local_port {
        Some(port) => {
            // Something already listening there would receive the credential URL.
            if std::net::TcpListener::bind(("127.0.0.1", port)).is_err() {
                eprintln!("error: 127.0.0.1:{port} is already in use; pick another --local-port");
                return 1;
            }
            port
        }
        None => match free_local_port() {
            Ok(port) => port,
            Err(message) => {
                eprintln!("error: {message}");
                return 1;
            }
        },
    };
    let mut forward = match rightkit_child_process::tokio_support::command("ssh")
        .args(&base)
        .args(["-o", "ExitOnForwardFailure=yes", "-N", "-L"])
        .arg(format!("127.0.0.1:{local_port}:127.0.0.1:{}", remote.port))
        .arg("--")
        .arg(&args.destination)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            eprintln!("error: could not start the SSH forward: {error}");
            return 1;
        }
    };
    // Wait for the forward to answer before printing the URL. A bare TCP connect is not
    // proof: another process may hold the port (ssh then exits on ExitOnForwardFailure).
    // Require ssh to still be running and the port to answer `/health` as this app's
    // daemon; the token is never sent during this check.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        if let Ok(Some(status)) = forward.try_wait() {
            eprintln!("error: the SSH forward exited ({status})");
            return 1;
        }
        if forward_answers_as_daemon(local_port).await {
            if let Ok(Some(status)) = forward.try_wait() {
                eprintln!("error: the SSH forward exited ({status})");
                return 1;
            }
            break;
        }
        if std::time::Instant::now() > deadline {
            eprintln!(
                "error: the SSH forward did not come up on 127.0.0.1:{local_port} (nothing there answers as the {} daemon)",
                crate::host_identity::app()
            );
            stop_forward(&mut forward).await;
            return 1;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let url = crate::api_web::web_url(&format!("http://127.0.0.1:{local_port}"), &remote.token);
    println!("{url}");
    eprintln!(
        "{} on {} is forwarded to 127.0.0.1:{local_port}. The URL is a credential. Ctrl-C ends the forward.",
        rightkit_agent_config::app_name(),
        args.destination
    );
    tracing::info!(
        event = "remote_forward_started",
        local_port,
        "remote over ssh"
    );
    if args.open && !crate::open_url::open_url(&url).is_ok_and(|status| status.success()) {
        eprintln!("could not open a browser; open the URL above yourself");
    }
    // 3. Hold the forward until Ctrl-C or until SSH drops.
    tokio::select! {
        status = forward.wait() => {
            match status {
                Ok(status) if status.success() => 0,
                Ok(status) => {
                    eprintln!("the SSH forward ended ({status})");
                    1
                }
                Err(error) => {
                    eprintln!("the SSH forward failed: {error}");
                    1
                }
            }
        }
        _ = tokio::signal::ctrl_c() => {
            stop_forward(&mut forward).await;
            0
        }
    }
}

#[cfg(test)]
mod forward_readiness_tests {
    use super::{forward_answers_as_daemon, health_reply_is_daemon};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn only_a_daemon_health_reply_counts() {
        let service = crate::host_identity::prefixed("daemon");
        let daemon = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{{\"ok\":true,\"service\":\"{service}\",\"version\":\"1\",\"contractVersion\":1,\"protocolVersion\":1}}"
        );
        assert!(health_reply_is_daemon(daemon.as_bytes()));
        let other = b"HTTP/1.0 200 OK\r\nContent-type: text/html\r\n\r\n<html>listing</html>";
        assert!(!health_reply_is_daemon(other));
        let not_found =
            format!("HTTP/1.1 404 Not Found\r\n\r\n{{\"ok\":true,\"service\":\"{service}\"}}");
        assert!(!health_reply_is_daemon(not_found.as_bytes()));
        assert!(!health_reply_is_daemon(b""));
    }

    #[tokio::test]
    async fn a_squatter_on_the_port_is_not_taken_for_the_forward() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = [0u8; 1024];
            let read = socket.read(&mut request).await.expect("read");
            let request = String::from_utf8_lossy(&request[..read]).to_string();
            socket
                .write_all(b"HTTP/1.0 200 OK\r\n\r\n<html>not the daemon</html>")
                .await
                .expect("write");
            request
        });
        assert!(!forward_answers_as_daemon(port).await);
        let request = server.await.expect("server");
        assert!(request.starts_with("GET /health "), "{request}");
        assert!(!request.contains("token"), "{request}");
    }
}

#[cfg(test)]
mod argument_tests {
    use super::{refused_argument, RemoteArgs};

    fn args(destination: &str, options: &[&str]) -> RemoteArgs {
        RemoteArgs {
            destination: destination.to_string(),
            local_port: None,
            ssh_port: None,
            ssh_options: options.iter().map(|option| option.to_string()).collect(),
            remote_bin: "app".to_string(),
            open: false,
        }
    }

    #[test]
    fn a_destination_is_a_host_and_an_option_is_one_directive() {
        assert!(refused_argument(&args("user@host", &["IdentityFile=~/.ssh/work"])).is_none());
        assert!(refused_argument(&args("-oProxyCommand=x", &[])).is_some());
        assert!(refused_argument(&args("", &[])).is_some());
        assert!(refused_argument(&args("host name", &[])).is_some());
        assert!(refused_argument(&args("host", &["A=b\nProxyCommand=x"])).is_some());
        assert!(refused_argument(&args("host", &["-vvv"])).is_some());
        assert!(refused_argument(&args("host", &["  "])).is_some());
    }
}
