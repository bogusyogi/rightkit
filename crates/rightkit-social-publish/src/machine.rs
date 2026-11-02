//! The phase machine. One call to [`step_once`] advances a job by exactly one phase or one poll, in
//! three stages, so it is restartable at every boundary:
//!
//! 1. Under a write transaction: load the job, refuse a finished job, turn a leftover in-flight marker
//!    into `outcome_unknown`, honour `next_attempt_at`, and (for a phase whose request creates something)
//!    re-check the approval and the account pause, then store the in-flight marker. Commit.
//! 2. With no transaction open: send exactly one request. Retries live here, not in the HTTP client.
//! 3. Under a second write transaction: store the outcome and the remote id and advance the phase in
//!    the same UPDATE. Commit.
//!
//! A crash between 2 and 3 leaves the in-flight marker, and the next step reports `outcome_unknown`
//! rather than sending the create again. `publish.reconcile` then looks for the post.
//!
//! Phases: container platforms `create_container -> poll_status -> publish -> read_permalink -> done`;
//! simple platforms `create -> read_permalink -> done`.
//!
//! Mastodon's `create` makes two requests in one step: the instance limit read (a read: its failures are
//! retried, never `outcome_unknown`) and then the create. `PublishStepOutput.calls` reports both.
//!
//! Images (Bluesky, Mastodon, X). A job with media has one extra step at the start of `create`: the UPLOAD
//! step (`media_upload`). It fetches each file from its media URL with a plain GET (no token), checks its size
//! and magic bytes, uploads it to the platform and stores the platform's references in `remote.container_id`
//! (a JSON array), then ends with outcome `advanced` and the job still at `create`. The next step posts with
//! those references. An upload only stages a file, so its failures are handled as a read's are (retried, never
//! `outcome_unknown`); a refusal of the media itself fails the job (`media_too_large`, `media_invalid`).
//! `PublishStepOutput.calls` counts the fetch and the upload of each file. See `media.rs`.
//!
//! LinkedIn's `create` is one request with no idempotency key (the Posts API has none), so it is never
//! repeated: an ambiguous answer is `outcome_unknown` and `publish.reconcile` cannot create (see
//! `reconcile_linkedin`). A 4xx saying the `LinkedIn-Version` is unsupported fails the job
//! `api_version_unsupported` (a published post whose read-back hits it stays `published`).
//!
//! Threads runs `create_container -> publish -> read_permalink` (no `poll_status`: the text container is
//! published right after it is created). Neither write carries an idempotency key. A lost answer at
//! `create_container` only orphans an unpublished container, so reconcile returns it to `queued` once and
//! the retry makes a new container. A lost answer at `publish` is `outcome_unknown` and reconcile never
//! publishes (see `reconcile_threads`). Error code 190 in a 4xx body is `token_expired`, and codes 4, 17,
//! 32 and 613 are a rate limit (see `threads::normalize`).
//!
//! Facebook's `create` is one `POST /v<version>/<page>/feed` with no idempotency key, so it is never repeated:
//! an ambiguous answer is `outcome_unknown` and `publish.reconcile` cannot create (see `reconcile_facebook`).
//! Error code 190 in a 4xx body is `token_expired`, codes 4, 17, 32 and 613 are a rate limit (see
//! `facebook::normalize`), and codes 10 and 200 fail the job `permission_denied` without a retry.
//!
//! X's `create` is one `POST /2/tweets` with no idempotency key, so it is never repeated: an ambiguous answer is
//! `outcome_unknown` and `publish.reconcile` cannot create (see `reconcile_x`). A 403 is final: a body saying
//! duplicate content fails the job `duplicate_content`, any other 403 fails it `permission_denied`. A 429 waits
//! until `x-rate-limit-reset` (epoch seconds) when it is present and sane (see `http::parse_epoch_reset`),
//! else the default backoff.
//!
//! Instagram runs `create_container -> poll_status -> publish -> read_permalink`. Neither write carries an
//! idempotency key. A lost answer at `create_container` only orphans an unpublished container, so reconcile
//! returns it to `queued` and the retry makes a new container. A container that reports `EXPIRED` while polled
//! is replaced by a new one (bounded by `instagram::MAX_RESTARTS`); `ERROR` fails the job `container_failed`.
//! A lost answer at `publish` is `outcome_unknown`; reconcile never publishes blind, it READS the container
//! (see `reconcile_instagram`): `FINISHED` proves nothing was published and queues one repeat, `PUBLISHED`
//! proves a post exists whose id the container read does not return, so the job stays for a person. Error code
//! 190 in a 4xx body is `token_expired`, codes 4, 17, 32 and 613 are a rate limit and codes 10 and 200 fail the
//! job `permission_denied` (see `instagram::normalize`).
//!
//! | Platform answer | Write phase (create_container, create, publish) | Read phase (poll_status, read_permalink) |
//! |---|---|---|
//! | 2xx | store id, advance | advance (a poll that is not finished stays) |
//! | 429 | `waiting` until Retry-After | `waiting` until Retry-After |
//! | 401 | `failed` `token_expired` | `failed` (a published post stays `published`) |
//! | 5xx | `outcome_unknown` | `waiting`, backoff, bounded |
//! | connection refused | `waiting`, backoff, bounded | same |
//! | dropped after send, timeout | `outcome_unknown` | `waiting`, backoff, bounded |
//! | other 4xx | `failed` `platform_rejected` | `published` without a permalink |
use rightkit_http::{Client, Method};
use serde_json::Value;

use crate::calendar_time as time;
use crate::http::{self, Reply, RetryAfter, MAX_RETRY_AFTER_SECS};
use crate::media;
use crate::platform::{self, Spec};
use crate::platforms::{
    bluesky, facebook, instagram, linkedin, mastodon, pinterest, threads, tiktok, x, youtube,
};
use crate::protocol::{PublishJob, PublishStepOutput, Scope};
use crate::quota::{self, PublishQuotaStore};
use crate::util::not_found;
use crate::Fail;

/// A calendar entry as the approval gate reads it.
#[derive(Debug, Clone)]
pub struct EntryGate {
    pub revision: i64,
    pub approval_state: String,
    pub approved_entry_revision: Option<i64>,
}

/// What the host app provides on top of the job and pacing stores: the calendar entry the job was
/// approved from, the account pause flag and the posts other jobs already own.
pub trait MachineStore: PublishQuotaStore {
    /// The job's calendar entry (`entry_id` of the job's scope), if stored.
    fn entry_gate(&self, job: &PublishJob) -> Result<Option<EntryGate>, Fail>;
    /// Whether the stored account is paused; `None` when no account row exists.
    fn account_paused(&self, scope: &Scope, platform: &str) -> Result<Option<bool>, Fail>;
    /// The stored `remote` JSON text of every job on `platform` and `remote_account` other than
    /// `except_job`, in any scope.
    fn remote_json_on_account(
        &self,
        platform: &str,
        remote_account: &str,
        except_job: &str,
    ) -> Result<Vec<String>, Fail>;
}

/// Transient failures tolerated in one phase before the job fails with `retries_exhausted`.
pub const MAX_ATTEMPTS: i64 = 5;
/// Container status polls before the job fails with `container_timeout`.
pub const MAX_POLLS: i64 = 10;

/// What a command needs to talk to the platform. The token lives here for the length of the command.
pub struct Drive {
    pub client: Client,
    pub base: String,
    pub token: String,
    pub now: i64,
    pub poll_interval: i64,
    /// Bluesky only: the refresh JWT of the session, from the optional `refresh_token` input. Never stored.
    pub refresh: Option<String>,
    /// The access JWT minted by a refresh during this command; it replaces `token` for the rest of it.
    pub access: std::sync::Mutex<Option<String>>,
    /// The host app's endpoint settings: URLs discovered mid-job (Bluesky's PLC directory and PDS) pass the same
    /// loopback rule as the base URL.
    pub endpoints: http::EndpointSettings,
}

type Outcome = (&'static str, Option<String>);

fn output(job: PublishJob, outcome: &str, detail: Option<String>, calls: i64) -> PublishStepOutput {
    PublishStepOutput {
        job,
        outcome: outcome.to_string(),
        detail,
        calls,
    }
}

fn set_error(job: &mut PublishJob, code: &str, message: String) {
    job.last_error_code = Some(code.to_string());
    job.last_error_message = Some(message);
}

fn clear_error(job: &mut PublishJob) {
    job.last_error_code = None;
    job.last_error_message = None;
}

/// Move to the next phase after a success: counters and the wait are reset.
fn advance(job: &mut PublishJob, phase: &str) {
    job.phase = phase.to_string();
    job.attempts = 0;
    job.inflight = None;
    job.next_attempt_at = None;
    job.state = "running".to_string();
    clear_error(job);
}

fn fail(job: &mut PublishJob, code: &str, message: String) -> Outcome {
    job.state = "failed".to_string();
    job.inflight = None;
    job.next_attempt_at = None;
    set_error(job, code, message.clone());
    ("failed", Some(format!("{code}: {message}")))
}

/// A request was sent and its result is not known. Never retried blind.
fn unknown(job: &mut PublishJob, code: &str, message: String) -> Outcome {
    job.state = "outcome_unknown".to_string();
    job.inflight = None;
    job.next_attempt_at = None;
    set_error(job, code, message.clone());
    ("outcome_unknown", Some(format!("{code}: {message}")))
}

/// The post is live; only its permalink could not be read. The job stays truthful: `published`.
fn published_without_permalink(job: &mut PublishJob, code: &str, message: String) -> Outcome {
    job.state = "published".to_string();
    job.phase = "done".to_string();
    job.inflight = None;
    job.next_attempt_at = None;
    set_error(job, code, message.clone());
    ("published", Some(format!("{code}: {message}")))
}

/// A bounded wait. The request was not accepted (or was a read), so trying again is safe.
fn defer(job: &mut PublishJob, now: i64, at: i64, code: &str, message: String) -> Outcome {
    job.attempts += 1;
    job.inflight = None;
    if job.attempts > MAX_ATTEMPTS {
        return fail(
            job,
            "retries_exhausted",
            format!("{message} (gave up after {MAX_ATTEMPTS} retries)"),
        );
    }
    let at = at.max(now);
    job.state = "waiting".to_string();
    job.next_attempt_at = Some(time::format_instant(at));
    set_error(job, code, message.clone());
    (
        "waiting",
        Some(format!(
            "{code}: {message}; next attempt at {}",
            time::format_instant(at)
        )),
    )
}

/// Seconds to wait after transient failure number `attempts` (1-based): 5, 10, 20, 40, 80, ... up to 300.
fn backoff_secs(attempts: i64) -> i64 {
    let policy = rightkit_http::RetryPolicy {
        initial_backoff: std::time::Duration::from_secs(5),
        max_backoff: std::time::Duration::from_secs(300),
        multiplier: 2.0,
        jitter: 0.0,
        ..rightkit_http::RetryPolicy::default()
    };
    policy.backoff(attempts.clamp(1, 32) as u32).as_secs() as i64
}

/// The instant a 429 tells the job to wait until, given the job's transient failures so far in this
/// phase (`attempts`, before this one is counted). One rule for the job's own wait (`apply_as`) and for
/// the account's `rate_limited_until` (`step_once` -> `quota::record_rate_limit`), so they never differ.
fn rate_limit_at(after: &RetryAfter, now: i64, attempts: i64) -> i64 {
    match *after {
        RetryAfter::Seconds(secs) => now + secs.max(0),
        RetryAfter::At(at) => at.clamp(now, now + MAX_RETRY_AFTER_SECS),
        RetryAfter::Unspecified => now + backoff_secs(attempts + 1),
    }
}

/// The platform id in a create or publish response: a non-empty string, or a number.
fn id_of(body: &Value) -> Option<String> {
    match body.get("id")? {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// How many child containers the job's Instagram carousel needs; 0 for a reel, a photo, or a payload that
/// cannot be prepared (its own step then refuses it).
fn instagram_child_count(job: &PublishJob) -> usize {
    instagram::prepare(&job.payload, &job.remote_account).map_or(0, |p| p.child_count())
}

/// The carousel child ids stored so far: `remote.container_id` read as a JSON array of strings. Anything
/// else (nothing stored, a plain container id) is none.
fn instagram_children(job: &PublishJob) -> Vec<String> {
    job.remote
        .container_id
        .as_deref()
        .and_then(|text| serde_json::from_str::<Vec<Value>>(text).ok())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A 2xx answer for the job's current phase.
fn on_success(job: &mut PublishJob, body: &Value, d: &Drive) -> Outcome {
    let phase = job.phase.clone();
    match phase.as_str() {
        "create_container" => {
            // An Instagram carousel makes one child container per item before its parent: a child's id is
            // appended to `remote.container_id` (a JSON array) and the job stays at `create_container`.
            if job.platform == instagram::PLATFORM {
                let wanted = instagram_child_count(job);
                let mut children = instagram_children(job);
                if wanted > 0 && children.len() < wanted {
                    return match instagram::object_id(body) {
                        Some(id) => {
                            children.push(id);
                            let made = children.len();
                            job.remote.container_id = Some(
                                Value::Array(children.into_iter().map(Value::String).collect())
                                    .to_string(),
                            );
                            job.polls = 0;
                            advance(job, "create_container");
                            (
                                "advanced",
                                Some(format!("carousel child {made} of {wanted} created")),
                            )
                        }
                        None => unknown(
                            job,
                            "bad_response",
                            "the child create response carried no id; a container may exist"
                                .to_string(),
                        ),
                    };
                }
            }
            let is_threads = job.platform == threads::PLATFORM;
            let id = if is_threads {
                threads::object_id(body)
            } else if job.platform == instagram::PLATFORM {
                instagram::object_id(body)
            } else {
                id_of(body)
            };
            match id {
                Some(id) => {
                    job.remote.container_id = Some(id);
                    job.polls = 0;
                    // Threads has no container status read here: its text container is published next.
                    let polls = !is_threads || threads::needs_poll(&job.payload);
                    advance(job, if polls { "poll_status" } else { "publish" });
                    ("advanced", Some("container created".to_string()))
                }
                None => unknown(
                    job,
                    "bad_response",
                    "the create response carried no id; a container may exist".to_string(),
                ),
            }
        }
        "poll_status" => {
            job.polls += 1;
            let is_instagram = job.platform == instagram::PLATFORM;
            let is_threads = job.platform == threads::PLATFORM;
            let status = if is_threads {
                job.remote
                    .container_id
                    .as_deref()
                    .and_then(|container| threads::status_of(container, body))
            } else if is_instagram {
                // Instagram's answer must be this job's container (see `instagram::status_code`).
                job.remote
                    .container_id
                    .as_deref()
                    .and_then(|container| instagram::status_code(container, body))
            } else {
                body.get("status_code").and_then(Value::as_str)
            }
            .unwrap_or("");
            match status {
                "FINISHED" if job.platform == tiktok::PLATFORM => {
                    match tiktok::finished_post_id(body) {
                        Some(id) => {
                            job.remote.post_id = Some(id);
                            advance(job, "read_permalink");
                            ("advanced", Some("video finished".to_string()))
                        }
                        None => fail(
                            job,
                            "bad_response",
                            "the status answer was FINISHED but carried no video id".to_string(),
                        ),
                    }
                }
                "FINISHED" => {
                    advance(job, "publish");
                    ("advanced", Some("container finished".to_string()))
                }
                // An Instagram container that expired before it was published is replaced by a new one: the
                // job goes back to `create_container` (a new create, counted by pacing as a new round).
                "EXPIRED" if is_instagram && job.reconciles < instagram::MAX_RESTARTS => {
                    job.reconciles += 1;
                    job.remote.container_id = None;
                    job.polls = 0;
                    advance(job, "create_container");
                    (
                        "advanced",
                        Some("the container expired; a new container will be created".to_string()),
                    )
                }
                // Nothing of this job has been published at poll_status, so a container reporting PUBLISHED is
                // not ours to continue: stop and say so rather than poll it until the timeout.
                "PUBLISHED" if is_instagram || is_threads => fail(
                    job,
                    "container_failed",
                    "the container reports PUBLISHED although this job has not published it; check the account"
                        .to_string(),
                ),
                "EXPIRED" | "ERROR" => fail(
                    job,
                    "container_failed",
                    format!("the container status is {status}"),
                ),
                _ if job.polls >= MAX_POLLS => fail(
                    job,
                    "container_timeout",
                    format!("the container was not finished after {} polls", job.polls),
                ),
                _ => {
                    job.attempts = 0;
                    job.inflight = None;
                    clear_error(job);
                    if d.poll_interval > 0 {
                        job.state = "waiting".to_string();
                        job.next_attempt_at =
                            Some(time::format_instant(d.now + d.poll_interval));
                    } else {
                        job.state = "running".to_string();
                        job.next_attempt_at = None;
                    }
                    (
                        "polling",
                        Some(format!(
                            "container status {}; poll {}",
                            if status.is_empty() { "unknown" } else { status },
                            job.polls
                        )),
                    )
                }
            }
        }
        "publish" | "create" => {
            let id = if job.platform == bluesky::PLATFORM {
                bluesky::created_post_id(&job.idempotency_key, body)
            } else if job.platform == mastodon::PLATFORM {
                mastodon::created_post_id(body)
            } else if job.platform == linkedin::PLATFORM {
                linkedin::created_post_id(body)
            } else if job.platform == threads::PLATFORM {
                threads::object_id(body)
            } else if job.platform == instagram::PLATFORM {
                instagram::object_id(body)
            } else if job.platform == facebook::PLATFORM {
                facebook::created_post_id(
                    &job.remote_account,
                    body,
                    facebook::has_video(&job.payload),
                )
            } else if job.platform == x::PLATFORM {
                x::created_post_id(body)
            } else if job.platform == youtube::PLATFORM {
                youtube::created_video_id(body)
            } else if job.platform == pinterest::PLATFORM {
                pinterest::created_pin_id(body)
            } else {
                id_of(body)
            };
            match id {
                Some(id) => {
                    job.remote.post_id = Some(id);
                    advance(job, "read_permalink");
                    ("advanced", Some("post id stored".to_string()))
                }
                None => unknown(
                    job,
                    "bad_response",
                    format!("the {phase} response carried no id; a post may exist"),
                ),
            }
        }
        "read_permalink" => {
            let link: Option<String> = if job.platform == bluesky::PLATFORM {
                bluesky::permalink_from(&job.payload.settings, job.remote.post_id.as_deref(), body)
            } else if job.platform == mastodon::PLATFORM {
                mastodon::permalink_from(job.remote.post_id.as_deref(), body)
            } else if job.platform == linkedin::PLATFORM {
                linkedin::permalink_from(job.remote.post_id.as_deref(), body)
            } else if job.platform == threads::PLATFORM {
                threads::permalink_from(job.remote.post_id.as_deref(), body)
            } else if job.platform == instagram::PLATFORM {
                instagram::permalink_from(job.remote.post_id.as_deref(), body)
            } else if job.platform == facebook::PLATFORM {
                facebook::permalink_from(job.remote.post_id.as_deref(), body)
            } else if job.platform == x::PLATFORM {
                x::permalink_from(job.remote.post_id.as_deref(), body)
            } else if job.platform == youtube::PLATFORM {
                youtube::permalink_from(&job.payload.settings, job.remote.post_id.as_deref(), body)
            } else if job.platform == tiktok::PLATFORM {
                tiktok::permalink_from(&job.payload.settings, job.remote.post_id.as_deref(), body)
            } else if job.platform == pinterest::PLATFORM {
                pinterest::permalink_from(job.remote.post_id.as_deref(), body)
            } else {
                body.get("permalink")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_string)
            };
            match link {
                Some(link) => {
                    job.remote.permalink = Some(link.clone());
                    job.state = "published".to_string();
                    job.phase = "done".to_string();
                    job.attempts = 0;
                    job.inflight = None;
                    job.next_attempt_at = None;
                    clear_error(job);
                    ("published", Some(link))
                }
                None => published_without_permalink(
                    job,
                    "permalink_missing",
                    "the post is live but the read returned no permalink".to_string(),
                ),
            }
        }
        other => fail(job, "internal", format!("no handler for phase {other}")),
    }
}

/// Turn one platform answer into the job's next state. Pure: it touches only `job`.
fn apply(job: &mut PublishJob, reply: Reply, d: &Drive) -> Outcome {
    let write = platform::is_write_phase(&job.phase);
    let what = job.phase.clone();
    apply_as(job, reply, d, write, &what)
}

/// [`apply`] with the request's kind given. `write` says whether the request could have created
/// something (an ambiguous answer is then `outcome_unknown`); `what` names the request in messages
/// (the phase, or a read that precedes the phase's write). A read is never ambiguous: it is retried.
fn apply_as(job: &mut PublishJob, reply: Reply, d: &Drive, write: bool, what: &str) -> Outcome {
    let phase = what.to_string();
    match reply {
        Reply::Ok(body) => on_success(job, &body, d),
        Reply::Unauthorized => {
            if phase == "read_permalink" {
                published_without_permalink(
                    job,
                    "token_expired",
                    "the token expired before the permalink could be read; the post is live"
                        .to_string(),
                )
            } else {
                fail(
                    job,
                    "token_expired",
                    format!("the platform rejected the token during {phase}; not retried"),
                )
            }
        }
        Reply::RateLimited(after) => {
            let at = rate_limit_at(&after, d.now, job.attempts);
            defer(
                job,
                d.now,
                at,
                "rate_limited",
                format!("rate limited during {phase}"),
            )
        }
        Reply::ServerError(status) => {
            if write {
                unknown(
                    job,
                    "ambiguous_response",
                    format!("HTTP {status} on {phase}; the platform may have applied it"),
                )
            } else {
                let at = d.now + backoff_secs(job.attempts + 1);
                defer(
                    job,
                    d.now,
                    at,
                    "server_error",
                    format!("HTTP {status} on {phase}"),
                )
            }
        }
        Reply::Transport {
            connect_phase: true,
            message,
        } => {
            let at = d.now + backoff_secs(job.attempts + 1);
            defer(
                job,
                d.now,
                at,
                "connect_failed",
                format!("could not connect during {phase}: {message}"),
            )
        }
        Reply::Transport {
            connect_phase: false,
            message,
        } => {
            if write {
                unknown(
                    job,
                    "connection_lost",
                    format!("the connection failed after the {phase} request was sent: {message}"),
                )
            } else {
                let at = d.now + backoff_secs(job.attempts + 1);
                defer(
                    job,
                    d.now,
                    at,
                    "network_error",
                    format!("network error during {phase}: {message}"),
                )
            }
        }
        Reply::Timeout => {
            if write {
                unknown(
                    job,
                    "timeout",
                    format!("the {phase} request timed out after it was sent"),
                )
            } else {
                let at = d.now + backoff_secs(job.attempts + 1);
                defer(job, d.now, at, "timeout", format!("timeout during {phase}"))
            }
        }
        Reply::Rejected { status, message }
            if job.platform == linkedin::PLATFORM
                && linkedin::is_version_rejection(status, &message) =>
        {
            let text = linkedin::version_rejection(status, &message);
            if phase == "read_permalink" {
                published_without_permalink(job, "api_version_unsupported", text)
            } else {
                fail(job, "api_version_unsupported", text)
            }
        }
        Reply::Rejected { status, message }
            if job.platform == facebook::PLATFORM && facebook::is_permission_error(&message) =>
        {
            let text = format!(
                "HTTP {status} on {phase}: the token has no permission to post to this Page; not retried"
            );
            if phase == "read_permalink" {
                published_without_permalink(job, "permission_denied", text)
            } else {
                fail(job, "permission_denied", text)
            }
        }
        Reply::Rejected { status, message }
            if job.platform == instagram::PLATFORM && instagram::is_permission_error(&message) =>
        {
            let text = format!(
                "HTTP {status} on {phase}: the token has no permission to publish to this Instagram account; not retried"
            );
            if phase == "read_permalink" {
                published_without_permalink(job, "permission_denied", text)
            } else {
                fail(job, "permission_denied", text)
            }
        }
        Reply::Rejected { status, message }
            if job.platform == youtube::PLATFORM && status == 403 =>
        {
            // YouTube's 403 is final. An exhausted daily quota is `quota_exceeded` (nothing was created; the
            // quota resets by itself); any other 403 is a missing permission. Neither is retried.
            let (code, text) = if phase != "read_permalink" && youtube::is_quota_exceeded(&message)
            {
                (
                    "quota_exceeded",
                    format!(
                        "HTTP 403 on {phase}: the YouTube API quota is exhausted; nothing was posted; not retried"
                    ),
                )
            } else {
                (
                    "permission_denied",
                    format!(
                        "HTTP 403 on {phase}: the token has no permission for this request; not retried"
                    ),
                )
            };
            if phase == "read_permalink" {
                published_without_permalink(job, code, text)
            } else {
                fail(job, code, text)
            }
        }
        Reply::Rejected { status, message } if job.platform == x::PLATFORM && status == 403 => {
            // X's 403 is final. A text that duplicates a recent post of the account is `duplicate_content`;
            // any other 403 is a missing permission. Neither is retried.
            let (code, text) = if phase != "read_permalink" && x::is_duplicate_content(&message) {
                (
                    "duplicate_content",
                    format!(
                        "HTTP 403 on {phase}: X refuses a text that duplicates a recent post of this account; nothing was posted; not retried"
                    ),
                )
            } else {
                (
                    "permission_denied",
                    format!(
                        "HTTP 403 on {phase}: the token has no permission for this request; not retried"
                    ),
                )
            };
            if phase == "read_permalink" {
                published_without_permalink(job, code, text)
            } else {
                fail(job, code, text)
            }
        }
        Reply::Rejected { status, message } => {
            if phase == "read_permalink" {
                published_without_permalink(
                    job,
                    "permalink_unavailable",
                    format!("HTTP {status} reading the permalink: {message}"),
                )
            } else {
                fail(
                    job,
                    "platform_rejected",
                    format!("HTTP {status} on {phase}: {message}"),
                )
            }
        }
        Reply::Invalid(message) => fail(job, "request_invalid", message),
    }
}

/// `Some((code, message))` when the job may no longer go to the platform: its entry changed or lost
/// its approval, or the account is paused.
fn gate(
    store: &dyn MachineStore,
    job: &PublishJob,
) -> Result<Option<(&'static str, String)>, Fail> {
    match store.entry_gate(job)? {
        None => {
            return Ok(Some((
                "approval_invalidated",
                format!("calendar entry {} no longer exists", job.entry_id),
            )))
        }
        Some(EntryGate {
            revision,
            approval_state: approval,
            approved_entry_revision: approved,
        }) => {
            if revision != job.entry_revision
                || approval != "approved"
                || approved != Some(job.entry_revision)
            {
                return Ok(Some((
                    "approval_invalidated",
                    format!(
                        "entry {} is at revision {revision} with approval {approval}; the job was approved at revision {}",
                        job.entry_id, job.entry_revision
                    ),
                )));
            }
        }
    }
    if store.account_paused(&job.scope, &job.platform)? == Some(true) {
        return Ok(Some((
            "account_paused",
            format!("the {} account is paused", job.platform),
        )));
    }
    Ok(None)
}

enum Early {
    Return(&'static str, Option<String>),
    Send,
}

/// Stage 1. Mutates and saves `job` only when it has something to record. For a write phase it also
/// asks `quota::before_write`, in this same transaction and after the approval and pause checks: an
/// opted-in account whose pacing does not allow the job's first write is refused here (an `Err`, nothing
/// stored, nothing sent), and the send about to happen is recorded on the account's pacing state.
fn stage_a(
    store: &dyn MachineStore,
    job: &mut PublishJob,
    d: &Drive,
    stamp: &str,
    pace: &quota::Pace,
) -> Result<Early, Fail> {
    match job.state.as_str() {
        "published" | "failed" | "cancelled" => {
            return Ok(Early::Return(
                "terminal",
                Some(format!("job is {}", job.state)),
            ))
        }
        "outcome_unknown" => {
            return Ok(Early::Return(
                "outcome_unknown",
                Some("a request may have been accepted: run publish.reconcile".to_string()),
            ))
        }
        _ => {}
    }
    if let Some(phase) = job.inflight.clone() {
        let message = format!(
            "a {phase} request was started and its result was never stored (a crash, or another command ran at the same time)"
        );
        job.state = "outcome_unknown".to_string();
        job.inflight = None;
        job.next_attempt_at = None;
        set_error(job, "interrupted_in_flight", message.clone());
        store.save_job(job, stamp)?;
        return Ok(Early::Return("outcome_unknown", Some(message)));
    }
    if let Some(at) = job.next_attempt_at.clone() {
        if d.now < time::parse_instant(&at, "next_attempt_at")? {
            return Ok(Early::Return(
                "not_due",
                Some(format!("next attempt at {at}")),
            ));
        }
    }
    if platform::is_write_phase(&job.phase) {
        if let Some((code, message)) = gate(store, job)? {
            job.state = "cancelled".to_string();
            job.inflight = None;
            job.next_attempt_at = None;
            set_error(job, code, message.clone());
            store.save_job(job, stamp)?;
            return Ok(Early::Return(
                "cancelled",
                Some(format!("{code}: {message}")),
            ));
        }
        quota::before_write(store, job, d.now, pace)?;
        job.inflight = Some(job.phase.clone());
        job.state = "running".to_string();
        job.next_attempt_at = None;
        store.save_job(job, stamp)?;
    }
    Ok(Early::Send)
}

/// Records a 429 seen by a reconcile request on the job's account, as a publish 429 is recorded by `step_once`.
fn record_reconcile_limit(
    store: &dyn MachineStore,
    job: &PublishJob,
    d: &Drive,
    reply: &Reply,
) -> Result<(), Fail> {
    if let Reply::RateLimited(after) = reply {
        let until = rate_limit_at(after, d.now, job.attempts).min(d.now + MAX_RETRY_AFTER_SECS);
        store.write_txn(&mut || quota::record_rate_limit(store, job, d.now, until))?;
    }
    Ok(())
}

/// Bluesky create: createRecord at the record key derived from the job's idempotency key. No
/// `Idempotency-Key` header: the record key is what makes a repeat harmless. A payload this client
/// cannot post is `Reply::Invalid`: nothing is sent and the job fails `request_invalid`.
/// The access token to send: the one a refresh minted during this command, else the command's `token`.
fn current_token(d: &Drive) -> String {
    d.access
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .unwrap_or_else(|| d.token.clone())
}

/// Sends a Bluesky request and, when the access JWT is refused as expired and the command carries a
/// `refresh_token`, mints a new access JWT with `com.atproto.server.refreshSession` (the refresh JWT is the
/// bearer) and sends the SAME request once more. An expired-token answer means the request was not applied,
/// so the repeat cannot post twice (and a create carries the same record key anyway). A refresh that fails
/// leaves the original `token_expired` answer. Neither JWT is stored or put in a message.
fn bluesky_authed(d: &Drive, base: &str, send: impl Fn(&str) -> Reply) -> Reply {
    let reply = send(&current_token(d));
    if !matches!(reply, Reply::Unauthorized) {
        return reply;
    }
    let Some(refresh) = d.refresh.as_deref() else {
        return reply;
    };
    let refreshed = http::send(
        &d.client,
        Method::Post,
        &format!("{}{}", base, bluesky::REFRESH_SESSION_PATH),
        refresh,
        None,
        None,
    );
    match refreshed {
        Reply::Ok(body) => match bluesky::access_jwt(&body) {
            Some(access) => {
                if let Ok(mut guard) = d.access.lock() {
                    *guard = Some(access.clone());
                }
                send(&access)
            }
            None => reply,
        },
        _ => reply,
    }
}

fn bluesky_create(job: &PublishJob, d: &Drive) -> Reply {
    bluesky_create_at(job, d, &d.base, &job.remote_account)
}

/// [`bluesky_create`] against `base` (the account's PDS) for the repo `repo`.
fn bluesky_create_at(job: &PublishJob, d: &Drive, base: &str, repo: &str) -> Reply {
    let created_at = time::format_instant(d.now);
    let blobs = match media::stored(job.remote.container_id.as_deref(), job.payload.media.len()) {
        Ok(blobs) => blobs,
        Err((_, message)) => return Reply::Invalid(message),
    };
    match bluesky::create_request(
        repo,
        &job.idempotency_key,
        &job.payload,
        &created_at,
        &blobs,
    ) {
        Ok((path, body)) => bluesky_authed(d, base, |token| {
            bluesky::normalize(http::send(
                &d.client,
                Method::Post,
                &format!("{}{}", base, path),
                token,
                None,
                Some(&body),
            ))
        }),
        Err(message) => Reply::Invalid(message),
    }
}

/// Bluesky read back: getRecord for the stored post id (an at:// uri).
fn bluesky_read(job: &PublishJob, d: &Drive) -> Reply {
    bluesky_read_at(job, d, &d.base, &job.remote_account)
}

/// [`bluesky_read`] against `base` (the account's PDS) for the repo `repo`.
fn bluesky_read_at(job: &PublishJob, d: &Drive, base: &str, repo: &str) -> Reply {
    let Some(post_id) = job.remote.post_id.as_deref() else {
        return Reply::Invalid("read_permalink has no post id".to_string());
    };
    let Some(path) = bluesky::read_path(repo, post_id) else {
        return Reply::Invalid("the stored bluesky post id is not an at:// uri".to_string());
    };
    bluesky_authed(d, base, |token| {
        bluesky::normalize(http::send(
            &d.client,
            Method::Get,
            &format!("{}{}", base, path),
            token,
            None,
            None,
        ))
    })
}

/// Bluesky stage 2 for a job that names `settings.plc_directory`: the account's DID is resolved (from
/// `remote_account`, or from the handle through `resolveHandle` at the base URL), its DID document is read
/// from the directory, its PDS is taken from the document (and must pass the loopback rule like any base
/// URL), and before the create the handle in `settings.handle` is verified against the document and the
/// DID: a handle the DID does not claim, or that resolves to another DID, posts nothing
/// (`handle_mismatch`). The reads here are public (no bearer) and are retried like any read; the create or
/// read-back then goes to the discovered PDS, never to the base URL.
fn bluesky_resolved_send(job: &PublishJob, d: &Drive, directory: &str) -> Sent {
    let refuse = |code: &'static str, message: String, calls: i64| Sent::Refused {
        code,
        message,
        calls,
    };
    let directory = match http::base_url(&d.endpoints, bluesky::PLATFORM, Some(directory)) {
        Ok(url) => url,
        Err((code, message)) => return refuse(code, message, 0),
    };
    let mut calls = 0;
    let did = if bluesky::plain_did(&job.remote_account) {
        job.remote_account.clone()
    } else {
        calls += 1;
        let reply = http::send(
            &d.client,
            Method::Get,
            &format!(
                "{}{}",
                d.base,
                bluesky::resolve_handle_path(&job.remote_account)
            ),
            "",
            None,
            None,
        );
        match reply {
            Reply::Ok(body) => match bluesky::resolved_did(&body) {
                Some(did) => did,
                None => {
                    return refuse(
                        "did_unresolved",
                        "the handle resolved to no usable DID; nothing was posted".to_string(),
                        calls,
                    )
                }
            },
            other => {
                return Sent::Before {
                    reply: other,
                    calls,
                    what: "the handle resolution",
                }
            }
        }
    };
    let Some(doc_path) = bluesky::did_doc_path(&did) else {
        return refuse(
            "did_unresolved",
            "the DID is not usable in a path".to_string(),
            calls,
        );
    };
    calls += 1;
    let doc = match http::send(
        &d.client,
        Method::Get,
        &format!("{directory}{doc_path}"),
        "",
        None,
        None,
    ) {
        Reply::Ok(body) => match bluesky::parse_did_doc(&did, &body) {
            Some(doc) => doc,
            None => {
                return refuse(
                    "did_document_invalid",
                    "the DID document is missing or names another DID; nothing was posted"
                        .to_string(),
                    calls,
                )
            }
        },
        other => {
            return Sent::Before {
                reply: other,
                calls,
                what: "the DID document read",
            }
        }
    };
    let Some(pds) = doc.pds.as_deref() else {
        return refuse(
            "pds_unknown",
            "the DID document names no atproto PDS; nothing was posted".to_string(),
            calls,
        );
    };
    let pds = match http::base_url(&d.endpoints, bluesky::PLATFORM, Some(pds)) {
        Ok(url) => url,
        Err((code, message)) => return refuse(code, message, calls),
    };
    if job.phase == "create" {
        let handle = bluesky::job_handle(&job.payload.settings).unwrap_or_default();
        if !doc.handles.contains(&handle) {
            return refuse(
                "handle_mismatch",
                format!("the DID document does not claim the handle {handle}; nothing was posted"),
                calls,
            );
        }
        calls += 1;
        match http::send(
            &d.client,
            Method::Get,
            &format!("{}{}", d.base, bluesky::resolve_handle_path(&handle)),
            "",
            None,
            None,
        ) {
            Reply::Ok(body) => {
                if bluesky::resolved_did(&body).as_deref() != Some(did.as_str()) {
                    return refuse(
                        "handle_mismatch",
                        format!("the handle {handle} does not resolve to this account's DID; nothing was posted"),
                        calls,
                    );
                }
            }
            other => {
                return Sent::Before {
                    reply: other,
                    calls,
                    what: "the handle verification",
                }
            }
        }
    }
    let reply = if job.phase == "create" {
        bluesky_create_at(job, d, &pds, &did)
    } else {
        bluesky_read_at(job, d, &pds, &did)
    };
    Sent::Reply {
        reply,
        calls: calls + 1,
    }
}

/// What stage 2 produced. `calls` counts the platform requests the step made.
enum Sent {
    /// The step's own request was sent; its answer is the phase's outcome.
    Reply { reply: Reply, calls: i64 },
    /// A read that must precede the phase's write (Mastodon's instance limit) was not usable. The write
    /// was NOT sent, so nothing was created and the answer is handled as a read: retried, never unknown.
    Before {
        reply: Reply,
        calls: i64,
        what: &'static str,
    },
    /// The upload step succeeded: every media file was uploaded. `media` is the text for
    /// `remote.container_id`. The job stays at `create`; the next step posts.
    Uploaded { media: String, calls: i64 },
    /// The step refused locally, after `calls` requests. Nothing was created.
    Refused {
        code: &'static str,
        message: String,
        calls: i64,
    },
}

/// Mastodon create. First the instance's status limit: `settings.max_characters` when the job names one,
/// else one `GET /api/v2/instance` for this attempt. A text over the limit is refused here, before the
/// create is sent. Then `POST /api/v1/statuses` carrying `Idempotency-Key: <the job's key>`, so a repeat
/// of this request (after a lost answer, by reconcile) returns the first status instead of a second one.
fn mastodon_create(job: &PublishJob, d: &Drive) -> Sent {
    let prepared = match mastodon::prepare(&job.payload) {
        Ok(prepared) => prepared,
        Err((code, message)) => {
            return Sent::Refused {
                code,
                message,
                calls: 0,
            }
        }
    };
    let media_ids =
        match media::stored_ids(job.remote.container_id.as_deref(), job.payload.media.len()) {
            Ok(ids) => ids,
            Err((code, message)) => {
                return Sent::Refused {
                    code,
                    message,
                    calls: 0,
                }
            }
        };
    let mut calls = 0;
    // Opt-in account check: the token must belong to this job's `remote_account`. A mismatch posts nothing.
    if mastodon::wants_verify(&job.payload.settings) {
        calls += 1;
        let reply = http::send(
            &d.client,
            Method::Get,
            &format!("{}{}", d.base, mastodon::VERIFY_PATH),
            &d.token,
            None,
            None,
        );
        match reply {
            Reply::Ok(body) => match mastodon::account_id(&body) {
                Some(id) if id == job.remote_account => {}
                Some(id) => {
                    return Sent::Refused {
                        code: "account_mismatch",
                        message: format!(
                            "the token belongs to account {id}, which is not this job's remote_account; nothing was posted"
                        ),
                        calls,
                    }
                }
                None => {
                    return Sent::Refused {
                        code: "account_unverified",
                        message: "the account check answered with no usable account id; nothing was posted"
                            .to_string(),
                        calls,
                    }
                }
            },
            other => {
                return Sent::Before {
                    reply: other,
                    calls,
                    what: "the account check",
                }
            }
        }
    }
    let (limit, source) = match prepared.limit_override() {
        Some(limit) => (limit, "settings.max_characters"),
        None => {
            calls += 1;
            let reply = http::send(
                &d.client,
                Method::Get,
                &format!("{}{}", d.base, mastodon::INSTANCE_PATH),
                &d.token,
                None,
                None,
            );
            match reply {
                Reply::Ok(body) => match mastodon::instance_limit(&body) {
                    Some(limit) => (limit, "the instance"),
                    None => {
                        return Sent::Refused {
                            code: "limit_unknown",
                            message: "the instance configuration carries no usable configuration.statuses.max_characters; nothing was posted. Set settings.max_characters on the job to name the limit"
                                .to_string(),
                            calls,
                        }
                    }
                },
                other => {
                    return Sent::Before {
                        reply: other,
                        calls,
                        what: "the instance limit read",
                    }
                }
            }
        }
    };
    if let Err((code, message)) = prepared.within(limit, source) {
        return Sent::Refused {
            code,
            message,
            calls,
        };
    }
    calls += 1;
    Sent::Reply {
        reply: http::send(
            &d.client,
            Method::Post,
            &format!("{}{}", d.base, mastodon::STATUSES_PATH),
            &d.token,
            Some(job.idempotency_key.as_str()),
            Some(&prepared.body(&media_ids)),
        ),
        calls,
    }
}

/// Mastodon read back: `GET /api/v1/statuses/:id` for the stored status id.
fn mastodon_read(job: &PublishJob, d: &Drive) -> Reply {
    let Some(post_id) = job.remote.post_id.as_deref() else {
        return Reply::Invalid("read_permalink has no post id".to_string());
    };
    http::send(
        &d.client,
        Method::Get,
        &format!("{}{}", d.base, mastodon::read_path(post_id)),
        &d.token,
        None,
        None,
    )
}

/// LinkedIn create: one `POST /rest/posts` with the two version headers and NO `Idempotency-Key` (the
/// Posts API has none, so a repeat could post twice and the machine never repeats an ambiguous create).
/// The text is validated again here (it was at enqueue), so a payload that cannot be posted is refused
/// before the request is built. The `LinkedIn-Version` comes from the version registry.
fn linkedin_create(job: &PublishJob, d: &Drive) -> Sent {
    let images =
        match media::stored_ids(job.remote.container_id.as_deref(), job.payload.media.len()) {
            Ok(images) => images,
            Err((code, message)) => {
                return Sent::Refused {
                    code,
                    message,
                    calls: 0,
                }
            }
        };
    let prepared = match linkedin::prepare_with(&job.payload, &job.remote_account, &images) {
        Ok(prepared) => prepared,
        Err((code, message)) => {
            return Sent::Refused {
                code,
                message,
                calls: 0,
            }
        }
    };
    let version = match linkedin::api_version() {
        Ok(version) => version,
        Err((code, message)) => {
            return Sent::Refused {
                code,
                message,
                calls: 0,
            }
        }
    };
    let headers = linkedin::request_headers(version);
    Sent::Reply {
        reply: http::send_with(
            &d.client,
            Method::Post,
            &format!("{}{}", d.base, linkedin::POSTS_PATH),
            &d.token,
            None,
            Some(&prepared.body()),
            &http::Extra {
                headers: &headers,
                echo: &linkedin::ECHO,
            },
        ),
        calls: 1,
    }
}

/// LinkedIn read back: `GET /rest/posts/<url-encoded urn>` for the stored post URN, with the same two
/// version headers.
fn linkedin_read(job: &PublishJob, d: &Drive) -> Reply {
    let Some(post_id) = job.remote.post_id.as_deref() else {
        return Reply::Invalid("read_permalink has no post id".to_string());
    };
    let Some(path) = linkedin::read_path(post_id) else {
        return Reply::Invalid("the stored linkedin post id is not a post URN".to_string());
    };
    let version = match linkedin::api_version() {
        Ok(version) => version,
        Err((_, message)) => return Reply::Invalid(message),
    };
    let headers = linkedin::request_headers(version);
    http::send_with(
        &d.client,
        Method::Get,
        &format!("{}{}", d.base, path),
        &d.token,
        None,
        None,
        &http::Extra {
            headers: &headers,
            echo: &[],
        },
    )
}

/// Threads read back: `GET /v1.0/<media id>?fields=id,permalink,text,timestamp` for the stored media id.
/// The token is the bearer header only; nothing about it is in the URL.
fn threads_read_reply(job: &PublishJob, d: &Drive) -> Reply {
    let Some(post_id) = job.remote.post_id.as_deref() else {
        return Reply::Invalid("read_permalink has no post id".to_string());
    };
    let Some(path) = threads::read_path(post_id) else {
        return Reply::Invalid("the stored threads post id is not a plain id".to_string());
    };
    http::send(
        &d.client,
        Method::Get,
        &format!("{}{}", d.base, path),
        &d.token,
        None,
        None,
    )
}

/// Threads stage 2: the one request of this step. The text is validated again here (it was at enqueue), so
/// a payload that cannot be posted is refused with no request. Neither write carries an `Idempotency-Key`:
/// `threads_publish` has none, so it is never repeated after an ambiguous answer, and a repeated container
/// create only orphans an unpublished container.
fn threads_send(job: &PublishJob, d: &Drive) -> Sent {
    // The container status read (opt-in `wait_for_container`) is a read of an existing container.
    if job.phase == "poll_status" {
        let reply = match job
            .remote
            .container_id
            .as_deref()
            .and_then(threads::status_path)
        {
            Some(path) => http::send(
                &d.client,
                Method::Get,
                &format!("{}{}", d.base, path),
                &d.token,
                None,
                None,
            ),
            None => Reply::Invalid("poll_status has no usable container id".to_string()),
        };
        let calls = if matches!(reply, Reply::Invalid(_)) {
            0
        } else {
            1
        };
        return Sent::Reply {
            reply: threads::normalize(reply),
            calls,
        };
    }
    // The read-back of a live post is never refused for its text: the post exists whatever it says.
    if job.phase == "read_permalink" {
        let reply = threads_read_reply(job, d);
        let calls = if matches!(reply, Reply::Invalid(_)) {
            0
        } else {
            1
        };
        return Sent::Reply {
            reply: threads::normalize(reply),
            calls,
        };
    }
    let prepared = match threads::prepare_any(&job.payload, &job.remote_account) {
        Ok(prepared) => prepared,
        Err((code, message)) => {
            return Sent::Refused {
                code,
                message,
                calls: 0,
            }
        }
    };
    // The create_container step may make several requests (a carousel: one per item and the carousel's).
    let mut made: Option<i64> = None;
    let reply = match job.phase.as_str() {
        "create_container" => {
            let (reply, n) = threads::create_container(&prepared, |path, body| {
                http::send(
                    &d.client,
                    Method::Post,
                    &format!("{}{}", d.base, path),
                    &d.token,
                    None,
                    Some(body),
                )
            });
            made = Some(n);
            reply
        }
        "publish" => match job.remote.container_id.as_deref() {
            Some(container_id) => http::send(
                &d.client,
                Method::Post,
                &format!("{}{}", d.base, prepared.publish_path()),
                &d.token,
                None,
                Some(&threads::publish_body(container_id)),
            ),
            None => Reply::Invalid("publish has no container id".to_string()),
        },
        other => Reply::Invalid(format!("no threads request exists for phase {other}")),
    };
    let calls = made.unwrap_or(if matches!(reply, Reply::Invalid(_)) {
        0
    } else {
        1
    });
    Sent::Reply {
        reply: threads::normalize(reply),
        calls,
    }
}

/// Facebook read back: `GET /v<version>/<post id>?fields=id,message,permalink_url,created_time` for the
/// stored post id. The token is the bearer header only; nothing about it is in the URL.
fn facebook_read_reply(job: &PublishJob, d: &Drive) -> Reply {
    let Some(post_id) = job.remote.post_id.as_deref() else {
        return Reply::Invalid("read_permalink has no post id".to_string());
    };
    let Some(path) = facebook::read_path(post_id) else {
        return Reply::Invalid("the stored facebook post id is not a plain id".to_string());
    };
    http::send(
        &d.client,
        Method::Get,
        &format!("{}{}", d.base, path),
        &d.token,
        None,
        None,
    )
}

/// Facebook stage 2: the one request of this step. The text is validated again here (it was at enqueue), so
/// a payload that cannot be posted is refused with no request. The create carries NO `Idempotency-Key`: the
/// feed create has none, so it is never repeated after an ambiguous answer.
fn facebook_send(job: &PublishJob, d: &Drive) -> Sent {
    // The read-back of a live post is never refused for its text: the post exists whatever it says.
    if job.phase == "read_permalink" {
        let reply = facebook_read_reply(job, d);
        let calls = if matches!(reply, Reply::Invalid(_)) {
            0
        } else {
            1
        };
        return Sent::Reply {
            reply: facebook::normalize(reply),
            calls,
        };
    }
    let prepared = match facebook::prepare(&job.payload, &job.remote_account) {
        Ok(prepared) => prepared,
        Err((code, message)) => {
            return Sent::Refused {
                code,
                message,
                calls: 0,
            }
        }
    };
    let reply = match job.phase.as_str() {
        "create" => http::send(
            &d.client,
            Method::Post,
            &format!("{}{}", d.base, prepared.create_path()),
            &d.token,
            None,
            Some(&prepared.create_body()),
        ),
        other => Reply::Invalid(format!("no facebook request exists for phase {other}")),
    };
    let calls = if matches!(reply, Reply::Invalid(_)) {
        0
    } else {
        1
    };
    Sent::Reply {
        reply: facebook::normalize(reply),
        calls,
    }
}

/// X read back: `GET /2/tweets/<id>?tweet.fields=created_at,text` for the stored post id. The token is the
/// bearer header only; nothing about it is in the URL.
fn x_read_reply(job: &PublishJob, d: &Drive) -> Reply {
    let Some(post_id) = job.remote.post_id.as_deref() else {
        return Reply::Invalid("read_permalink has no post id".to_string());
    };
    let Some(path) = x::read_path(post_id) else {
        return Reply::Invalid("the stored x post id is not a plain id".to_string());
    };
    http::send(
        &d.client,
        Method::Get,
        &format!("{}{}", d.base, path),
        &d.token,
        None,
        None,
    )
}

/// X stage 2: the one request of this step. The text is validated again here (it was at enqueue), so a payload
/// that cannot be posted is refused with no request. The create carries NO `Idempotency-Key`: the tweet create
/// has none, so it is never repeated after an ambiguous answer.
fn x_send(job: &PublishJob, d: &Drive) -> Sent {
    // The read-back of a live post is never refused for its text: the post exists whatever it says.
    if job.phase == "read_permalink" {
        let reply = x_read_reply(job, d);
        let calls = if matches!(reply, Reply::Invalid(_)) {
            0
        } else {
            1
        };
        return Sent::Reply { reply, calls };
    }
    let prepared = match x::prepare(&job.payload) {
        Ok(prepared) => prepared,
        Err((code, message)) => {
            return Sent::Refused {
                code,
                message,
                calls: 0,
            }
        }
    };
    let media_ids =
        match media::stored_ids(job.remote.container_id.as_deref(), job.payload.media.len()) {
            Ok(ids) => ids,
            Err((code, message)) => {
                return Sent::Refused {
                    code,
                    message,
                    calls: 0,
                }
            }
        };
    let reply = match job.phase.as_str() {
        "create" => http::send(
            &d.client,
            Method::Post,
            &format!("{}{}", d.base, x::CREATE_PATH),
            &d.token,
            None,
            Some(&prepared.body(&media_ids)),
        ),
        other => Reply::Invalid(format!("no x request exists for phase {other}")),
    };
    let calls = if matches!(reply, Reply::Invalid(_)) {
        0
    } else {
        1
    };
    Sent::Reply { reply, calls }
}

/// Instagram container status: `GET /v<version>/<container id>?fields=status_code` for the stored container id.
/// A read, so its failures are retried (a poll) or leave the job as it was (a reconcile). The token is the bearer
/// header only; nothing about it is in the URL.
fn instagram_status_reply(job: &PublishJob, d: &Drive) -> Reply {
    let Some(container_id) = job.remote.container_id.as_deref() else {
        return Reply::Invalid("poll_status has no container id".to_string());
    };
    let Some(path) = instagram::status_path(container_id) else {
        return Reply::Invalid("the stored instagram container id is not a plain id".to_string());
    };
    http::send(
        &d.client,
        Method::Get,
        &format!("{}{}", d.base, path),
        &d.token,
        None,
        None,
    )
}

/// Instagram read back: `GET /v<version>/<media id>?fields=id,permalink,caption,timestamp` for the stored media
/// id. The token is the bearer header only; nothing about it is in the URL.
fn instagram_read_reply(job: &PublishJob, d: &Drive) -> Reply {
    let Some(post_id) = job.remote.post_id.as_deref() else {
        return Reply::Invalid("read_permalink has no post id".to_string());
    };
    let Some(path) = instagram::read_path(post_id) else {
        return Reply::Invalid("the stored instagram post id is not a plain id".to_string());
    };
    http::send(
        &d.client,
        Method::Get,
        &format!("{}{}", d.base, path),
        &d.token,
        None,
        None,
    )
}

/// Instagram stage 2: the one request of this step. The two reads (the container status and the read-back of a
/// live post) are never refused for the job's text: the container and the post exist whatever they say. The
/// writes validate the payload again here (it was at enqueue), so a payload that cannot be posted is refused with
/// no request. Neither write carries an `Idempotency-Key`: the container create and `media_publish` have none, so
/// `media_publish` is never repeated blind after an ambiguous answer, and a repeated container create only
/// orphans an unpublished container.
fn instagram_send(job: &PublishJob, d: &Drive) -> Sent {
    let read = match job.phase.as_str() {
        "poll_status" => Some(instagram_status_reply(job, d)),
        "read_permalink" => Some(instagram_read_reply(job, d)),
        _ => None,
    };
    if let Some(reply) = read {
        let calls = if matches!(reply, Reply::Invalid(_)) {
            0
        } else {
            1
        };
        return Sent::Reply {
            reply: instagram::normalize(reply),
            calls,
        };
    }
    let prepared = match instagram::prepare(&job.payload, &job.remote_account) {
        Ok(prepared) => prepared,
        Err((code, message)) => {
            return Sent::Refused {
                code,
                message,
                calls: 0,
            }
        }
    };
    let reply = match job.phase.as_str() {
        "create_container" => {
            // A carousel is one create per child (the next child is the number already stored) and then the
            // parent; anything else is the single create.
            let wanted = prepared.child_count();
            let body = if wanted == 0 {
                Some(prepared.create_body())
            } else {
                let children = instagram_children(job);
                if children.len() < wanted {
                    prepared.child_body(children.len())
                } else {
                    prepared.parent_body(&children)
                }
            };
            match body {
                Some(body) => http::send(
                    &d.client,
                    Method::Post,
                    &format!("{}{}", d.base, prepared.create_path()),
                    &d.token,
                    None,
                    Some(&body),
                ),
                None => Reply::Invalid(
                    "the stored carousel children do not match the media; nothing was sent"
                        .to_string(),
                ),
            }
        }
        "publish" => match job.remote.container_id.as_deref() {
            Some(container_id) => http::send(
                &d.client,
                Method::Post,
                &format!("{}{}", d.base, prepared.publish_path()),
                &d.token,
                None,
                Some(&instagram::publish_body(container_id)),
            ),
            None => Reply::Invalid("publish has no container id".to_string()),
        },
        other => Reply::Invalid(format!("no instagram request exists for phase {other}")),
    };
    let calls = if matches!(reply, Reply::Invalid(_)) {
        0
    } else {
        1
    };
    Sent::Reply {
        reply: instagram::normalize(reply),
        calls,
    }
}

/// YouTube stage 2: the provider module runs the whole step (the upload session, the chunks, the read-back)
/// and returns its result in the machine's own terms. `container_id` carries the stored upload session and
/// `post_id` the video id once the upload finished.
fn youtube_send(job: &PublishJob, d: &Drive) -> Sent {
    let step = youtube::send(
        &youtube::Job {
            phase: job.phase.as_str(),
            payload: &job.payload,
            session: job.remote.container_id.as_deref(),
            post_id: job.remote.post_id.as_deref(),
        },
        &d.client,
        &d.base,
        &d.token,
    );
    match step {
        youtube::Step::Reply { reply, calls } => Sent::Reply { reply, calls },
        youtube::Step::Before { reply, calls, what } => Sent::Before { reply, calls, what },
        youtube::Step::Uploaded { media, calls } => Sent::Uploaded { media, calls },
        youtube::Step::Refused {
            code,
            message,
            calls,
        } => Sent::Refused {
            code,
            message,
            calls,
        },
    }
}

/// TikTok: the provider module runs the whole step and returns its result in the machine's own terms.
/// `container_id` carries the publish id and `post_id` the video id once the status step finished.
fn tiktok_send(job: &PublishJob, d: &Drive) -> Sent {
    let step = tiktok::send(
        &tiktok::Job {
            phase: job.phase.as_str(),
            payload: &job.payload,
            publish_id: job.remote.container_id.as_deref(),
            post_id: job.remote.post_id.as_deref(),
        },
        &d.client,
        &d.base,
        &d.token,
    );
    match step {
        tiktok::Step::Reply { reply, calls } => Sent::Reply { reply, calls },
        tiktok::Step::Before { reply, calls, what } => Sent::Before { reply, calls, what },
        tiktok::Step::Refused {
            code,
            message,
            calls,
        } => Sent::Refused {
            code,
            message,
            calls,
        },
    }
}

/// Pinterest: `POST <base>/v5/pins` at create, `GET <base>/v5/pins/<id>` at read_permalink. The payload is
/// validated again here (it was at enqueue); a refusal sends nothing.
fn pinterest_send(job: &PublishJob, d: &Drive) -> Sent {
    if job.phase == "read_permalink" {
        let reply = match job.remote.post_id.as_deref().and_then(pinterest::read_path) {
            Some(path) => http::send(
                &d.client,
                Method::Get,
                &format!("{}{}", d.base, path),
                &d.token,
                None,
                None,
            ),
            None => Reply::Invalid("the stored pinterest pin id is not a plain id".to_string()),
        };
        let calls = if matches!(reply, Reply::Invalid(_)) {
            0
        } else {
            1
        };
        return Sent::Reply { reply, calls };
    }
    let prepared = match pinterest::prepare(&job.payload) {
        Ok(prepared) => prepared,
        Err((code, message)) => {
            return Sent::Refused {
                code,
                message,
                calls: 0,
            }
        }
    };
    let reply = http::send(
        &d.client,
        Method::Post,
        &format!("{}{}", d.base, pinterest::CREATE_PATH),
        &d.token,
        None,
        Some(&prepared.create_body()),
    );
    Sent::Reply { reply, calls: 1 }
}

/// True when the job's next step is its upload step: a media job of an image-uploading platform, at `create`,
/// with nothing stored yet.
fn needs_media_upload(job: &PublishJob) -> bool {
    job.phase == "create"
        && job.remote.container_id.is_none()
        && !job.payload.media.is_empty()
        && matches!(
            job.platform.as_str(),
            bluesky::PLATFORM | mastodon::PLATFORM | x::PLATFORM | linkedin::PLATFORM
        )
}

/// LinkedIn upload of ONE video or document already fetched into `bytes` (#135): the size and type checks, then
/// `initializeUpload` and the PUT of the file (a document), or `initializeUpload`, one PUT per named range and
/// `finalizeUpload` (a video; see `linkedin::upload_video`). `calls` is advanced by every request made here (the
/// fetch was counted by the caller). Returns the media URN, or the `Sent` that ends the step: a refusal for a
/// file that fails its checks or an answer with no usable URN, `Before` for a retryable failure.
fn linkedin_upload_file(
    job: &PublishJob,
    d: &Drive,
    kind: &str,
    hash: &str,
    bytes: &[u8],
    calls: &mut i64,
) -> Result<String, Sent> {
    let refuse = |code: &'static str, message: String, calls: i64| Sent::Refused {
        code,
        message,
        calls,
    };
    if let Err((code, message)) = linkedin::check_file(kind, bytes) {
        return Err(refuse(
            code,
            format!("media item {hash}: {message}"),
            *calls,
        ));
    }
    let owner = linkedin::owner(&job.payload, &job.remote_account)
        .map_err(|(code, message)| refuse(code, message, *calls))?;
    let version =
        linkedin::api_version().map_err(|(code, message)| refuse(code, message, *calls))?;
    let headers = linkedin::request_headers(version);
    // The initialise request is the first of the upload (one call); the rest are reported by the helper.
    *calls += 1;
    let post = |path: &str, body: &Value| {
        http::send_with(
            &d.client,
            Method::Post,
            &format!("{}{}", d.base, path),
            &d.token,
            None,
            Some(body),
            &http::Extra {
                headers: &headers,
                echo: &[],
            },
        )
    };
    // UNVERIFIED: LinkedIn's upload URLs take `application/octet-stream`; the part ETag comes back in `etag`.
    let (reply, extra) = if kind == "video" {
        linkedin::upload_video(
            &owner,
            bytes.len(),
            &d.base,
            post,
            |url, with_token, first, last| {
                http::send_body(
                    &d.client,
                    Method::Put,
                    url,
                    if with_token { d.token.as_str() } else { "" },
                    None,
                    Some(("application/octet-stream", bytes[first..=last].to_vec())),
                    &http::Extra {
                        headers: &[],
                        echo: &["etag"],
                    },
                )
            },
        )
    } else {
        linkedin::upload_document(&owner, &d.base, post, |url, with_token| {
            http::send_body(
                &d.client,
                Method::Put,
                url,
                if with_token { d.token.as_str() } else { "" },
                None,
                Some(("application/octet-stream", bytes.to_vec())),
                &http::Extra::NONE,
            )
        })
    };
    *calls += extra;
    match reply {
        Reply::Ok(body) => linkedin::media_urn(kind, &body).ok_or_else(|| {
            refuse(
                "bad_response",
                format!(
                    "the upload of media item {hash} answered with no usable media reference; nothing was posted"
                ),
                *calls,
            )
        }),
        other => Err(Sent::Before {
            reply: other,
            calls: *calls,
            what: "the media upload",
        }),
    }
}

/// The upload step. For each media item, in order: a plain GET of its URL (NO token), a size check against the
/// platform's limit, a magic-byte check, and the platform's upload request. A fetch or upload that fails in a
/// retryable way is `Sent::Before` (handled as a read: retried, never `outcome_unknown`, because an upload only
/// stages a file). A file that is too large or not an image, and an upload answer with no usable reference, are
/// refusals that fail the job. On success every reference is returned for `remote.container_id`.
/// `calls` counts each fetch and each upload request. The Bluesky upload goes through `bluesky_authed`, so an
/// expired access JWT is refreshed once like any other Bluesky request.
fn media_upload(job: &PublishJob, d: &Drive) -> Sent {
    let platform = job.platform.as_str();
    let refuse = |code: &'static str, message: String, calls: i64| Sent::Refused {
        code,
        message,
        calls,
    };
    if platform == bluesky::PLATFORM && bluesky::plc_directory(&job.payload.settings).is_some() {
        return refuse(
            "media_unsupported",
            "a bluesky job that resolves its PDS through settings.plc_directory cannot upload images: the blob upload to the discovered PDS is not implemented"
                .to_string(),
            0,
        );
    }
    let limit = if platform == linkedin::PLATFORM {
        linkedin::MAX_IMAGE_BYTES
    } else {
        media::max_bytes(platform)
    };
    let mut calls = 0;
    let mut stored: Vec<Value> = Vec::new();
    for item in &job.payload.media {
        let Some(url) = item.url.as_deref().filter(|u| media::plain_url(u)) else {
            return refuse(
                "media_url_invalid",
                format!("media item {} has no usable http(s) URL", item.hash),
                calls,
            );
        };
        calls += 1;
        let bytes = match http::fetch(&d.client, url) {
            Ok(bytes) => bytes,
            Err(reply) => {
                return Sent::Before {
                    reply,
                    calls,
                    what: "the media fetch",
                }
            }
        };
        // A LinkedIn video or document has its own limits, types and upload protocol.
        if platform == linkedin::PLATFORM && item.kind != "image" {
            match linkedin_upload_file(job, d, &item.kind, &item.hash, &bytes, &mut calls) {
                Ok(urn) => {
                    stored.push(Value::String(urn));
                    continue;
                }
                Err(sent) => return sent,
            }
        }
        if bytes.len() > limit {
            return refuse(
                "media_too_large",
                format!(
                    "media item {} is {} bytes and the {platform} image limit is {limit}; nothing was posted",
                    item.hash,
                    bytes.len()
                ),
                calls,
            );
        }
        let Some((mime, ext)) = media::sniff_image(&bytes) else {
            return refuse(
                "media_invalid",
                format!(
                    "media item {} is not a PNG, JPEG, GIF or WebP image; nothing was posted",
                    item.hash
                ),
                calls,
            );
        };
        calls += 1;
        let file_name = format!("media.{ext}");
        let reply = match platform {
            bluesky::PLATFORM => bluesky_authed(d, &d.base, |token| {
                bluesky::normalize(http::send_body(
                    &d.client,
                    Method::Post,
                    &format!("{}{}", d.base, bluesky::UPLOAD_BLOB_PATH),
                    token,
                    None,
                    Some((mime, bytes.clone())),
                    &http::Extra::NONE,
                ))
            }),
            mastodon::PLATFORM => {
                let (content_type, body) = media::multipart(&[], "file", &file_name, mime, &bytes);
                http::send_body(
                    &d.client,
                    Method::Post,
                    &format!("{}{}", d.base, mastodon::MEDIA_PATH),
                    &d.token,
                    None,
                    Some((content_type.as_str(), body)),
                    &http::Extra::NONE,
                )
            }
            linkedin::PLATFORM => {
                let owner = match linkedin::owner(&job.payload, &job.remote_account) {
                    Ok(owner) => owner,
                    Err((code, message)) => return refuse(code, message, calls),
                };
                let version = match linkedin::api_version() {
                    Ok(version) => version,
                    Err((code, message)) => return refuse(code, message, calls),
                };
                let headers = linkedin::request_headers(version);
                // The fetch above and the initialise request count as one `calls` each (already added);
                // `upload_image` reports only the requests beyond the first (the PUT).
                let (reply, extra) = linkedin::upload_image(
                    &owner,
                    &d.base,
                    |path, body| {
                        http::send_with(
                            &d.client,
                            Method::Post,
                            &format!("{}{}", d.base, path),
                            &d.token,
                            None,
                            Some(body),
                            &http::Extra {
                                headers: &headers,
                                echo: &[],
                            },
                        )
                    },
                    |url, with_token| {
                        http::send_body(
                            &d.client,
                            Method::Put,
                            url,
                            if with_token { d.token.as_str() } else { "" },
                            None,
                            Some((mime, bytes.clone())),
                            &http::Extra::NONE,
                        )
                    },
                );
                calls += extra;
                reply
            }
            _ => {
                let (content_type, body) = media::multipart(
                    &[("media_category", "tweet_image")],
                    "media",
                    &file_name,
                    mime,
                    &bytes,
                );
                http::send_body(
                    &d.client,
                    Method::Post,
                    &format!("{}{}", d.base, x::MEDIA_PATH),
                    &d.token,
                    None,
                    Some((content_type.as_str(), body)),
                    &http::Extra::NONE,
                )
            }
        };
        match reply {
            Reply::Ok(body) => {
                let reference = match platform {
                    bluesky::PLATFORM => bluesky::blob_of(&body),
                    mastodon::PLATFORM => mastodon::media_id(&body).map(Value::String),
                    linkedin::PLATFORM => linkedin::image_urn(&body).map(Value::String),
                    _ => x::media_id(&body).map(Value::String),
                };
                match reference {
                    Some(reference) => stored.push(reference),
                    None => {
                        return refuse(
                            "bad_response",
                            format!(
                                "the upload of media item {} answered with no usable media reference; nothing was posted",
                                item.hash
                            ),
                            calls,
                        )
                    }
                }
            }
            other => {
                return Sent::Before {
                    reply: other,
                    calls,
                    what: "the media upload",
                }
            }
        }
    }
    Sent::Uploaded {
        media: media::encode(&stored),
        calls,
    }
}

/// Stage 2: the one request of this step.
fn send_phase(job: &PublishJob, spec: &Spec, d: &Drive) -> Reply {
    let url = |path: String| format!("{}{}", d.base, path);
    let key = job.idempotency_key.as_str();
    match job.phase.as_str() {
        "create" if job.platform == bluesky::PLATFORM => bluesky_create(job, d),
        "read_permalink" if job.platform == bluesky::PLATFORM => bluesky_read(job, d),
        "read_permalink" if job.platform == mastodon::PLATFORM => mastodon_read(job, d),
        "read_permalink" if job.platform == linkedin::PLATFORM => linkedin_read(job, d),
        // Every shipped platform has a provider module; these generic paths serve a platform added without one.
        "create" => http::send(
            &d.client,
            Method::Post,
            &url(spec.create_path(&job.remote_account)),
            &d.token,
            Some(key),
            Some(&spec.create_body(&job.payload)),
        ),
        "read_permalink" => match job.remote.post_id.as_deref() {
            Some(id) => http::send(
                &d.client,
                Method::Get,
                &url(spec.read_path(&job.remote_account, id)),
                &d.token,
                None,
                None,
            ),
            None => Reply::Invalid("read_permalink has no post id".to_string()),
        },
        other => Reply::Invalid(format!("no request exists for phase {other}")),
    }
}

/// Advance one job by exactly one phase or one poll. See the module comment for the three stages.
/// `pace` says whether an opted-in account's pacing is enforced for this call (`quota::pace_for`); a 429
/// seen by the step is recorded on the account in the same transaction that stores the outcome.
pub fn step_once(
    store: &dyn MachineStore,
    scope: &Scope,
    job_id: &str,
    d: &Drive,
    pace: &quota::Pace,
) -> Result<PublishStepOutput, Fail> {
    let stamp = time::format_instant(d.now);
    let mut job = {
        let mut staged: Option<(PublishJob, Early)> = None;
        store.write_txn(&mut || {
            let mut job = store
                .load_job(scope, job_id)?
                .ok_or_else(|| not_found(job_id))?;
            let early = stage_a(store, &mut job, d, &stamp, pace)?;
            staged = Some((job, early));
            Ok(())
        })?;
        let (job, early) =
            staged.ok_or_else(|| ("internal", "stage 1 produced no job".to_string()))?;
        if let Early::Return(outcome, detail) = early {
            return Ok(output(job, outcome, detail, 0));
        }
        job
    };
    let spec = platform::spec(&job.platform).ok_or_else(|| {
        (
            "platform_unsupported",
            format!("no platform spec for {}", job.platform),
        )
    })?;
    let sent = if needs_media_upload(&job) {
        media_upload(&job, d)
    } else if job.platform == mastodon::PLATFORM && job.phase == "create" {
        mastodon_create(&job, d)
    } else if job.platform == linkedin::PLATFORM && job.phase == "create" {
        linkedin_create(&job, d)
    } else if let Some(directory) = (job.platform == bluesky::PLATFORM)
        .then(|| bluesky::plc_directory(&job.payload.settings))
        .flatten()
    {
        bluesky_resolved_send(&job, d, &directory)
    } else if job.platform == threads::PLATFORM {
        threads_send(&job, d)
    } else if job.platform == instagram::PLATFORM {
        instagram_send(&job, d)
    } else if job.platform == facebook::PLATFORM {
        facebook_send(&job, d)
    } else if job.platform == x::PLATFORM {
        x_send(&job, d)
    } else if job.platform == youtube::PLATFORM {
        youtube_send(&job, d)
    } else if job.platform == tiktok::PLATFORM {
        tiktok_send(&job, d)
    } else if job.platform == pinterest::PLATFORM {
        pinterest_send(&job, d)
    } else {
        Sent::Reply {
            reply: send_phase(&job, spec, d),
            calls: 1,
        }
    };
    // A 429 (or the platform's equivalent, already normalised into `Reply::RateLimited`) on any request of
    // this step is the platform telling the account to wait: remember the instant, computed by the same
    // rule as the job's own wait and held to the same 24 h clamp.
    let limited_until = match &sent {
        Sent::Reply {
            reply: Reply::RateLimited(after),
            ..
        }
        | Sent::Before {
            reply: Reply::RateLimited(after),
            ..
        } => Some(rate_limit_at(after, d.now, job.attempts).min(d.now + MAX_RETRY_AFTER_SECS)),
        _ => None,
    };
    // A step that refused locally sent nothing, so its first write (counted by `before_write` above) is
    // un-counted in the same transaction that stores the refusal.
    let refused_locally = matches!(sent, Sent::Refused { .. });
    let (outcome, detail, calls) = match sent {
        Sent::Reply { reply, calls } => {
            let (outcome, detail) = apply(&mut job, reply, d);
            (outcome, detail, calls)
        }
        Sent::Before { reply, calls, what } => {
            let (outcome, detail) = apply_as(&mut job, reply, d, false, what);
            (outcome, detail, calls)
        }
        Sent::Uploaded { media, calls } => {
            job.remote.container_id = Some(media);
            advance(&mut job, "create");
            (
                "advanced",
                Some("media uploaded; the post is created next".to_string()),
                calls,
            )
        }
        Sent::Refused {
            code,
            message,
            calls,
        } => {
            let (outcome, detail) = fail(&mut job, code, message);
            (outcome, detail, calls)
        }
    };
    let detail = if job.platform == pinterest::PLATFORM && job.state == "published" {
        Some(match detail {
            Some(text) => format!("{text}; {}", pinterest::TRIAL_VISIBILITY_NOTE),
            None => pinterest::TRIAL_VISIBILITY_NOTE.to_string(),
        })
    } else {
        detail
    };
    store.write_txn(&mut || {
        store.save_job(&mut job, &stamp)?;
        if refused_locally {
            quota::uncount_refused(store, &job, d.now)?;
        }
        if let Some(until) = limited_until {
            quota::record_rate_limit(store, &job, d.now, until)?;
        }
        Ok(())
    })?;
    Ok(output(job, outcome, detail, calls))
}

/// True when `publish.run` should stop after this step.
pub fn run_should_stop(out: &PublishStepOutput) -> bool {
    out.outcome == "not_due"
        || matches!(
            out.job.state.as_str(),
            "published" | "failed" | "cancelled" | "outcome_unknown" | "waiting"
        )
}

/// Whitespace-normalised caption, for matching a post by content.
fn fingerprint(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Post ids that other jobs for the same platform account already own.
fn claimed_posts(store: &dyn MachineStore, job: &PublishJob) -> Result<Vec<String>, Fail> {
    let mut ids = Vec::new();
    for text in store.remote_json_on_account(&job.platform, &job.remote_account, &job.job_id)? {
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            if let Some(id) = value.get("post_id").and_then(Value::as_str) {
                ids.push(id.to_string());
            }
        }
    }
    Ok(ids)
}

fn describe(reply: &Reply) -> String {
    match reply {
        Reply::Ok(_) => "ok".to_string(),
        Reply::RateLimited(_) => "HTTP 429: rate limited".to_string(),
        Reply::Unauthorized => "token_expired: HTTP 401".to_string(),
        Reply::ServerError(status) => format!("HTTP {status}"),
        Reply::Rejected { status, message } => format!("HTTP {status}: {message}"),
        Reply::Transport { message, .. } => format!("transport error: {message}"),
        Reply::Timeout => "timeout".to_string(),
        Reply::Invalid(message) => message.clone(),
    }
}

/// No post was found. The first time, return the job to `queued` for one safe retry (the idempotency
/// key is sent again). After that, leave it `outcome_unknown` for the operator.
fn requeue(
    store: &dyn MachineStore,
    mut job: PublishJob,
    stamp: &str,
    calls: i64,
    why: &str,
) -> Result<PublishStepOutput, Fail> {
    let (outcome, code, message) = if job.reconciles >= 1 {
        (
            "reconcile_exhausted",
            "reconcile_exhausted",
            "the one safe retry was already used and the outcome is still unknown; check the platform by hand".to_string(),
        )
    } else {
        job.reconciles += 1;
        job.state = "queued".to_string();
        job.attempts = 0;
        (
            "reconciled_requeued",
            "reconciled_retry",
            format!("no post found on the platform ({why}); queued for one safe retry"),
        )
    };
    job.inflight = None;
    job.next_attempt_at = None;
    set_error(&mut job, code, message.clone());
    store.write_txn(&mut || store.save_job(&mut job, stamp))?;
    Ok(output(job, outcome, Some(message), calls))
}

/// Bluesky reconcile. A post has a known record key, so no list is searched: getRecord reads that one
/// record. A record whose text is this job's text is adopted (its at:// uri becomes the post id and the
/// job continues at `read_permalink`); `RecordNotFound` returns the job to `queued` once, and the retry
/// targets the same record key, so it cannot make a second post; a record at the key with other text is
/// left for the operator; anything unreadable leaves the job `outcome_unknown`.
fn reconcile_bluesky(
    store: &dyn MachineStore,
    mut job: PublishJob,
    d: &Drive,
    stamp: &str,
) -> Result<PublishStepOutput, Fail> {
    let url = format!(
        "{}{}",
        d.base,
        bluesky::lookup_path(&job.remote_account, &job.idempotency_key)
    );
    let reply = bluesky_authed(d, &d.base, |token| {
        bluesky::normalize(http::send(&d.client, Method::Get, &url, token, None, None))
    });
    record_reconcile_limit(store, &job, d, &reply)?;
    match reply {
        Reply::Ok(body) => match bluesky::adoptable(&job.idempotency_key, &job.payload, &body) {
            Some(uri) => {
                job.remote.post_id = Some(uri.clone());
                advance(&mut job, "read_permalink");
                store.write_txn(&mut || store.save_job(&mut job, stamp))?;
                Ok(output(
                    job,
                    "reconciled_adopted",
                    Some(format!("adopted post {uri}")),
                    1,
                ))
            }
            None => {
                let message =
                    "a record exists at this job's record key but it is not this job's post; none was adopted. Check the platform by hand"
                        .to_string();
                set_error(&mut job, "reconcile_ambiguous", message.clone());
                store.write_txn(&mut || store.save_job(&mut job, stamp))?;
                Ok(output(job, "reconcile_ambiguous", Some(message), 1))
            }
        },
        Reply::Rejected { message, .. } if bluesky::is_not_found(&message) => requeue(
            store,
            job,
            stamp,
            1,
            "no record exists at this job's record key",
        ),
        other => Ok(output(
            job,
            "reconcile_failed",
            Some(format!(
                "the record could not be read ({}); the job stays outcome_unknown",
                describe(&other)
            )),
            1,
        )),
    }
}

/// Mastodon reconcile. The status id is only ever learned from the create answer, and that answer was
/// lost, so there is nothing to read. The API's contract is that a repeated create carrying the same
/// `Idempotency-Key` returns the FIRST status instead of making another, so reconcile repeats the create
/// WITH THE SAME KEY: if the first request had been applied, the answer is that status (adopted: its id
/// is stored and the job continues at `read_permalink`); if it had not, this request is the one safe
/// retry and creates the single status. Either way the job ends with exactly one status, which is what
/// the reconcile contract requires; no recent-posts list is searched and none can be mistaken for it.
///
/// This is a write, so it is gated like one: the approval and the account pause are re-checked, and the
/// create is not repeated once the key may have expired (`mastodon::KEY_WINDOW_SECS` after the ambiguous
/// answer; a failed reconcile does not save the job, so the window is not extended by trying). In every
/// case where it does not adopt, the job stays `outcome_unknown` and `reconcile_failed` says why.
fn reconcile_mastodon(
    store: &dyn MachineStore,
    mut job: PublishJob,
    d: &Drive,
    stamp: &str,
    pace: &quota::Pace,
) -> Result<PublishStepOutput, Fail> {
    let failed = |job: PublishJob, why: String, calls: i64| {
        Ok(output(job, "reconcile_failed", Some(why), calls))
    };
    if job.phase != "create" {
        return failed(
            job,
            "a mastodon job is only unsure at create; check the account by hand".to_string(),
            0,
        );
    }
    let since = time::parse_instant(&job.updated_at, "updated_at")?;
    if d.now - since > mastodon::KEY_WINDOW_SECS {
        return failed(
            job,
            format!(
                "the create was last attempted more than {} seconds ago, so its Idempotency-Key may have expired and repeating it could post twice; check the account by hand",
                mastodon::KEY_WINDOW_SECS
            ),
            0,
        );
    }
    if let Some((code, message)) = gate(store, &job)? {
        return failed(
            job,
            format!("the create is not repeated: {code}: {message}"),
            0,
        );
    }
    let prepared = match mastodon::prepare(&job.payload) {
        Ok(prepared) => prepared,
        Err((code, message)) => return failed(job, format!("{code}: {message}"), 0),
    };
    // A job with media whose upload step was cut short has no stored attachments: the repeat would post the
    // status without them, so nothing is sent and a person decides.
    let media_ids =
        match media::stored_ids(job.remote.container_id.as_deref(), job.payload.media.len()) {
            Ok(ids) => ids,
            Err((code, message)) => return failed(job, format!("{code}: {message}"), 0),
        };
    // The repeat is a provider write, so it passes the account's pacing as a create does. It is charged to this
    // job's round (already counted by its first create), so it adds no count of its own; a refusal sends nothing.
    let mut refused: Option<Fail> = None;
    let written = store.write_txn(&mut || {
        if let Err(fail) = quota::before_write(store, &job, d.now, pace) {
            refused = Some(fail.clone());
            return Err(fail);
        }
        Ok(())
    });
    if let Some((code, message)) = refused {
        return failed(
            job,
            format!("the create is not repeated: {code}: {message}"),
            0,
        );
    }
    written?;
    let reply = http::send(
        &d.client,
        Method::Post,
        &format!("{}{}", d.base, mastodon::STATUSES_PATH),
        &d.token,
        Some(job.idempotency_key.as_str()),
        Some(&prepared.body(&media_ids)),
    );
    record_reconcile_limit(store, &job, d, &reply)?;
    let adopted = match &reply {
        Reply::Ok(body) => mastodon::created_post_id(body),
        _ => None,
    };
    match adopted {
        Some(id) => {
            job.remote.post_id = Some(id.clone());
            advance(&mut job, "read_permalink");
            store.write_txn(&mut || store.save_job(&mut job, stamp))?;
            Ok(output(
                job,
                "reconciled_adopted",
                Some(format!("adopted status {id}")),
                1,
            ))
        }
        None => {
            let why = match &reply {
                Reply::Ok(_) => "the repeated create answered with no usable status id".to_string(),
                other => describe(other),
            };
            failed(
                job,
                format!("the repeated create did not return a status ({why}); the job stays outcome_unknown"),
                1,
            )
        }
    }
}

/// LinkedIn reconcile. The Posts API has no idempotency key, so a create cannot be repeated safely: if
/// the first request had been applied, a second would post twice, and nothing the engine sends can tell
/// the two cases apart. Reconcile therefore NEVER creates. It can only adopt a post it can READ BY URN,
/// and the URN is only ever learned from the create answer, which is exactly what was lost. So:
///
/// - no URN stored (every ambiguous create today): nothing is sent (`reconcile_failed`, zero calls) and
///   the job stays `outcome_unknown` for a person to check the member's feed and decide. No recent-posts
///   list is searched: a text match can be wrong, and a wrong adopt hides a missing post;
/// - a URN stored (not reached by any path today; kept so a URN learned some other way is never
///   discarded): `GET /rest/posts/<urn>`; a post whose `id` is that URN and whose `commentary` is this
///   job's text is adopted (`reconciled_adopted`, continue at `read_permalink`); anything else leaves the
///   job `outcome_unknown`.
///
/// Because it can never write, it needs no approval or pause re-check and no time window.
fn reconcile_linkedin(
    store: &dyn MachineStore,
    mut job: PublishJob,
    d: &Drive,
    stamp: &str,
) -> Result<PublishStepOutput, Fail> {
    let failed = |job: PublishJob, why: String, calls: i64| {
        Ok(output(job, "reconcile_failed", Some(why), calls))
    };
    if job.phase != "create" {
        return failed(
            job,
            "a linkedin job is only unsure at create; check the member's feed by hand".to_string(),
            0,
        );
    }
    let Some(urn) = job.remote.post_id.clone() else {
        return failed(
            job,
            "LinkedIn's Posts API has no idempotency key and the answer that carried the post URN was lost, so repeating the create could post twice; nothing was sent and the job stays outcome_unknown. Check the member's feed by hand"
                .to_string(),
            0,
        );
    };
    let reply = linkedin_read(&job, d);
    record_reconcile_limit(store, &job, d, &reply)?;
    let adoptable = match &reply {
        Reply::Ok(body) => linkedin::confirmed_commentary(&urn, body)
            .is_some_and(|text| fingerprint(text) == fingerprint(&job.payload.caption)),
        _ => false,
    };
    if !adoptable {
        let why = match &reply {
            Reply::Ok(_) => "the post at the stored URN is not this job's post".to_string(),
            other => describe(other),
        };
        return failed(
            job,
            format!(
                "the stored post could not be confirmed ({why}); the job stays outcome_unknown"
            ),
            1,
        );
    }
    advance(&mut job, "read_permalink");
    store.write_txn(&mut || store.save_job(&mut job, stamp))?;
    Ok(output(
        job,
        "reconciled_adopted",
        Some(format!("adopted post {urn}")),
        1,
    ))
}

/// Threads reconcile, for a job that is `outcome_unknown` at `publish` (one unsure at `create_container`
/// never gets here: `reconcile_once` returns it to `queued` first, because an orphaned container is
/// harmless and the retry makes a new one). `threads_publish` has no idempotency key, so it cannot be
/// repeated safely: if the first request had been applied, a second could post twice, and nothing the
/// engine sends can tell the two cases apart. Reconcile therefore NEVER publishes. It can only adopt a post
/// it can READ BY MEDIA ID, and the media id is only ever learned from the publish answer, which is exactly
/// what was lost. So:
///
/// - no media id stored (every ambiguous publish today): nothing is sent (`reconcile_failed`, zero calls)
///   and the job stays `outcome_unknown` for a person to check the account's threads and decide. No
///   recent-posts list is searched: a text match can be wrong, and a wrong adopt hides a missing post;
/// - a media id stored (not reached by any path today; kept so an id learned some other way is never
///   discarded): `GET /v1.0/<id>?fields=...`; a post whose `id` is that media id and whose `text` is this
///   job's text is adopted (`reconciled_adopted`, continue at `read_permalink`); anything else leaves the
///   job `outcome_unknown`.
///
/// Because it can never write, it needs no approval or pause re-check and no time window.
fn reconcile_threads(
    store: &dyn MachineStore,
    mut job: PublishJob,
    d: &Drive,
    stamp: &str,
) -> Result<PublishStepOutput, Fail> {
    let failed = |job: PublishJob, why: String, calls: i64| {
        Ok(output(job, "reconcile_failed", Some(why), calls))
    };
    if job.phase != "publish" {
        return failed(
            job,
            "a threads job is only unsure at create_container or publish; check the account's threads by hand"
                .to_string(),
            0,
        );
    }
    let Some(media_id) = job.remote.post_id.clone() else {
        return failed(
            job,
            "Threads' publish step has no idempotency key and the answer that carried the media id was lost, so repeating it could post twice; nothing was sent and the job stays outcome_unknown. Check the account's threads by hand"
                .to_string(),
            0,
        );
    };
    let reply = threads::normalize(threads_read_reply(&job, d));
    record_reconcile_limit(store, &job, d, &reply)?;
    let adoptable = match &reply {
        Reply::Ok(body) => threads::confirmed_text(&media_id, body)
            .is_some_and(|text| fingerprint(text) == fingerprint(&job.payload.caption)),
        _ => false,
    };
    if !adoptable {
        let why = match &reply {
            Reply::Ok(_) => "the post at the stored media id is not this job's post".to_string(),
            other => describe(other),
        };
        return failed(
            job,
            format!(
                "the stored post could not be confirmed ({why}); the job stays outcome_unknown"
            ),
            1,
        );
    }
    advance(&mut job, "read_permalink");
    store.write_txn(&mut || store.save_job(&mut job, stamp))?;
    Ok(output(
        job,
        "reconciled_adopted",
        Some(format!("adopted post {media_id}")),
        1,
    ))
}

/// Facebook reconcile. The feed create has no idempotency key, so it cannot be repeated safely: if the first
/// request had been applied, a second would post twice, and nothing the engine sends can tell the two cases
/// apart. Reconcile therefore NEVER creates. It can only adopt a post it can READ BY POST ID, and the post id
/// is only ever learned from the create answer, which is exactly what was lost. So:
///
/// - no post id stored (every ambiguous create today): nothing is sent (`reconcile_failed`, zero calls) and
///   the job stays `outcome_unknown` for a person to check the Page's feed and decide. No recent-posts list is
///   searched: a text match can be wrong, and a wrong adopt hides a missing post;
/// - a post id stored (not reached by any path today; kept so an id learned some other way is never
///   discarded): `GET /v<version>/<id>?fields=...`; a post whose `id` is that post id and whose `message` is
///   this job's text is adopted (`reconciled_adopted`, continue at `read_permalink`); anything else leaves the
///   job `outcome_unknown`.
///
/// Because it can never write, it needs no approval or pause re-check and no time window.
fn reconcile_facebook(
    store: &dyn MachineStore,
    mut job: PublishJob,
    d: &Drive,
    stamp: &str,
) -> Result<PublishStepOutput, Fail> {
    let failed = |job: PublishJob, why: String, calls: i64| {
        Ok(output(job, "reconcile_failed", Some(why), calls))
    };
    if job.phase != "create" {
        return failed(
            job,
            "a facebook job is only unsure at create; check the Page's feed by hand".to_string(),
            0,
        );
    }
    let Some(post_id) = job.remote.post_id.clone() else {
        return failed(
            job,
            "Facebook's feed create has no idempotency key and the answer that carried the post id was lost, so repeating the create could post twice; nothing was sent and the job stays outcome_unknown. Check the Page's feed by hand"
                .to_string(),
            0,
        );
    };
    let reply = facebook::normalize(facebook_read_reply(&job, d));
    record_reconcile_limit(store, &job, d, &reply)?;
    let adoptable = match &reply {
        Reply::Ok(body) => facebook::confirmed_message(&post_id, body)
            .is_some_and(|text| fingerprint(text) == fingerprint(&job.payload.caption)),
        _ => false,
    };
    if !adoptable {
        let why = match &reply {
            Reply::Ok(_) => "the post at the stored id is not this job's post".to_string(),
            other => describe(other),
        };
        return failed(
            job,
            format!(
                "the stored post could not be confirmed ({why}); the job stays outcome_unknown"
            ),
            1,
        );
    }
    advance(&mut job, "read_permalink");
    store.write_txn(&mut || store.save_job(&mut job, stamp))?;
    Ok(output(
        job,
        "reconciled_adopted",
        Some(format!("adopted post {post_id}")),
        1,
    ))
}

/// X reconcile. The tweet create has no idempotency key, so it cannot be repeated safely: if the first request
/// had been applied, a second would post twice (or be refused as duplicate content, which is not relied on),
/// and nothing the engine sends can tell the two cases apart. Reconcile therefore NEVER creates. It can only
/// adopt a post it can READ BY ID, and the id is only ever learned from the create answer, which is exactly
/// what was lost. So:
///
/// - no post id stored (every ambiguous create today): nothing is sent (`reconcile_failed`, zero calls) and
///   the job stays `outcome_unknown` for a person to check the account's posts and record what they saw with
///   `publish.resolve_unknown`. No recent-posts list is searched: a text match can be wrong, and a wrong adopt
///   hides a missing post;
/// - a post id stored (not reached by any path today; kept so an id learned some other way is never
///   discarded): `GET /2/tweets/<id>?tweet.fields=...`; a post whose `data.id` is that id and whose `text` is
///   this job's text is adopted (`reconciled_adopted`, continue at `read_permalink`); anything else leaves the
///   job `outcome_unknown`.
///
/// Because it can never write, it needs no approval or pause re-check and no time window.
fn reconcile_x(
    store: &dyn MachineStore,
    mut job: PublishJob,
    d: &Drive,
    stamp: &str,
) -> Result<PublishStepOutput, Fail> {
    let failed = |job: PublishJob, why: String, calls: i64| {
        Ok(output(job, "reconcile_failed", Some(why), calls))
    };
    if job.phase != "create" {
        return failed(
            job,
            "an x job is only unsure at create; check the account's posts by hand".to_string(),
            0,
        );
    }
    let Some(post_id) = job.remote.post_id.clone() else {
        return failed(
            job,
            "X's tweet create has no idempotency key and the answer that carried the post id was lost, so repeating the create could post twice; nothing was sent and the job stays outcome_unknown. Check the account's posts by hand, then record what you saw with publish.resolve_unknown"
                .to_string(),
            0,
        );
    };
    let reply = x_read_reply(&job, d);
    record_reconcile_limit(store, &job, d, &reply)?;
    let adoptable = match &reply {
        Reply::Ok(body) => x::confirmed_text(&post_id, body)
            .is_some_and(|text| fingerprint(text) == fingerprint(&job.payload.caption)),
        _ => false,
    };
    if !adoptable {
        let why = match &reply {
            Reply::Ok(_) => "the post at the stored id is not this job's post".to_string(),
            other => describe(other),
        };
        return failed(
            job,
            format!(
                "the stored post could not be confirmed ({why}); the job stays outcome_unknown"
            ),
            1,
        );
    }
    advance(&mut job, "read_permalink");
    store.write_txn(&mut || store.save_job(&mut job, stamp))?;
    Ok(output(
        job,
        "reconciled_adopted",
        Some(format!("adopted post {post_id}")),
        1,
    ))
}

/// Writes `job` in one transaction.
fn store_job(store: &dyn MachineStore, job: &mut PublishJob, stamp: &str) -> Result<(), Fail> {
    store.write_txn(&mut || store.save_job(job, stamp))
}

/// Instagram reconcile, for a job that is `outcome_unknown` at `publish` (one unsure at `create_container` never
/// gets here: `reconcile_once` returns it to `queued` first, because an orphaned container is not a post and the
/// retry makes a new one). `media_publish` has no idempotency key, so it is never repeated blind. Reconcile reads
/// the one thing that can tell the two cases apart, the CONTAINER's `status_code`
/// (`GET /v<version>/<container id>?fields=status_code`), and sends nothing else:
///
/// - `FINISHED`: a published container reports `PUBLISHED`, so the publish did not happen. The job returns to
///   `queued` at `publish` with its container, once (`reconciled_requeued`; `job.reconciles` counts it, at most
///   [`instagram::MAX_RESTARTS`] restarts of any kind per job), and the next step sends the publish again. The
///   container cannot be published twice by the platform, so a wrong reading could only be refused, not doubled;
/// - `PUBLISHED`: the post exists, but the container read does not return its media id and the publish answer
///   that carried it was lost. Nothing is guessed and no feed is searched: the job stays `outcome_unknown` with
///   `published_id_unknown` recorded (`reconcile_failed`) for a person to find the post and record its id with
///   `publish.resolve_unknown`;
/// - any other status, or a status that cannot be read: nothing is proven, the job stays `outcome_unknown`
///   (`reconcile_failed`).
///
/// It never sends a write, so it needs no approval or pause re-check (the repeat, when queued, is a write step
/// and is gated like one).
fn reconcile_instagram(
    store: &dyn MachineStore,
    mut job: PublishJob,
    d: &Drive,
    stamp: &str,
) -> Result<PublishStepOutput, Fail> {
    let failed = |job: PublishJob, why: String, calls: i64| {
        Ok(output(job, "reconcile_failed", Some(why), calls))
    };
    if job.phase != "publish" {
        return failed(
            job,
            "an instagram job is only unsure at create_container or publish; check the account by hand"
                .to_string(),
            0,
        );
    }
    let Some(container_id) = job.remote.container_id.clone() else {
        return failed(
            job,
            "no container id is stored for this job, so there is nothing to read; nothing was sent and the job stays outcome_unknown. Check the account by hand"
                .to_string(),
            0,
        );
    };
    let reply = instagram::normalize(instagram_status_reply(&job, d));
    record_reconcile_limit(store, &job, d, &reply)?;
    let calls = if matches!(reply, Reply::Invalid(_)) {
        0
    } else {
        1
    };
    let status = match &reply {
        Reply::Ok(body) => instagram::status_code(&container_id, body),
        _ => None,
    };
    match status {
        Some("FINISHED") => {
            if job.reconciles >= instagram::MAX_RESTARTS {
                let message = "the job was already restarted as often as allowed and the outcome is still unknown; check the account by hand".to_string();
                set_error(&mut job, "reconcile_exhausted", message.clone());
                store_job(store, &mut job, stamp)?;
                return Ok(output(job, "reconcile_exhausted", Some(message), calls));
            }
            job.reconciles += 1;
            job.state = "queued".to_string();
            job.attempts = 0;
            job.inflight = None;
            job.next_attempt_at = None;
            let message = format!(
                "container {container_id} reports FINISHED, so the publish was not made (a published container reports PUBLISHED); queued for one repeat of the publish"
            );
            set_error(&mut job, "reconciled_retry", message.clone());
            store_job(store, &mut job, stamp)?;
            Ok(output(job, "reconciled_requeued", Some(message), calls))
        }
        Some("PUBLISHED") => {
            let message = format!(
                "container {container_id} reports PUBLISHED, so the post exists, but the container read does not return its media id and the publish answer was lost; nothing was sent and the job stays outcome_unknown. Find the post on the account, then record its id with publish.resolve_unknown"
            );
            set_error(&mut job, "published_id_unknown", message.clone());
            store_job(store, &mut job, stamp)?;
            Ok(output(job, "reconcile_failed", Some(message), calls))
        }
        Some(other) => failed(
            job,
            format!(
                "container {container_id} reports {other}, which does not show whether the publish was made; the job stays outcome_unknown. Check the account by hand, then record what you saw with publish.resolve_unknown"
            ),
            calls,
        ),
        None => {
            let why = match &reply {
                Reply::Ok(_) => "the answer carried no status for this container".to_string(),
                other => describe(other),
            };
            failed(
                job,
                format!("the container status could not be read ({why}); the job stays outcome_unknown"),
                calls,
            )
        }
    }
}

/// `publish.reconcile`. Reads the platform's recent posts for the job's account and looks for this
/// job's post: first by the idempotency key (when the platform reports one), else by caption
/// fingerprint among posts no other job owns. Exactly one match is adopted (its id is stored and the
/// job continues at `read_permalink`); several matches are left for the operator; none returns the job
/// to `queued` once. A container is not a post, so a job that was unsure at `create_container` skips
/// the read.
pub fn reconcile_once(
    store: &dyn MachineStore,
    scope: &Scope,
    job_id: &str,
    d: &Drive,
    pace: &quota::Pace,
) -> Result<PublishStepOutput, Fail> {
    let stamp = time::format_instant(d.now);
    let mut job = store
        .load_job(scope, job_id)?
        .ok_or_else(|| not_found(job_id))?;
    if job.state != "outcome_unknown" {
        return Err((
            "invalid_transition",
            format!(
                "job {} is {}: only an outcome_unknown job is reconciled",
                job.job_id, job.state
            ),
        ));
    }
    if job.phase == "create_container" {
        return requeue(
            store,
            job,
            &stamp,
            0,
            "a container is not a post, so there is nothing to find",
        );
    }
    if job.platform == bluesky::PLATFORM {
        return reconcile_bluesky(store, job, d, &stamp);
    }
    if job.platform == mastodon::PLATFORM {
        return reconcile_mastodon(store, job, d, &stamp, pace);
    }
    if job.platform == linkedin::PLATFORM {
        return reconcile_linkedin(store, job, d, &stamp);
    }
    if job.platform == threads::PLATFORM {
        return reconcile_threads(store, job, d, &stamp);
    }
    if job.platform == instagram::PLATFORM {
        return reconcile_instagram(store, job, d, &stamp);
    }
    if job.platform == facebook::PLATFORM {
        return reconcile_facebook(store, job, d, &stamp);
    }
    if job.platform == x::PLATFORM {
        return reconcile_x(store, job, d, &stamp);
    }
    if job.platform == tiktok::PLATFORM {
        // TikTok's init carries no idempotency key and has no recent-posts read here: nothing is repeated.
        return Ok(output(
            job,
            "reconcile_failed",
            Some(
                "a tiktok publish whose answer was lost cannot be searched for; nothing was sent and the job stays outcome_unknown. Check the account's videos by hand, then record what you saw with publish.resolve_unknown"
                    .to_string(),
            ),
            0,
        ));
    }
    if job.platform == youtube::PLATFORM {
        // YouTube's upload carries no idempotency key and has no recent-posts read here: a lost answer could
        // hide a created video, so nothing is repeated and nothing is guessed.
        return Ok(output(
            job,
            "reconcile_failed",
            Some(
                "a youtube upload whose answer was lost cannot be searched for; nothing was sent and the job stays outcome_unknown. Check the channel's videos by hand, then record what you saw with publish.resolve_unknown"
                    .to_string(),
            ),
            0,
        ));
    }
    let spec = platform::spec(&job.platform).ok_or_else(|| {
        (
            "platform_unsupported",
            format!("no platform spec for {}", job.platform),
        )
    })?;
    let url = format!("{}{}", d.base, spec.recent_path(&job.remote_account));
    let reply = http::send(&d.client, Method::Get, &url, &d.token, None, None);
    record_reconcile_limit(store, &job, d, &reply)?;
    let body = match reply {
        Reply::Ok(body) => body,
        other => {
            return Ok(output(
                job,
                "reconcile_failed",
                Some(format!(
                    "the recent posts could not be read ({}); the job stays outcome_unknown",
                    describe(&other)
                )),
                1,
            ))
        }
    };
    let items: &[Value] = body
        .get("data")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let claimed = claimed_posts(store, &job)?;
    let want = fingerprint(&job.payload.caption);
    let mut keyed: Vec<&Value> = Vec::new();
    let mut by_text: Vec<&Value> = Vec::new();
    for item in items {
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        if claimed.iter().any(|c| c == id) {
            continue;
        }
        let key = item.get("idempotency_key").and_then(Value::as_str);
        if key == Some(job.idempotency_key.as_str()) {
            keyed.push(item);
        } else if item
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|t| fingerprint(t) == want)
        {
            by_text.push(item);
        }
    }
    let candidates = if keyed.is_empty() { &by_text } else { &keyed };
    match candidates.len() {
        0 => requeue(store, job, &stamp, 1, "no recent post matches this job"),
        1 => {
            let item = candidates[0];
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            job.remote.post_id = Some(id.clone());
            if let Some(link) = item
                .get("permalink")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                job.remote.permalink = Some(link.to_string());
            }
            advance(&mut job, "read_permalink");
            store.write_txn(&mut || store.save_job(&mut job, &stamp))?;
            Ok(output(
                job,
                "reconciled_adopted",
                Some(format!("adopted post {id}")),
                1,
            ))
        }
        n => {
            let message = format!(
                "{n} recent posts match this job; none was adopted. Check the platform by hand"
            );
            set_error(&mut job, "reconcile_ambiguous", message.clone());
            store.write_txn(&mut || store.save_job(&mut job, &stamp))?;
            Ok(output(job, "reconcile_ambiguous", Some(message), 1))
        }
    }
}
