//! rightkit-speech-client: the shared speech service's discovery and host election in an isolated suite: no live record in an empty
//! suite, a host plan for this role, a takeover request written, read and cleared, and a connect to an absent service refused.
//! Transcription itself needs the running speech host (HeardRight); the app-level e2e covers it when that host is in the run.
use rightkit_speech_client::{
    clear_takeover_if_ours, connect, plan_host, read_live_record, read_takeover, request_takeover,
    HostRole,
};
use serde_json::{json, Value};

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let suite = ctx.dir("speech_client")?;
    if read_live_record(&suite).is_some() {
        return Err("an empty suite reported a live speech service".into());
    }
    let plan = plan_host(&suite, HostRole::CutRight);
    request_takeover(&suite, HostRole::CutRight).map_err(|e| format!("request takeover: {e}"))?;
    let takeover =
        read_takeover(&suite).ok_or("a takeover request that was written cannot be read")?;
    clear_takeover_if_ours(&suite);
    let cleared = read_takeover(&suite).is_none();
    let absent_refused = connect(&suite).is_err();
    if !cleared || !absent_refused {
        return Err(format!(
            "takeover cleared {cleared}, connect to an absent service refused {absent_refused}"
        ));
    }
    Ok(
        json!({ "plan": format!("{plan:?}"), "takeover": format!("{takeover:?}"), "cleared": cleared, "absent_refused": absent_refused }),
    )
}
