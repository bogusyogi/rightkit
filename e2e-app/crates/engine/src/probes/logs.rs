//! rightkit-logs (default features: http transport built, not used; no `tracing`/`testing`): a rotating owner-only JSONL
//! store, redacting diagnostics collection read back from disk, text scrub and known-secret redaction, and the consented
//! outbox (refuses with no consent, redacts what it queues, dedupes, acknowledges).
use rightkit_logs::{
    scrub_text, BundleMetadata, ConsentStore, EnqueueError, EventRecord, JsonlStore, JsonlStoreConfig, KnownSecrets, Level,
    Outbox, OutboxConfig,
};
use serde_json::{json, Value};

const SECRET_VALUE: &str = "hunter2-the-real-secret";
const EMAIL: &str = "ada.lovelace@example.org";
const APP_TOKEN: &str = "e2e-app-token-5521";

fn ensure(cond: bool, what: &str) -> Result<(), String> {
    if cond {
        Ok(())
    } else {
        Err(format!("rightkit-logs: {what}"))
    }
}

fn e<T: std::fmt::Display>(what: &str) -> impl FnOnce(T) -> String + '_ {
    move |err| format!("rightkit-logs: {what}: {err}")
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let dir = ctx.dir("logs")?;

    // Rotating, owner-only JSONL store.
    let config = JsonlStoreConfig::from_stem(&dir, "app", 600).map_err(e("from_stem"))?;
    ensure(JsonlStoreConfig::from_stem(&dir, "../x", 600).is_err(), "a path-escaping log stem was accepted")?;
    let store = JsonlStore::from_config(&config).with_generations(2).with_owner_only(true);
    let now = 1_800_000_000_000i64;
    for i in 0..30 {
        let ev = EventRecord::new(now + i, Level::Info, "tick")
            .with_field("n", json!(i))
            .with_field("api_key", json!(SECRET_VALUE))
            .with_field("note", json!(format!("failed for {EMAIL}")));
        store.append(&ev).map_err(e("append"))?;
    }
    store.append(&EventRecord::new(now + 99, Level::Error, "boom").with_field("detail", json!(format!("token {APP_TOKEN}")))).map_err(e("append error"))?;
    let st = store.status();
    ensure(st.failed_appends == 0 && st.failed_rotations == 0, &format!("store absorbed failures: {st:?}"))?;
    let files: Vec<String> = std::fs::read_dir(&dir)
        .map_err(e("read_dir"))?
        .filter_map(|x| x.ok().map(|x| x.file_name().to_string_lossy().into_owned()))
        .filter(|n| n.starts_with("app"))
        .collect();
    ensure(files.len() >= 2 && files.len() <= 3, &format!("rotation produced {files:?}, expected the live file plus up to 2 generations"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(store.path()).map_err(e("stat log"))?.permissions().mode() & 0o777;
        ensure(mode & 0o077 == 0, &format!("owner-only log has mode {mode:o}"))?;
    }
    let tail = store.tail_bytes(4096).map_err(e("tail_bytes"))?;
    ensure(String::from_utf8_lossy(&tail).contains("boom"), "the newest event is missing from the tail")?;

    // Diagnostics read back from disk are redacted by key and by value pattern.
    let bundle = config.collect(BundleMetadata {
        app_name: "RkE2E".into(),
        app_version: "0.1.0".into(),
        build: "e2e".into(),
        os: std::env::consts::OS.into(),
        settings: json!({}),
    });
    ensure(!bundle.sources.is_empty() && !bundle.sources[0].events.is_empty(), "the bundle holds no events")?;
    let dumped = serde_json::to_string(&bundle).map_err(e("serialize bundle"))?;
    for forbidden in [SECRET_VALUE, EMAIL] {
        ensure(!dumped.contains(forbidden), &format!("the bundle leaked {forbidden:?}"))?;
    }
    ensure(dumped.contains("boom"), "the error event is missing from the bundle")?;

    // Text scrub and known-secret redaction.
    let scrubbed = scrub_text(&format!("mail {EMAIL} from 10.1.2.3"));
    ensure(!scrubbed.contains(EMAIL) && !scrubbed.contains("10.1.2.3"), &format!("scrub_text left {scrubbed:?}"))?;
    let known = KnownSecrets::from_values([APP_TOKEN]);
    let redacted = known.redact(&format!("auth {APP_TOKEN} ok"));
    ensure(!redacted.contains(APP_TOKEN) && known.is_exposed(&format!("x{APP_TOKEN}")), &format!("known secret not redacted: {redacted:?}"))?;

    // Consented outbox.
    let consent = ConsentStore::new(dir.join("consent.json"));
    let outbox = Outbox::open(dir.join("outbox.json"), consent.clone(), OutboxConfig::default());
    ensure(outbox.enqueue("usage", json!({ "n": 1 })) == Err(EnqueueError::NoConsent), "an event was accepted without consent")?;
    ensure(outbox.is_empty(), "the outbox stored something without consent")?;
    consent.grant("2026-10").map_err(e("grant"))?;
    ensure(consent.is_granted(), "grant did not read back")?;
    let id = outbox
        .enqueue("usage", json!({ "api_key": SECRET_VALUE, "note": format!("for {EMAIL}"), "count": 7 }))
        .map_err(|x| format!("rightkit-logs: enqueue: {x}"))?;
    let dedupe_a = outbox.enqueue_deduped("k1", "usage", json!({ "n": 2 })).map_err(|x| format!("rightkit-logs: deduped: {x}"))?;
    let dedupe_b = outbox.enqueue_deduped("k1", "usage", json!({ "n": 2 })).map_err(|x| format!("rightkit-logs: deduped again: {x}"))?;
    ensure(dedupe_a == dedupe_b && outbox.len() == 2, "a repeated dedupe key stored a second event")?;
    let pending = outbox.pending(10, i64::MAX);
    let first = pending.iter().find(|p| p.id == id).ok_or("rightkit-logs: the queued event is not pending")?;
    let payload = first.payload.to_string();
    ensure(!payload.contains(SECRET_VALUE) && !payload.contains(EMAIL), &format!("the queued payload leaked: {payload}"))?;
    ensure(first.payload["count"] == json!(7), "the non-sensitive field was altered")?;
    // Reopen: the ledger is durable.
    let reopened = Outbox::open(dir.join("outbox.json"), consent.clone(), OutboxConfig::default());
    ensure(reopened.len() == 2, "the outbox did not persist across reopen")?;
    reopened.acknowledge(&[id, dedupe_a]).map_err(e("acknowledge"))?;
    ensure(reopened.is_empty(), "acknowledge left events")?;
    consent.revoke().map_err(e("revoke"))?;
    ensure(reopened.enqueue("usage", json!({})) == Err(EnqueueError::NoConsent), "an event was accepted after revocation")?;

    Ok(json!({ "log_files": files, "bundle_events": bundle.sources[0].events.len(), "outbox_ids": [id, dedupe_a] }))
}
