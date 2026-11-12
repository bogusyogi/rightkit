#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NofileDiagnostic {
    pub(crate) supported: bool,
    pub(crate) current_soft: Option<u64>,
    pub(crate) hard: Option<u64>,
    pub(crate) target_soft: Option<u64>,
    pub(crate) applied_soft: Option<u64>,
    pub(crate) detail: String,
}

pub(crate) fn target_nofile_soft(current_soft: u64, hard: u64) -> u64 {
    current_soft.max(TARGET_NOFILE_SOFT.min(hard))
}

#[cfg(unix)]
fn raise_nofile_with<Read, Set>(mut read: Read, mut set: Set) -> NofileDiagnostic
where
    Read: FnMut() -> Result<(u64, u64), String>,
    Set: FnMut(u64, u64) -> Result<(), String>,
{
    let (current_soft, hard) = match read() {
        Ok(limits) => limits,
        Err(error) => return nofile_read_failure(error),
    };
    let target = target_nofile_soft(current_soft, hard);
    if target == current_soft {
        return nofile_result(
            current_soft,
            hard,
            target,
            current_soft,
            "RLIMIT_NOFILE already meets the bounded target",
        );
    }
    match set(target, hard) {
        Ok(()) => nofile_result(
            current_soft,
            hard,
            target,
            target,
            "raised RLIMIT_NOFILE soft limit",
        ),
        Err(error) => nofile_result(
            current_soft,
            hard,
            target,
            current_soft,
            &format!("could not raise RLIMIT_NOFILE: {error}"),
        ),
    }
}

fn nofile_read_failure(error: String) -> NofileDiagnostic {
    NofileDiagnostic {
        supported: cfg!(unix),
        current_soft: None,
        hard: None,
        target_soft: None,
        applied_soft: None,
        detail: format!("failed to read RLIMIT_NOFILE: {error}"),
    }
}

fn nofile_result(
    current: u64,
    hard: u64,
    target: u64,
    applied: u64,
    detail: &str,
) -> NofileDiagnostic {
    NofileDiagnostic {
        supported: true,
        current_soft: Some(current),
        hard: Some(hard),
        target_soft: Some(target),
        applied_soft: Some(applied),
        detail: detail.to_string(),
    }
}

#[cfg(unix)]
pub(crate) fn raise_nofile_soft_limit() -> NofileDiagnostic {
    raise_nofile_with(
        || {
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) };
            (rc == 0)
                .then_some((limit.rlim_cur, limit.rlim_max))
                .ok_or_else(|| std::io::Error::last_os_error().to_string())
        },
        |soft, hard| {
            let limit = libc::rlimit {
                rlim_cur: soft as libc::rlim_t,
                rlim_max: hard as libc::rlim_t,
            };
            let rc = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) };
            (rc == 0)
                .then_some(())
                .ok_or_else(|| std::io::Error::last_os_error().to_string())
        },
    )
}

#[cfg(not(unix))]
pub(crate) fn raise_nofile_soft_limit() -> NofileDiagnostic {
    NofileDiagnostic {
        supported: false,
        current_soft: None,
        hard: None,
        target_soft: None,
        applied_soft: None,
        detail: "RLIMIT_NOFILE is unsupported on this platform; no limit was changed".to_string(),
    }
}

#[cfg_attr(not(unix), allow(unused_variables))]
pub(crate) fn file_descriptor_error(
    error: &std::io::Error,
    diagnostic: &NofileDiagnostic,
) -> String {
    #[cfg(unix)]
    if matches!(error.raw_os_error(), Some(libc::EMFILE | libc::ENFILE)) {
        return format!("file descriptor exhaustion ({error}); current soft limit={}; hard limit={}. Close idle sessions/processes or raise the host nofile limit, then restart RightKit.", diagnostic.applied_soft.or(diagnostic.current_soft).map(|v| v.to_string()).unwrap_or_else(|| "unknown".into()), diagnostic.hard.map(|v| v.to_string()).unwrap_or_else(|| "unknown".into()));
    }
    error.to_string()
}

async fn cli_diagnostics(command: DiagnosticsCommand) -> i32 {
    match command {
        DiagnosticsCommand::Watch {
            once,
            interval_ms,
            json,
        } => daemon_diagnostics_watch(once, interval_ms, json).await,
        DiagnosticsCommand::Bundle { output } => daemon_diagnostics_bundle(&output).await,
    }
}

async fn daemon_diagnostics_watch(once: bool, interval_ms: u64, json: bool) -> i32 {
    let (base_url, token) = match crate::cli_presence::resolve_daemon_or_autostart().await {
        Some(connection) => connection,
        None => {
            eprintln!(
                "No running daemon found. Start one with `{app} serve`.",
                app = crate::host_identity::app()
            );
            return 1;
        }
    };
    let client = match rightkit_agent_daemon::http_client::daemon_client(&base_url) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("diagnostics watch: {error}");
            return 1;
        }
    };
    loop {
        let sample = match diagnostics_watch_sample(&client, &base_url, &token).await {
            Ok(sample) => sample,
            Err(error) => {
                eprintln!("diagnostics watch: {error}");
                return 1;
            }
        };
        if json {
            println!("{}", serde_json::to_string(&sample).unwrap_or_default());
        } else {
            println!(
                "sessions={} clients={} inference_active={} inference_queued={} retained_bytes={} sse_clients={}",
                sample["sessions"], sample["clients"], sample["inference"]["active"], sample["inference"]["queued"],
                sample["retainedBytes"], sample["sseClients"],
            );
        }
        if once {
            return 0;
        }
        tokio::time::sleep(std::time::Duration::from_millis(interval_ms)).await;
    }
}

async fn diagnostics_watch_sample(
    client: &rightkit_http::AsyncClient,
    base_url: &str,
    token: &str,
) -> Result<serde_json::Value, String> {
    async fn get(
        client: &rightkit_http::AsyncClient,
        base_url: &str,
        token: &str,
        path: &str,
    ) -> Result<serde_json::Value, String> {
        let response = client
            .send_any(&rightkit_http::Request::get(format!("{base_url}{path}")).bearer(token))
            .await
            .map_err(|error| format!("GET {path}: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(response.status) {
            return Err(format!("GET {path}: HTTP {}", response.status));
        }
        response
            .json()
            .map_err(|error| format!("decode {path}: {error}"))
    }
    let (health, sessions, clients, inference) = tokio::join!(
        get(client, base_url, token, "/health"),
        get(client, base_url, token, "/sessions"),
        get(client, base_url, token, "/clients"),
        get(client, base_url, token, "/inference/load"),
    );
    let health = health?;
    let sessions = sessions?.as_array().map(Vec::len).unwrap_or(0);
    let clients = clients?.as_array().map(Vec::len).unwrap_or(0);
    let inference = inference.unwrap_or_else(|error| serde_json::json!({ "unavailable": error }));
    Ok(serde_json::json!({
        "schema_version": crate::host_identity::schema("diagnostics.watch.v1"),
        "timestamp": now_iso(),
        "sessions": sessions,
        "clients": clients,
        "inference": inference,
        "retainedBytes": health.get("retainedBytes").cloned().unwrap_or(serde_json::Value::Null),
        "sseClients": health.get("sseClients").cloned().unwrap_or(serde_json::Value::Null),
        "health": health,
    }))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DaemonDiagnosticsBundleSummary {
    bundle_id: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DaemonDiagnosticsBundleDownload {
    content_base64: String,
}

async fn daemon_diagnostics_bundle(output: &str) -> i32 {
    let (base_url, token) = match crate::cli_presence::resolve_daemon_or_autostart().await {
        Some(p) => p,
        None => {
            eprintln!(
                "No running daemon found. Start one with `{app} serve`.",
                app = crate::host_identity::app()
            );
            return 1;
        }
    };
    let resp = match rightkit_agent_daemon::http_client::daemon_call(
        rightkit_http::Method::Post,
        &format!("{base_url}/diagnostics/bundle"),
        &token,
        None,
    )
    .await
    {
        Ok(resp) => resp,
        Err(e) => {
            eprintln!("Failed to contact daemon: {e}");
            return 1;
        }
    };
    let status = resp.status;
    let body = resp.text();
    if !rightkit_agent_daemon::http_client::is_success(status) {
        eprintln!("Daemon returned {status} for diagnostics bundle: {body}");
        return 1;
    }
    let summary: DaemonDiagnosticsBundleSummary = match serde_json::from_str(&body) {
        Ok(summary) => summary,
        Err(e) => {
            eprintln!("Daemon returned an invalid diagnostics bundle summary: {e}");
            return 1;
        }
    };

    let resp = match rightkit_agent_daemon::http_client::daemon_get(
        &format!("{base_url}/diagnostics/bundles/{}", summary.bundle_id),
        &token,
    )
    .await
    {
        Ok(resp) => resp,
        Err(e) => {
            eprintln!("Failed to download diagnostics bundle: {e}");
            return 1;
        }
    };
    let status = resp.status;
    let body = resp.text();
    if !rightkit_agent_daemon::http_client::is_success(status) {
        eprintln!("Daemon returned {status} for diagnostics bundle download: {body}");
        return 1;
    }
    let download: DaemonDiagnosticsBundleDownload = match serde_json::from_str(&body) {
        Ok(download) => download,
        Err(e) => {
            eprintln!("Daemon returned an invalid diagnostics bundle download: {e}");
            return 1;
        }
    };
    let bytes = match rightkit_crypto::base64_decode(&download.content_base64) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("Daemon returned undecodable diagnostics bundle bytes: {e}");
            return 1;
        }
    };

    let output_path = std::path::Path::new(output);
    if let Some(parent) = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!(
                "Failed to create output directory {}: {e}",
                parent.display()
            );
            return 1;
        }
    }
    if let Err(e) = std::fs::write(output_path, &bytes) {
        eprintln!(
            "Failed to write diagnostics bundle {}: {e}",
            output_path.display()
        );
        return 1;
    }

    println!(
        "Diagnostics bundle written to {} ({} bytes)\nBundle ID: {}",
        output_path.display(),
        bytes.len(),
        summary.bundle_id
    );
    0
}
