//! `publish.resolve_unknown`: a person closes an `outcome_unknown` job by hand.
//!
//! Five providers (LinkedIn, Threads, Facebook, X, Mastodon after its key window) cannot repeat a create
//! safely, so a lost answer leaves the job `outcome_unknown` and `publish.reconcile` deliberately sends
//! nothing. This command is the way out: the person looks at the platform and records what they saw.
//!
//! - `posted` (with the platform's id of the post): the job becomes `published` at phase `done` with that
//!   id (and the permalink, when given), as a normally published job looks to every later reader.
//! - `not_posted`: the job becomes `failed` with `resolved_not_posted`, terminal. It is never requeued.
//!
//! The command makes NO network call, takes no token and no base URL. Verification of a typed id is a
//! separate, existing command. The resolution (who, when, what they checked, and that the id was typed by
//! hand) is stored in the job's `resolution_json` column (the table had no event trail), in the
//! same transaction as the state change, and shown as `resolution` by every job read.
//!
//! The author rule is `util::require_person`: `adrian`, `skill` or `daemon`, where only `adrian` is a
//! person; the other two are agents and are refused.
use serde_json::{json, Value};

use crate::calendar_time as time;
use crate::jobs::{post_id_of, PublishJobStore};
use crate::platforms::{bluesky, facebook, linkedin, mastodon, threads, x};
use crate::protocol::{
    PublishJob, PublishResolution, PublishResolveUnknownInput, PublishResolveUnknownOutput,
    PUBLISH_RESOLUTIONS,
};
use crate::util::{
    bad, invalid_transition, not_found, parse_input, require_channel, require_person, to_value,
};
use crate::Fail;

/// Longest note, in characters.
const MAX_NOTE: usize = 500;
/// Longest remote id or permalink accepted.
const MAX_TEXT: usize = 512;
/// Longest id for a platform that has no provider module.
const MAX_GENERIC_ID: usize = 128;
/// The `last_error_code` of a job a person recorded as not posted.
const NOT_POSTED_CODE: &str = "resolved_not_posted";

/// The trimmed author name when it is a person. `bad_input` for an empty or unknown name (the same
/// refusal `memory.put` gives), `author_forbidden` for an agent author (`skill`, `daemon`).
fn require_human(resolved_by: &str) -> Result<String, Fail> {
    require_person(
        "resolved_by",
        resolved_by,
        "records what is on the platform",
    )
}

/// The trimmed note: 1 to [`MAX_NOTE`] characters.
fn clean_note(note: &str) -> Result<String, Fail> {
    let note = note.trim();
    if note.is_empty() {
        return Err(bad("note must not be empty: say what you checked"));
    }
    let count = note.chars().count();
    if count > MAX_NOTE {
        return Err(bad(format!(
            "note is {count} characters; the most is {MAX_NOTE}"
        )));
    }
    Ok(note.to_string())
}

/// A plain `https://` URL, the rule the providers apply to a permalink they store.
fn check_permalink(link: &str) -> Result<(), Fail> {
    let plain = link.len() <= MAX_TEXT
        && link.len() > "https://".len()
        && link.starts_with("https://")
        && !link.chars().any(|c| c.is_whitespace() || c.is_control());
    if plain {
        Ok(())
    } else {
        Err(bad(
            "permalink must be a plain https:// URL with no spaces (at most 512 characters)",
        ))
    }
}

/// True when `id` is an id this job's provider would store as its post id. Each provider's own check is
/// run on the answer shape it parses, so the rule is the provider's, not a copy: Bluesky also requires the
/// uri to name this job's record key, Facebook the Page id of this job. A platform with no provider module
/// takes a plain id (letters, digits, `-`, `_`, `.`, `:`).
fn remote_id_ok(job: &PublishJob, id: &str) -> bool {
    if id.is_empty() || id.len() > MAX_TEXT || !id.bytes().all(|b| b.is_ascii_graphic()) {
        return false;
    }
    let platform = job.platform.as_str();
    if platform == bluesky::PLATFORM {
        bluesky::created_post_id(&job.idempotency_key, &json!({ "uri": id })).is_some()
    } else if platform == mastodon::PLATFORM {
        mastodon::created_post_id(&json!({ "id": id })).is_some()
    } else if platform == linkedin::PLATFORM {
        linkedin::created_post_id(&json!({ "headers": { "x-restli-id": id } })).is_some()
    } else if platform == threads::PLATFORM {
        threads::object_id(&json!({ "id": id })).is_some()
    } else if platform == facebook::PLATFORM {
        {
            let body = json!({ "id": id });
            facebook::created_post_id(&job.remote_account, &body, false).is_some()
                || facebook::created_post_id(&job.remote_account, &body, true).is_some()
        }
    } else if platform == x::PLATFORM {
        x::created_post_id(&json!({ "data": { "id": id } })).is_some()
    } else {
        id.len() <= MAX_GENERIC_ID
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
    }
}

/// True when another job of the same platform in the same scope already records `id` as its post.
fn claimed_elsewhere(
    store: &dyn PublishJobStore,
    job: &PublishJob,
    id: &str,
) -> Result<bool, Fail> {
    let others = store.other_remote_json(&job.scope, &job.platform, &job.job_id)?;
    Ok(others
        .iter()
        .any(|text| post_id_of(text).as_deref() == Some(id)))
}

/// What the caller asked for, after the checks that need no job.
struct Wanted {
    posted: bool,
    resolved_by: String,
    note: String,
    remote_id: Option<String>,
    permalink: Option<String>,
}

/// True when the stored resolution is exactly what `wanted` asks for (the instant is not compared: a repeat
/// at a later `now` is still the same resolution).
fn is_same(job: &PublishJob, wanted: &Wanted) -> bool {
    let Some(stored) = &job.resolution else {
        return false;
    };
    let decision = if wanted.posted {
        "posted"
    } else {
        "not_posted"
    };
    stored.decision == decision
        && stored.resolved_by == wanted.resolved_by
        && stored.note == wanted.note
        && job.remote.post_id == wanted.remote_id
        && job.remote.permalink == wanted.permalink
}

/// `publish.resolve_unknown`. Order: input shape, author, note, scope, job, an already stored resolution
/// (identical: returned unchanged; different: `invalid_transition`), state, clock, phase, remote id rules,
/// the one-post-one-job rule, then one transaction writes the state and the resolution. Every refusal
/// happens before the write, so a refusal changes nothing.
pub fn resolve_unknown(store: &dyn PublishJobStore, input: Value) -> Result<Value, Fail> {
    let req: PublishResolveUnknownInput = parse_input(input, "publish.resolve_unknown")?;
    if !PUBLISH_RESOLUTIONS.contains(&req.decision.as_str()) {
        return Err(bad(format!(
            "decision must be one of {}, got {}",
            PUBLISH_RESOLUTIONS.join(", "),
            req.decision
        )));
    }
    let posted = req.decision == "posted";
    let resolved_by = require_human(&req.resolved_by)?;
    let note = clean_note(&req.note)?;
    if posted {
        if req.remote_id.as_deref().is_none_or(str::is_empty) {
            return Err(bad(
                "remote_id is required when the decision is posted: the platform's id of the post you found",
            ));
        }
        if let Some(link) = req.permalink.as_deref() {
            check_permalink(link)?;
        }
    } else if req.remote_id.is_some() || req.permalink.is_some() {
        return Err(bad(
            "remote_id and permalink belong to a posted decision: nothing was posted, so none can be given",
        ));
    }
    let wanted = Wanted {
        posted,
        resolved_by,
        note,
        remote_id: req.remote_id.clone(),
        permalink: req.permalink.clone(),
    };
    let now = time::now_secs(req.now.as_deref())?;
    let stamp = time::format_instant(now);

    let mut output: Option<PublishResolveUnknownOutput> = None;
    store.write_txn(&mut || {
    require_channel(store, &req.scope)?;
    let mut job = store
        .load_job(&req.scope, &req.job_id)?
        .ok_or_else(|| not_found(&req.job_id))?;

    if let Some(stored) = job.resolution.as_ref() {
        if is_same(&job, &wanted) {
            output = Some(PublishResolveUnknownOutput {
                job,
                applied: false,
            });
            return Ok(());
        }
        return Err(invalid_transition(format!(
            "job {} was already resolved as {} by {}; a resolution is final, and a different one is refused. To post again, change the entry and approve the new revision",
            job.job_id, stored.decision, stored.resolved_by
        )));
    }
    if job.state != "outcome_unknown" {
        return Err(invalid_transition(format!(
            "job {} is {}: only an outcome_unknown job is resolved by hand",
            job.job_id, job.state
        )));
    }
    let updated = time::parse_instant(&job.updated_at, "updated_at")?;
    if now < updated {
        return Err((
            "now_before_update",
            format!(
                "now {stamp} is earlier than the job's last update {}: a resolution cannot be dated before what it resolves",
                job.updated_at
            ),
        ));
    }

    if let Some(known) = job.remote.post_id.as_deref() {
        if !posted {
            return Err((
                "remote_id_known",
                format!(
                    "job {} already holds post id {known}: run publish.reconcile to read it instead of recording that nothing was posted",
                    job.job_id
                ),
            ));
        }
        if Some(known) != wanted.remote_id.as_deref() {
            return Err((
                "remote_id_mismatch",
                format!(
                    "job {} already holds post id {known}, not the one given",
                    job.job_id
                ),
            ));
        }
    }
    if posted {
        if job.phase == "create_container" {
            return Err(invalid_transition(format!(
                "job {} is unsure at create_container: a container is not a post, so nothing can be live. Run publish.reconcile (it queues the one safe retry) or record not_posted",
                job.job_id
            )));
        }
        let id = wanted.remote_id.as_deref().unwrap_or_default();
        if !remote_id_ok(&job, id) {
            return Err(bad(format!(
                "remote_id {id:?} is not an id a {} post has: it must be shaped as the {} provider stores post ids",
                job.platform, job.platform
            )));
        }
        if claimed_elsewhere(store, &job, id)? {
            return Err((
                "remote_id_claimed",
                format!(
                    "another {} job in this account already records {id} as its post: one post cannot belong to two jobs",
                    job.platform
                ),
            ));
        }
    }

    job.inflight = None;
    job.next_attempt_at = None;
    if posted {
        job.state = "published".to_string();
        job.phase = "done".to_string();
        job.attempts = 0;
        job.remote.post_id = wanted.remote_id.clone();
        job.remote.permalink = wanted.permalink.clone();
        job.last_error_code = None;
        job.last_error_message = None;
    } else {
        job.state = "failed".to_string();
        job.last_error_code = Some(NOT_POSTED_CODE.to_string());
        job.last_error_message = Some(format!(
            "{} checked the platform and recorded that nothing was posted; the job is closed and is not retried",
            wanted.resolved_by
        ));
    }
    let resolution = PublishResolution {
        decision: req.decision.clone(),
        resolved_by: wanted.resolved_by.clone(),
        resolved_at: stamp.clone(),
        note: wanted.note.clone(),
        remote_id_entered_by_hand: posted,
    };
    // The resolution is a field of the job: `save` writes it with the move out of the held status.
    job.resolution = Some(resolution);
    store.save_job(&mut job, &stamp)?;
    output = Some(PublishResolveUnknownOutput { job, applied: true });
    Ok(())
    })?;
    let output =
        output.ok_or_else(|| crate::util::internal("resolve_unknown produced no output"))?;
    to_value(&output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_person_may_resolve() {
        assert_eq!(require_human(" adrian ").expect("a person"), "adrian");
        for agent in ["skill", "daemon"] {
            assert_eq!(
                require_human(agent).expect_err("an agent").0,
                "author_forbidden"
            );
        }
        assert_eq!(require_human("  ").expect_err("empty").0, "bad_input");
        assert_eq!(require_human("claude").expect_err("unknown").0, "bad_input");
    }

    #[test]
    fn the_note_is_one_to_five_hundred_characters() {
        assert_eq!(
            clean_note("  seen in the feed ").expect("note"),
            "seen in the feed"
        );
        assert!(clean_note("   ").is_err());
        assert!(clean_note(&"x".repeat(MAX_NOTE)).is_ok());
        assert!(clean_note(&"x".repeat(MAX_NOTE + 1)).is_err());
        // Characters, not bytes: 500 two-byte characters pass.
        assert!(clean_note(&"\u{e9}".repeat(MAX_NOTE)).is_ok());
    }

    #[test]
    fn a_permalink_is_a_plain_https_url() {
        assert!(check_permalink("https://www.linkedin.com/feed/update/urn:li:share:1/").is_ok());
        for bad_link in [
            "",
            "https://",
            "http://example.com/a",
            "https://example.com/a b",
            "javascript:alert(1)",
        ] {
            assert!(check_permalink(bad_link).is_err(), "{bad_link}");
        }
        assert!(check_permalink(&format!("https://e.com/{}", "a".repeat(MAX_TEXT))).is_err());
    }
}
