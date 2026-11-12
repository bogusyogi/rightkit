async fn daemon_logs(session_id: &str) -> i32 {
    let (base_url, token) = match crate::cli_presence::resolve_daemon_or_autostart().await {
        Some(p) => p,
        None => {
            eprintln!("No running daemon found. Start one with `{app} serve`.", app = crate::host_identity::app());
            return 1;
        }
    };

    // CR-27: print durable history first, through bounded backwards pages —
    // never an unbounded query. The conduit ring can be empty after a daemon
    // restart even when the SQLite ledger holds the full transcript, so the
    // ledger is the canonical catch-up source and the SSE tail below starts
    // after the highest seq printed here (no duplicate delivery).
    let mut last_seq = 0_i64;
    match fetch_full_scrollback(&base_url, &token, session_id, SCROLLBACK_PAGE_LIMIT).await {
        Ok(events) => {
            for event in &events {
                println!("{event}");
                if let Some(seq) = event.get("seq").and_then(|s| s.as_i64()) {
                    last_seq = last_seq.max(seq);
                }
            }
        }
        Err(ScrollbackError::SessionNotFound) => {
            eprintln!("No such session: {session_id}");
            return 1;
        }
        // R11 invariant: a storage fault must surface as an error, never
        // flatten into "no more history".
        Err(ScrollbackError::HistoryCatchUpFailed(detail)) => {
            eprintln!("History catch-up failed for session {session_id}: {detail}");
            return 1;
        }
        Err(ScrollbackError::Transport(detail)) => {
            eprintln!("Failed to contact daemon: {detail}");
            return 1;
        }
    }

    let client = match rightkit_http::http_client::daemon_client(&base_url) {
        Ok(client) => client,
        Err(e) => {
            eprintln!("Failed to contact daemon: {e}");
            return 1;
        }
    };
    stream_events_with_reconnect(&client, &base_url, &token, session_id, last_seq, true).await
}

// ── ra-009: bounded reconnect/backoff for the live SSE tail ────────────────
//
// Both `{app} logs` (`daemon_logs`) and `{app} attach`
// (`daemon_logs_inner`) used to stop the moment the SSE connection dropped
// or a chunk read failed — a transient network blip silently truncated
// monitoring instead of resuming. `stream_events_with_reconnect` is the
// shared tail loop for both: it remembers the highest `seq` it has printed
// (the durable cursor), and on a transport error it reconnects at
// `after=<cursor>` instead of giving up, so no event already printed is
// re-delivered (no duplicate) and no event after the cursor is skipped (no
// gap) — the daemon's `events?after=` contract is exclusive-of-`after`,
// ascending, the same contract `daemon_logs`'s pre-existing scrollback catch
// -up already relies on. Reconnect attempts are bounded and back off
// exponentially (capped) so a genuinely dead daemon fails loudly instead of
// spinning forever; exhausting the bound returns failure (1), never a quiet
// success.
const STREAM_RECONNECT_MAX_ATTEMPTS: u32 = 6;
const STREAM_RECONNECT_BASE_DELAY_MS: u64 = 200;
const STREAM_RECONNECT_MAX_DELAY_MS: u64 = 5_000;

fn stream_reconnect_backoff(attempt: u32) -> std::time::Duration {
    let shift = attempt.saturating_sub(1).min(16);
    let delay_ms = STREAM_RECONNECT_BASE_DELAY_MS
        .saturating_mul(1u64 << shift)
        .min(STREAM_RECONNECT_MAX_DELAY_MS);
    std::time::Duration::from_millis(delay_ms)
}

/// Tail `GET /sessions/{id}/events?after={cursor}` from `start_after_seq`,
/// printing each chunk to stdout, and transparently reconnect at the
/// latest-seen cursor on a transport error instead of failing immediately.
/// A clean end-of-stream (`None`) is treated as the daemon intentionally
/// closing the tail (session finished) and returns success; only read/send
/// errors trigger reconnect. `print_status` controls whether reconnect
/// attempts are reported on stderr (quiet for the `attach` background
/// tailer, visible for the interactive `logs` tail).
async fn stream_events_with_reconnect(
    client: &rightkit_http::AsyncClient,
    base_url: &str,
    token: &str,
    session_id: &str,
    start_after_seq: i64,
    print_status: bool,
) -> i32 {
    let encoded_session_id = url_encode_component(session_id);
    let mut cursor = start_after_seq;
    let mut attempt = 0u32;
    let mut line_buf = String::new();

    loop {
        let url = format!("{base_url}/sessions/{encoded_session_id}/events?after={cursor}");
        let resp = match client
            .stream_any(&rightkit_http::Request::get(url).bearer(token))
            .await
        {
            Ok(r) if rightkit_http::http_client::is_success(r.status) => r,
            Ok(r) => {
                eprintln!("Daemon returned {} for session {session_id}", r.status);
                return 1;
            }
            Err(e) => {
                if !reconnect_or_fail(&mut attempt, print_status, &format!("connect: {e}")).await {
                    return 1;
                }
                continue;
            }
        };

        let mut body = resp.into_stream();
        loop {
            match futures_util::StreamExt::next(&mut body).await {
                Some(Ok(bytes)) => {
                    attempt = 0; // any live data resets the reconnect budget
                    let text = String::from_utf8_lossy(&bytes);
                    print!("{text}");
                    let _ = std::io::stdout().flush();
                    line_buf.push_str(&text);
                    if let Some(last_nl) = line_buf.rfind('\n') {
                        for line in line_buf[..last_nl].lines() {
                            if let Some(event) = parse_sse_json_event(line) {
                                if let Some(seq) = event.get("seq").and_then(|s| s.as_i64()) {
                                    cursor = cursor.max(seq);
                                }
                            }
                        }
                        line_buf.drain(..=last_nl);
                    }
                }
                None => return 0,
                Some(Err(e)) => {
                    if !reconnect_or_fail(&mut attempt, print_status, &format!("read: {e}")).await {
                        return 1;
                    }
                    break;
                }
            }
        }
    }
}

/// Bump the bounded reconnect budget and sleep the backoff. Returns `false`
/// (recovery exhausted) once `attempt` exceeds
/// `STREAM_RECONNECT_MAX_ATTEMPTS`, so the caller can surface a real failure
/// instead of looping forever against a dead daemon.
async fn reconnect_or_fail(attempt: &mut u32, print_status: bool, reason: &str) -> bool {
    *attempt += 1;
    if *attempt > STREAM_RECONNECT_MAX_ATTEMPTS {
        eprintln!(
            "Stream error: {reason} (giving up after {STREAM_RECONNECT_MAX_ATTEMPTS} reconnect attempts)"
        );
        return false;
    }
    let delay = stream_reconnect_backoff(*attempt);
    if print_status {
        eprintln!(
            "Stream error: {reason} — reconnecting (attempt {attempt}/{STREAM_RECONNECT_MAX_ATTEMPTS}) in {}ms",
            delay.as_millis()
        );
    }
    tokio::time::sleep(delay).await;
    true
}

/// Headless multi-agent conductor run: create a session bound to the workspace,
/// kick off the conductor (`/orchestrate`), then stream its progress events
/// (plan → task graph → verdict) until the stream ends. Reuses the same daemon
/// client + SSE-tail path as `{app} logs`.
fn conduct_create_session_body(
    session_id: &str,
    workspace_root: &str,
    safe_mode: bool,
    model: Option<&str>,
    auto_approve: bool,
) -> serde_json::Value {
    // Matches daemon_session_create_body's precedence: an explicit
    // auto-approve maps to accept-edits; absence stays None so the workspace
    // config decides (safe-mode sessions still resolve ask server-side).
    serde_json::json!({
        "id": session_id,
        "workspaceRoot": workspace_root,
        "safeMode": safe_mode,
        "model": model,
        "permissionMode": if auto_approve { Some("accept-edits") } else { None },
    })
}

async fn daemon_conduct(
    goal: &str,
    dry_run: bool,
    safe_mode: bool,
    workspace: Option<&str>,
    model: Option<&str>,
    auto_approve: bool,
) -> i32 {
    let (base_url, token) = match crate::cli_presence::resolve_daemon_or_autostart().await {
        Some(p) => p,
        None => {
            eprintln!("No running daemon found. Start one with `{app} serve`.", app = crate::host_identity::app());
            return 1;
        }
    };
    let workspace_root = workspace
        .map(str::to_string)
        .unwrap_or_else(default_cli_workspace);
    let session_id = format!(
        "conduct-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    );

    // 1. Create a session bound to the workspace.
    match rightkit_http::http_client::daemon_call(
        rightkit_http::Method::Post,
        &format!("{base_url}/sessions"),
        &token,
        Some(&conduct_create_session_body(
            &session_id,
            &workspace_root,
            safe_mode,
            model,
            auto_approve,
        )),
    )
    .await
    {
        Ok(r) if rightkit_http::http_client::is_success(r.status) => {}
        Ok(r) => {
            eprintln!("Failed to create session: daemon returned {}", r.status);
            return 1;
        }
        Err(e) => {
            eprintln!("Failed to contact daemon: {e}");
            return 1;
        }
    }

    // 2. Kick off the conductor (returns immediately; progress streams via SSE).
    let encoded_session_id = url_encode_component(&session_id);
    match rightkit_http::http_client::daemon_call(
        rightkit_http::Method::Post,
        &format!("{base_url}/sessions/{encoded_session_id}/orchestrate"),
        &token,
        Some(&serde_json::json!({ "goal": goal, "dryRun": dry_run })),
    )
    .await
    {
        Ok(r) if rightkit_http::http_client::is_success(r.status) => {
            eprintln!(
                "[conduct] session {session_id}: {} started",
                if dry_run { "dry-run plan" } else { "run" }
            );
        }
        Ok(r) if r.status == 402 => {
            eprintln!(
                "[conduct] the multi-agent conductor (BREWS) is a Pro feature; this daemon is not licensed for it."
            );
            return 1;
        }
        Ok(r) => {
            eprintln!("[conduct] orchestrate failed: daemon returned {}", r.status);
            return 1;
        }
        Err(e) => {
            eprintln!("[conduct] failed to contact daemon: {e}");
            return 1;
        }
    }

    // 3. Stream the run's events until the conductor emits a terminal verdict.
    daemon_logs_until_terminal(&base_url, &token, &session_id).await
}

async fn daemon_attach(session_id: &str) -> i32 {
    // DL-014: the daemon-reachability step now flows through the shared
    // `LifecycleAuthority` (`Start { Background }`) instead of calling the
    // autostart coordinator directly; see
    // `daemon::lifecycle_backend::resolve_daemon_endpoint_via_authority`.
    let (base_url, token) =
        match crate::daemon::lifecycle_backend::resolve_daemon_endpoint_via_authority().await {
            Some(p) => p,
            None => {
                eprintln!("No running daemon found. Start one with `{app} serve`.", app = crate::host_identity::app());
                return 1;
            }
        };

    let encoded_session_id = url_encode_component(session_id);

    // Resume the session first (rehydrate if dormant).
    match rightkit_http::http_client::daemon_call(
        rightkit_http::Method::Post,
        &format!("{base_url}/sessions/{encoded_session_id}/resume"),
        &token,
        None,
    )
    .await
    {
        Ok(resp) if rightkit_http::http_client::is_success(resp.status) => {
            eprintln!("[attach] session {session_id} resumed");
        }
        Ok(resp) => {
            eprintln!(
                "[attach] resume returned {} — session may already be live",
                resp.status
            );
        }
        Err(e) => {
            eprintln!("Failed to contact daemon: {e}");
            return 1;
        }
    }

    // Spawn a task that tails the SSE event stream and prints to stdout.
    let base_url_clone = base_url.clone();
    let token_clone = token.clone();
    let session_id_owned = session_id.to_string();
    let log_handle = tokio::spawn(async move {
        let _ = daemon_logs_inner(&base_url_clone, &token_clone, &session_id_owned).await;
    });

    // Read stdin lines and send them as messages.
    let stdin = std::io::stdin();
    let mut line = String::new();
    loop {
        line.clear();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {
                let msg = line.trim();
                if msg.is_empty() {
                    continue;
                }
                let body = serde_json::json!({ "text": msg });
                match rightkit_http::http_client::daemon_call(
                    rightkit_http::Method::Post,
                    &format!("{base_url}/sessions/{encoded_session_id}/messages"),
                    &token,
                    Some(&body),
                )
                .await
                {
                    Ok(resp) if rightkit_http::http_client::is_success(resp.status) => {}
                    Ok(resp) => eprintln!("[attach] send returned {}", resp.status),
                    Err(e) => eprintln!("[attach] send failed: {e}"),
                }
            }
            Err(_) => break,
        }
    }

    log_handle.abort();
    0
}

async fn daemon_logs_inner(base_url: &str, token: &str, session_id: &str) -> i32 {
    let client = match rightkit_http::http_client::daemon_client(base_url) {
        Ok(client) => client,
        Err(e) => {
            eprintln!("Failed to contact daemon: {e}");
            return 1;
        }
    };
    // `attach` backgrounds this tailer alongside interactive stdin, so
    // reconnect status stays quiet (print_status=false) — but it still
    // resumes from the last-seen cursor with bounded backoff instead of
    // silently going dark on the first transient disconnect (ra-009).
    stream_events_with_reconnect(&client, base_url, token, session_id, 0, false).await
}

fn parse_sse_json_event(line: &str) -> Option<serde_json::Value> {
    let payload = line.strip_prefix("data:")?.trim();
    if payload.is_empty() || payload == "[DONE]" {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(payload).ok()
}

fn terminal_sse_exit_code(line: &str) -> Option<i32> {
    let event = parse_sse_json_event(line)?;
    match event.get("$kind").and_then(|value| value.as_str()) {
        Some("assistantMessageCompleted") => {
            let content = event
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(|content| content.as_str())
                .unwrap_or("");
            // ponytail: until the daemon emits a machine status for conductor stops,
            // keep the CLI honest for the existing terminal error summaries.
            if content.starts_with("Conductor run exceeded")
                || content.starts_with("Pre-gate checks failed")
                || content.starts_with("Plan rejected")
                || content.starts_with("PartiallyComplete (")
                || content.starts_with("Rejected (")
            {
                Some(1)
            } else {
                Some(0)
            }
        }
        Some("runFailed") | Some("runInterrupted") => Some(1),
        _ => None,
    }
}

// ── R11 CR-27: bounded scrollback client ───────────────────────────────────
//
// The daemon now serves `GET /sessions/{id}/events?before={seq}&limit={n}`
// as a bounded page backwards through durable history. No code constructed
// that request before this lane — this is the missing consumer.
//
// Every request carries a `limit`. Paging is cursor-based: the caller holds
// `next_before` (the lowest seq it still needs). An empty page means the
// start of history, not a storage error — storage errors surface as a
// distinct `Err` via `history_catch_up_failed` (sse.rs: history_catch_up_error).

/// Default page size for scrollback. Bounded so a long transcript never loads
/// unbounded into memory.
const SCROLLBACK_PAGE_LIMIT: usize = 100;

/// Result of one bounded history fetch.
#[derive(Debug, Clone)]
pub struct ScrollbackPage {
    pub events: Vec<serde_json::Value>,
    /// `None` when the start of history has been reached (empty page).
    /// `Some(seq)` is the cursor for the next backwards fetch: the smallest
    /// seq in this page. A caller that wants the previous page sends
    /// `before = smallest_seq` with the same limit.
    pub next_before: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScrollbackError {
    HistoryCatchUpFailed(String),
    Transport(String),
    SessionNotFound,
}

/// Fetch one bounded page backwards from `before_seq`.
///
/// Every request carries `limit`. A `before` without `limit` is never sent —
/// that would be the unbounded path R11 forbids. Storage failures are
/// surfaced as `ScrollbackError::HistoryCatchUpFailed`, never as an empty
/// page, so callers can distinguish "no more history" from "we don't know
/// what's in range" (CR-05).
pub async fn fetch_scrollback_page(
    client: &rightkit_http::AsyncClient,
    base_url: &str,
    token: &str,
    session_id: &str,
    before_seq: i64,
    limit: usize,
) -> Result<ScrollbackPage, ScrollbackError> {
    let encoded = url_encode_component(session_id);
    let url = format!("{base_url}/sessions/{encoded}/events?before={before_seq}&limit={limit}");
    let resp = client
        .send_any(&rightkit_http::Request::get(url).bearer(token))
        .await
        .map_err(|e| ScrollbackError::Transport(e.to_string()))?;
    let status = resp.status;
    if status == 404 {
        return Err(ScrollbackError::SessionNotFound);
    }
    if status == 500 {
        // The daemon's `history_catch_up_error` returns
        // `{"error":"history_catch_up_failed",...}` — surface any 500 as
        // that distinct fault rather than an empty page.
        return Err(ScrollbackError::HistoryCatchUpFailed(resp.text()));
    }
    if !rightkit_http::http_client::is_success(status) {
        return Err(ScrollbackError::Transport(format!(
            "daemon returned {}: {}",
            status,
            resp.text()
        )));
    }
    let events: Vec<serde_json::Value> = resp
        .json()
        .map_err(|e| ScrollbackError::Transport(e.to_string()))?;
    if events.is_empty() {
        return Ok(ScrollbackPage {
            events: Vec::new(),
            next_before: None,
        });
    }
    let smallest = events
        .iter()
        .filter_map(|v| v.get("seq").and_then(|s| s.as_i64()))
        .min()
        .unwrap_or(before_seq);
    Ok(ScrollbackPage {
        events,
        next_before: Some(smallest),
    })
}

/// Page backwards through an entire session's durable history, reassembling
/// the transcript in seq-ascending order. Real caller: `{app} logs`
/// (`daemon_logs` below), which prints this history before tailing the live
/// SSE stream from the highest seq printed.
///
/// Paging starts at `before = i64::MAX` so the first page is the newest
/// events; each later request uses the previous page's smallest seq as its
/// exclusive cursor. `events_before` is exclusive of `before` and returns
/// each page seq-ascending, so concatenating pages newest-page-first yields
/// every event exactly once — no duplicates, no gaps — by construction, with
/// no sorting or dedup needed to paper over boundary defects. Each request
/// is `limit`-bounded. An empty page or a cursor at seq 1 terminates
/// cleanly; a cursor that fails to advance is surfaced as an error rather
/// than spun on forever.
pub async fn fetch_full_scrollback(
    base_url: &str,
    token: &str,
    session_id: &str,
    limit: usize,
) -> Result<Vec<serde_json::Value>, ScrollbackError> {
    let client =
        rightkit_http::http_client::daemon_client(base_url).map_err(ScrollbackError::Transport)?;
    let mut pages: Vec<Vec<serde_json::Value>> = Vec::new();
    let mut before = i64::MAX;
    loop {
        let page =
            fetch_scrollback_page(&client, base_url, token, session_id, before, limit).await?;
        if page.events.is_empty() {
            break;
        }
        let next = page
            .next_before
            .expect("non-empty page always carries a cursor");
        pages.push(page.events);
        if next <= 1 {
            break;
        }
        if next >= before {
            // The daemon returned events but named no smaller cursor: paging
            // cannot make progress. Error out instead of looping.
            return Err(ScrollbackError::Transport(
                "scrollback cursor did not advance".to_string(),
            ));
        }
        before = next;
    }
    let mut all = Vec::new();
    for page in pages.into_iter().rev() {
        all.extend(page);
    }
    Ok(all)
}
