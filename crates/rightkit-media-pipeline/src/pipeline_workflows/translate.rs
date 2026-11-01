//! Translation step (OpenCreator terminology-consistent translation).
//! A provider request (`provider` field) is routed through the registered
//! text-LLM adapters (Kind::Text, e.g. fal any-llm). The model is told the
//! glossary in the prompt and the result is post-checked: source terms that
//! leaked through untranslated are deterministically replaced and counted in
//! `glossary_enforced`; glossary targets still missing afterwards are listed
//! in `glossary_violations` rather than hidden. Without a stored key the call
//! reports `route_unready` honestly. The deterministic `glossary` mode (or
//! absent provider) substitutes known terms only — it does not translate
//! sentences — and is labelled `machine_translation:false`.
//!
//! Spend: each segment sent to the text route is estimated, the whole run is
//! refused before its first call when the estimate exceeds the daily cap, and
//! each call is reserved in the spend ledger as `translate:<run id>` (attempt =
//! segment ordinal) immediately before it is sent and reconciled after it. A
//! failed segment stops the run; segments already translated are kept.
//!
//! A segment whose call has an unknown outcome stays reserved and stops the run
//! until `resolve` settles it: `release` (nothing was sent: the segment may be
//! sent again) or `abandon` (it may have run: it is never sent again and is
//! reported in `manual_text_segments`). See `resolve_uncertain`.

use serde_json::{json, Value};

use rightkit_media_gen::credentials;
use rightkit_media_gen::ledger;
use rightkit_media_gen::library::sha256_hex;
use rightkit_media_gen::providers::{
    self, GenRequest, Kind, Output, PollOutcome, Provider, SubmitOutcome,
};
use rightkit_media_gen::store;

use crate::{Context, GenError, GenResult};

/// Attempt-number stride between passes of one run. A pass that sends a
/// segment again takes attempts `pass * PASS_STRIDE + ordinal`, so it never
/// reuses a ledger key `(job_id, attempt)` written by an earlier pass.
const PASS_STRIDE: i64 = 1_000_000;

fn s(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Glossary accepted as a JSON object, a JSON-object string, or `a=b; c=d`
/// pairs (workflow inputs are strings).
fn parse_glossary(v: Option<&Value>) -> Vec<(String, String)> {
    match v {
        Some(Value::Object(m)) => m
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
            .collect(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|e| {
                let src = e.get("source").and_then(Value::as_str)?.trim();
                let dst = e.get("target").and_then(Value::as_str)?.trim();
                (!src.is_empty() && !dst.is_empty()).then(|| (src.to_string(), dst.to_string()))
            })
            .collect(),
        Some(Value::String(s)) => {
            let s = s.trim();
            if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(s) {
                return m
                    .iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                    .collect();
            }
            s.split([';', '\n'])
                .filter_map(|pair| pair.split_once('=').or_else(|| pair.split_once(':')))
                .map(|(a, b)| (a.trim().to_string(), b.trim().to_string()))
                .filter(|(a, b)| !a.is_empty() && !b.is_empty())
                .collect()
        }
        _ => Vec::new(),
    }
}

fn apply_glossary(text: &str, glossary: &[(String, String)]) -> (String, usize) {
    let mut out = text.to_string();
    let mut applied = 0usize;
    for (from, to) in glossary {
        if from.is_empty() {
            continue;
        }
        let count = out.matches(from.as_str()).count();
        if count > 0 {
            applied += count;
            out = out.replace(from.as_str(), to);
        }
    }
    (out, applied)
}

/// Terminology post-check for an LLM translation: every glossary pair whose
/// source term appears in the source text must come out as its target term.
/// Untranslated source terms are replaced (enforced); targets still missing
/// afterwards are reported.
fn enforce_glossary(
    source: &str,
    translated: &str,
    glossary: &[(String, String)],
) -> (String, usize, Vec<String>) {
    let mut out = translated.to_string();
    let mut enforced = 0usize;
    let mut violations = Vec::new();
    for (from, to) in glossary {
        if from.is_empty() || to.is_empty() || !source.contains(from.as_str()) {
            continue;
        }
        let leaked = out.matches(from.as_str()).count();
        if leaked > 0 {
            enforced += leaked;
            out = out.replace(from.as_str(), to);
        }
        if !out.contains(to.as_str()) {
            violations.push(format!("{from} -> {to}"));
        }
    }
    (out, enforced, violations)
}

/// System prompt carrying the translation contract (target language and
/// required terminology) so it survives provider model swaps.
fn translate_prompt(target: &str, glossary: &[(String, String)]) -> String {
    let mut p = format!(
        "You are a professional translator. Translate the user's text to {target}. \
         Preserve meaning, tone and any markup. Reply with the translation only."
    );
    if !glossary.is_empty() {
        let terms = glossary
            .iter()
            .map(|(a, b)| format!("{a} = {b}"))
            .collect::<Vec<_>>()
            .join("; ");
        p.push_str(&format!(
            " Required terminology — use these translations verbatim: {terms}."
        ));
    }
    p
}

/// How the spend ledger settles a call that did not return text.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Charge {
    /// Rejected before any job ran, or the job failed: nothing is owed.
    Nothing,
    /// The provider completed the job: the estimate is owed.
    Estimate,
    /// Outcome unknown (ambiguous submit, lost poll, timeout): the reservation
    /// stays open, so the possible charge stays counted.
    Unknown,
}

struct CallFailure {
    error: GenError,
    charge: Charge,
}

/// Drive one text request through a provider adapter to a terminal state.
/// Bounded polling (90 × 1 s): text routes are short-lived queue jobs.
fn run_text(p: &dyn Provider, req: &GenRequest, key: &str) -> Result<String, CallFailure> {
    let fail = |e: providers::ProviderError, charge: Charge| CallFailure {
        error: GenError::new(e.code, e.message),
        charge,
    };
    let submit = p.submit(req, key).map_err(|e| {
        let charge = if e.ambiguous {
            Charge::Unknown
        } else {
            Charge::Nothing
        };
        fail(e, charge)
    })?;
    let outputs = match submit {
        SubmitOutcome::Completed { outputs } => outputs,
        SubmitOutcome::Accepted { provider_job_id } => {
            let mut done = None;
            for _ in 0..90 {
                match p
                    .poll(req, &provider_job_id, key)
                    .map_err(|e| fail(e, Charge::Unknown))?
                {
                    PollOutcome::Running => std::thread::sleep(std::time::Duration::from_secs(1)),
                    PollOutcome::Completed { outputs } => {
                        done = Some(outputs);
                        break;
                    }
                    PollOutcome::Failed { message } => {
                        return Err(CallFailure {
                            error: GenError::new("provider_failed", message),
                            charge: Charge::Nothing,
                        });
                    }
                    // The provider answered with something this adapter cannot classify: it may
                    // still run or bill. Held like any other unknown outcome (reservation stays
                    // open); when the provider says it finished, its estimate is owed.
                    PollOutcome::Unknown {
                        status,
                        detail,
                        completed,
                    } => {
                        return Err(CallFailure {
                            error: GenError::new(
                                "outcome_unknown",
                                format!("{} answered text job {provider_job_id} with {status:?}, which is not a recognised outcome. Provider reply: {detail}", p.id()),
                            ),
                            charge: if completed { Charge::Estimate } else { Charge::Unknown },
                        });
                    }
                }
            }
            done.ok_or_else(|| CallFailure {
                error: GenError::new("timeout", "text job did not complete in 90 s"),
                charge: Charge::Unknown,
            })?
        }
    };
    // The provider ran the job from here on, so its estimate is owed even when
    // the output cannot be used.
    let owed = |error: GenError| CallFailure {
        error,
        charge: Charge::Estimate,
    };
    for o in &outputs {
        let (bytes, _mime) = p
            .fetch(o)
            .map_err(|e| owed(GenError::new(e.code, e.message)))?;
        if let Ok(t) = String::from_utf8(bytes) {
            if !t.trim().is_empty() {
                return Ok(t.trim().to_string());
            }
        }
        if let Output::Url { url, .. } = o {
            return Err(owed(GenError::new(
                "bad_response",
                format!("text route returned non-text output {url}"),
            )));
        }
    }
    Err(owed(GenError::new(
        "bad_response",
        "text route returned no output",
    )))
}

/// The paid text route a run sends segments through. The real route wraps a
/// provider adapter; tests inject outcomes.
trait TextRoute {
    fn provider_id(&self) -> &str;
    fn model(&self) -> &str;
    /// Advisory price of one request in USD, reserved before its call.
    fn estimate(&self, req: &GenRequest) -> GenResult<f64>;
    fn call(&self, req: &GenRequest) -> Result<String, CallFailure>;
}

struct ProviderRoute<'a> {
    provider: &'a dyn Provider,
    model: String,
    key: String,
}

impl TextRoute for ProviderRoute<'_> {
    fn provider_id(&self) -> &str {
        self.provider.id()
    }
    fn model(&self) -> &str {
        &self.model
    }
    fn estimate(&self, req: &GenRequest) -> GenResult<f64> {
        self.provider
            .estimate_usd(req)
            .map_err(|e| GenError::new(e.code, e.message))
    }
    fn call(&self, req: &GenRequest) -> Result<String, CallFailure> {
        run_text(self.provider, req, &self.key)
    }
}

/// One translation run: its terminology contract, provider parameters, ledger
/// job id, and the earlier partial result whose finished segments are kept.
struct Job<'a> {
    target: &'a str,
    glossary: &'a [(String, String)],
    params: &'a Value,
    /// Ledger job id, `translate:<run id>`.
    id: &'a str,
    previous: Option<&'a Value>,
}

/// How one segment is answered before any call is made.
enum Step {
    /// Final text and its glossary hits; no call and no ledger entry.
    Final(String, usize),
    /// Needs one paid call; `est` is filled in before the first call.
    Send { req: GenRequest, est: f64 },
    /// An earlier call for this segment had an unknown outcome and is still
    /// open in the ledger. It is not re-sent, and the run stops here until
    /// `resolve` settles it.
    Blocked,
    /// An earlier call was abandoned by `resolve`: it ran or may have, so it is
    /// never sent again. The segment stays untranslated and is reported as
    /// needing manual text; the run continues past it.
    Manual,
}

/// Translates `items` (objects carrying `text`) and returns the result payload
/// with one object per item. `route` None is the glossary-only path: terms are
/// substituted, no call is made and no ledger entry is written.
fn run_items(
    job: &Job,
    items: &[Value],
    route: Option<&dyn TextRoute>,
    db: Option<&mut store::Db>,
) -> GenResult<Value> {
    let srcs: Vec<String> = items.iter().map(|seg| s(seg, "text")).collect();
    let n = items.len();
    let system = translate_prompt(job.target, job.glossary);
    let request_key = route
        .map(|r| sha256_hex(format!("{}\n{}\n{system}", r.provider_id(), r.model()).as_bytes()));
    let mut texts = srcs.clone();
    let mut applied = vec![0usize; n];
    let mut translated = vec![false; n];
    let mut attempts: Vec<Option<i64>> = vec![None; n];
    let mut uncertain = vec![false; n];
    let mut manual = vec![false; n];
    let (mut enforced, mut violations) = (0usize, Vec::<String>::new());
    let (mut est_usd, mut stop) = (0.0_f64, None::<(usize, GenError)>);

    match route {
        None => {
            for (i, src) in srcs.iter().enumerate() {
                let (t, k) = apply_glossary(src, job.glossary);
                texts[i] = t;
                applied[i] = k;
            }
        }
        Some(route) => {
            let db = db.ok_or_else(|| {
                GenError::new(
                    "invalid_input",
                    "a provider translation needs the spend ledger",
                )
            })?;

            // A previous result is reused only for the same route, prompt and
            // source text.
            let previous = job
                .previous
                .filter(|p| p["request_key"].as_str() == request_key.as_deref());
            let prev_segs: &[Value] = previous
                .and_then(|p| p["segments"].as_array())
                .map(Vec::as_slice)
                .unwrap_or(&[]);

            let mut steps: Vec<Step> = Vec::with_capacity(n);
            for (i, src) in srcs.iter().enumerate() {
                let prior = prev_segs.get(i).filter(|p| s(p, "text_source") == *src);
                // Uncertainty is read from the ledger below, never from the caller's
                // payload: a `resolve` that released the reservation must take effect
                // even when the caller passes its previous partial result.
                let step = match prior {
                    Some(p) if p["translated"].as_bool() == Some(true) => Step::Final(
                        s(p, "text"),
                        p["glossary_terms_applied"].as_u64().unwrap_or(0) as usize,
                    ),
                    _ if src.trim().is_empty() => Step::Final(src.clone(), 0),
                    _ => match job.glossary.iter().find(|(from, _)| from == src) {
                        Some((_, to)) => Step::Final(to.clone(), 1),
                        None => {
                            let mut params = job.params.clone();
                            params["system_prompt"] = json!(system);
                            Step::Send {
                                req: GenRequest {
                                    kind: Kind::Text,
                                    provider: route.provider_id().to_string(),
                                    model: route.model().to_string(),
                                    prompt: src.clone(),
                                    params,
                                },
                                est: 0.0,
                            }
                        }
                    },
                };
                steps.push(step);
            }

            // A segment whose earlier call is still open in the ledger (reserved, never
            // reconciled: its outcome is unknown) is not sent again, even when the caller
            // passes no `previous` payload. Attempts are `pass * PASS_STRIDE + ordinal`.
            // An abandoned segment is never sent again (`Manual`); a segment whose call is
            // still open in the ledger (reserved, never reconciled: outcome unknown) is not
            // sent again either (`Blocked`), even when the caller passes no `previous`
            // payload. Attempts are `pass * PASS_STRIDE + ordinal`.
            let open = ledger::open_attempts(&*db, job.id)?;
            let abandoned = ledger::abandoned_ordinals(&*db, job.id)?;
            for (i, step) in steps.iter_mut().enumerate() {
                let ordinal = i as i64 + 1;
                if !matches!(step, Step::Send { .. }) {
                    continue;
                }
                if abandoned.contains(&ordinal) {
                    *step = Step::Manual;
                } else if open.iter().any(|a| a.rem_euclid(PASS_STRIDE) == ordinal) {
                    *step = Step::Blocked;
                }
            }

            // Estimate every segment that will be sent, up to the first blocked one.
            let (mut total, mut sent) = (0.0_f64, 0usize);
            for step in steps.iter_mut() {
                match step {
                    Step::Blocked => break,
                    Step::Send { req, est } => {
                        *est = route.estimate(req)?;
                        total += *est;
                        sent += 1;
                    }
                    Step::Final(..) | Step::Manual => {}
                }
            }

            // Refuse the whole run before any call when the cap cannot cover it.
            if sent > 0 {
                if let Err(e) = ledger::check(&*db, total) {
                    if e.code != "spend_cap" {
                        return Err(e);
                    }
                    let st = ledger::status(&*db)?;
                    let cap = st["cap_usd"].as_f64().unwrap_or(0.0);
                    let used = st["reserved_usd"].as_f64().unwrap_or(0.0)
                        + st["actual_usd"].as_f64().unwrap_or(0.0);
                    return Err(GenError::new(
                        "spend_cap",
                        format!(
                            "translation refused before any call: {sent} segments need an estimated ${total:.4}, remaining daily allowance is ${:.4} (cap ${cap:.2})",
                            (cap - used).max(0.0)
                        ),
                    ));
                }
            }

            // Pick the first pass whose attempt numbers are all free for the segments sent.
            let sends: Vec<usize> = steps
                .iter()
                .enumerate()
                .take_while(|(_, st)| !matches!(st, Step::Blocked))
                .filter(|(_, st)| matches!(st, Step::Send { .. }))
                .map(|(i, _)| i)
                .collect();
            let mut pass = 0_i64;
            'pass: loop {
                for &i in &sends {
                    if ledger::attempt_reserved(&*db, job.id, pass * PASS_STRIDE + i as i64 + 1)?
                        .is_some()
                    {
                        pass += 1;
                        continue 'pass;
                    }
                }
                break;
            }

            for (i, step) in steps.iter().enumerate() {
                match step {
                    Step::Final(text, k) => {
                        texts[i] = text.clone();
                        applied[i] = *k;
                        translated[i] = true;
                    }
                    Step::Manual => {
                        // No invented text: the segment stays empty until a person supplies it.
                        texts[i] = String::new();
                        manual[i] = true;
                    }
                    Step::Blocked => {
                        uncertain[i] = true;
                        stop = Some((
                            i,
                            GenError::new(
                                "uncertain_submit",
                                format!(
                                    "segment {} was sent before and its outcome is unknown; its reservation stays counted and it is not re-sent",
                                    i + 1
                                ),
                            ),
                        ));
                        break;
                    }
                    Step::Send { req, est } => {
                        let attempt = pass * PASS_STRIDE + i as i64 + 1;
                        if let Err(e) = ledger::reserve(db, job.id, attempt, *est) {
                            stop = Some((i, e));
                            break;
                        }
                        attempts[i] = Some(attempt);
                        est_usd += *est;
                        match route.call(req) {
                            Ok(raw) => {
                                let (t, hits, violated) =
                                    enforce_glossary(&srcs[i], &raw, job.glossary);
                                texts[i] = t;
                                translated[i] = true;
                                enforced += hits;
                                violations.extend(violated);
                                if let Err(e) = ledger::reconcile(db, job.id, attempt, *est) {
                                    stop = Some((i, e));
                                    break;
                                }
                            }
                            Err(f) => {
                                match f.charge {
                                    Charge::Nothing => {
                                        let _ = ledger::reconcile(db, job.id, attempt, 0.0);
                                    }
                                    Charge::Estimate => {
                                        let _ = ledger::reconcile(db, job.id, attempt, *est);
                                    }
                                    Charge::Unknown => uncertain[i] = true,
                                }
                                stop = Some((i, f.error));
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    let mut segments = Vec::with_capacity(n);
    for (i, seg) in items.iter().enumerate() {
        let mut o = seg.clone();
        o["text_source"] = json!(srcs[i]);
        o["text"] = json!(texts[i]);
        o["glossary_terms_applied"] = json!(applied[i]);
        o["translated"] = json!(translated[i]);
        o["attempt"] = json!(attempts[i]);
        o["uncertain"] = json!(uncertain[i]);
        o["needs_manual_text"] = json!(manual[i]);
        segments.push(o);
    }
    let manual_text: Vec<usize> = manual
        .iter()
        .enumerate()
        .filter(|(_, m)| **m)
        .map(|(i, _)| i + 1)
        .collect();
    let translated_count = translated.iter().filter(|t| **t).count();
    let failed_at = stop.as_ref().map(|(i, _)| *i + 1);
    let complete = route.is_some() && failed_at.is_none() && translated_count == n;
    let llm = route.map(|r| (r.provider_id().to_string(), r.model().to_string()));
    Ok(json!({
        "method": if route.is_some() { "llm" } else { "glossary" },
        "provider": llm.as_ref().map(|l| l.0.clone()),
        "model": llm.as_ref().map(|l| l.1.clone()),
        "machine_translation": route.is_some(),
        "complete": complete,
        "note": if route.is_some() {
            Value::Null
        } else {
            json!("Terminology substitution only; pass provider=<text provider> with a stored key for sentence translation.")
        },
        "target_lang": job.target,
        "terms_applied": applied.iter().sum::<usize>(),
        "glossary_enforced": enforced,
        "glossary_violations": violations,
        "translated_count": translated_count,
        "manual_text_segments": manual_text,
        "failed_at": failed_at,
        "error": stop.map(|(_, e)| json!({"code": e.code, "message": e.message})),
        "est_usd": est_usd,
        "reservation_id": route.map(|_| job.id),
        "request_key": request_key,
        "segments": segments,
    }))
}

/// `studio.pipelines.translate` `{text|segments, target_lang, glossary?,
/// provider?, model?, params?, mode?, run_id?, previous?}` → translated
/// text/segments + honesty metadata. See `run_items` for the spend contract.
pub fn translate(ctx: &Context, input: &Value) -> GenResult<Value> {
    if input.get("resolve").is_some_and(|r| !r.is_null()) {
        return resolve_uncertain(ctx, input);
    }
    translate_with(ctx, &providers::registry(), input)
}

/// `studio.pipelines.translate` `{run_id, resolve: "release"|"abandon", segment?}`: settles the
/// open reservation(s) a stopped run left for its segments (`segment` is a 1-based ordinal; omitted
/// settles every open reservation of the run).
///
/// - `release`: the provider did not run the call. The reservation goes to zero, and a rerun may
///   send the segment again (on its next pass).
/// - `abandon`: the call ran or may have. The reservation is settled as spent at its estimate, and
///   the segment is marked so no rerun sends it again; the rerun reports it in
///   `manual_text_segments` with empty text. No translation is invented.
pub fn resolve_uncertain(ctx: &Context, input: &Value) -> GenResult<Value> {
    let action = s(input, "resolve");
    if action != "release" && action != "abandon" {
        return Err(GenError::new(
            "invalid_input",
            "resolve must be \"release\" or \"abandon\"",
        ));
    }
    let run_id = s(input, "run_id");
    if run_id.is_empty() {
        return Err(GenError::new("invalid_input", "resolve needs run_id"));
    }
    let segment = match input.get("segment") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_i64().filter(|n| *n >= 1).ok_or_else(|| {
            GenError::new("invalid_input", "segment must be a 1-based segment number")
        })?),
    };
    let job_id = format!("translate:{run_id}");
    let mut db = store::open(ctx)?;
    let targets: Vec<i64> = ledger::open_attempts(&db, &job_id)?
        .into_iter()
        .filter(|a| segment.is_none_or(|n| a.rem_euclid(PASS_STRIDE) == n))
        .collect();
    if targets.is_empty() {
        return Err(GenError::new(
            "invalid_input",
            format!(
                "run {run_id} has no open reservation{}",
                segment
                    .map(|n| format!(" for segment {n}"))
                    .unwrap_or_default()
            ),
        ));
    }
    let mut settled = Vec::with_capacity(targets.len());
    for attempt in targets {
        let reserved = ledger::attempt_reserved(&db, &job_id, attempt)?.unwrap_or(0.0);
        let ordinal = attempt.rem_euclid(PASS_STRIDE);
        if action == "release" {
            ledger::reconcile(&mut db, &job_id, attempt, 0.0)?;
        } else {
            ledger::abandon(&mut db, &job_id, attempt, ordinal, reserved)?;
        }
        settled.push(json!({"segment": ordinal, "attempt": attempt, "reserved_usd": reserved, "resolve": action}));
    }
    Ok(json!({
        "run_id": run_id,
        "resolve": action,
        "settled": settled,
        "spend": ledger::status(&db)?,
    }))
}

pub fn translate_with(
    ctx: &Context,
    registry: &[Box<dyn Provider>],
    input: &Value,
) -> GenResult<Value> {
    let target = s(input, "target_lang");
    if target.is_empty() {
        return Err(GenError::new("invalid_input", "missing field: target_lang"));
    }
    let glossary = parse_glossary(input.get("glossary"));

    let provider_id = s(input, "provider");
    let mode = s(input, "mode");
    if !mode.is_empty() && mode != "glossary" {
        return Err(GenError::new(
            "invalid_input",
            format!("unknown mode: {mode} (supported: glossary)"),
        ));
    }

    let is_segments = input.get("segments").and_then(Value::as_array).is_some();
    let items: Vec<Value> = match input.get("segments").and_then(Value::as_array) {
        Some(segs) => segs.clone(),
        None => {
            let text = s(input, "text");
            if text.is_empty() {
                return Err(GenError::new(
                    "invalid_input",
                    "translate needs text or segments",
                ));
            }
            vec![json!({ "text": text })]
        }
    };

    let llm = if provider_id.is_empty() {
        None
    } else {
        // Resolve the adapter and its text route. `route_unready` always
        // names the providers that can serve text so callers can recover.
        let text_providers: Vec<String> = registry
            .iter()
            .filter(|p| p.models().iter().any(|m| m.kind == Kind::Text))
            .map(|p| p.id().to_string())
            .collect();
        let p = registry
            .iter()
            .find(|p| p.id() == provider_id)
            .ok_or_else(|| {
                GenError::new(
                    "route_unready",
                    format!(
                        "unknown text-LLM provider '{provider_id}' (text-capable: {})",
                        if text_providers.is_empty() {
                            "none".into()
                        } else {
                            text_providers.join(", ")
                        }
                    ),
                )
            })?;
        let want = s(input, "model");
        let model = p
            .models()
            .iter()
            .find(|m| m.kind == Kind::Text && (want.is_empty() || m.model == want))
            .ok_or_else(|| {
                GenError::new(
                    "route_unready",
                    format!(
                        "provider '{provider_id}' has no text model {want:?} (text-capable: {})",
                        text_providers.join(", ")
                    ),
                )
            })?;
        let key = if p.needs_key() {
            // A credential-store failure (e.g. disabled keychain) is the same
            // honest outcome as a missing key: the route is unready.
            credentials::get(ctx, p.id())
                .ok()
                .flatten()
                .ok_or_else(|| {
                    GenError::new(
                        "route_unready",
                        format!(
                            "no API key stored for '{}' (text-capable: {})",
                            p.id(),
                            text_providers.join(", ")
                        ),
                    )
                })?
        } else {
            String::new()
        };
        Some(ProviderRoute {
            provider: &**p,
            model: model.model.to_string(),
            key,
        })
    };
    let route = llm.as_ref().map(|r| r as &dyn TextRoute);

    // Stable across re-runs of the same work: the pipeline run id when given,
    // else a hash of the target and the source text.
    let run_id = {
        let id = s(input, "run_id");
        if id.is_empty() {
            let joined = items
                .iter()
                .map(|i| s(i, "text"))
                .collect::<Vec<_>>()
                .join("\n");
            let digest = sha256_hex(format!("{target}\n{joined}").as_bytes());
            format!("src-{}", &digest[..16])
        } else {
            id
        }
    };
    let job_id = format!("translate:{run_id}");
    let params = input.get("params").cloned().unwrap_or_else(|| json!({}));
    let job = Job {
        target: target.as_str(),
        glossary: glossary.as_slice(),
        params: &params,
        id: job_id.as_str(),
        previous: input.get("previous").filter(|p| p.is_object()),
    };
    let mut db = if llm.is_some() {
        Some(store::open(ctx)?)
    } else {
        None
    };
    let mut out = run_items(&job, &items, route, db.as_mut())?;

    if !is_segments {
        let first = out["segments"][0].clone();
        out["text"] = first["text"].clone();
        out["text_source"] = first["text_source"].clone();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::path::PathBuf;

    use super::*;

    const JOB: &str = "translate:test-run";

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir()
                .join(format!("rightkit-translate-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(p.join("data")).unwrap();
            Dir(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    type Scripted = Result<String, CallFailure>;

    /// Fake route: a fixed estimate, scripted outcomes in call order (calls
    /// after the script succeed), and a count of calls made.
    struct FakeRoute {
        est: f64,
        script: RefCell<VecDeque<Scripted>>,
        calls: Cell<usize>,
    }

    impl FakeRoute {
        fn new(est: f64, script: Vec<Scripted>) -> Self {
            Self {
                est,
                script: RefCell::new(script.into()),
                calls: Cell::new(0),
            }
        }
    }

    impl TextRoute for FakeRoute {
        fn provider_id(&self) -> &str {
            "fake"
        }
        fn model(&self) -> &str {
            "fake-model"
        }
        fn estimate(&self, _req: &GenRequest) -> GenResult<f64> {
            Ok(self.est)
        }
        fn call(&self, req: &GenRequest) -> Result<String, CallFailure> {
            self.calls.set(self.calls.get() + 1);
            self.script
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| Ok(format!("T:{}", req.prompt)))
        }
    }

    fn fail(code: &str, charge: Charge) -> Scripted {
        Err(CallFailure {
            error: GenError::new(code, "scripted failure"),
            charge,
        })
    }

    fn segs(texts: &[&str]) -> Vec<Value> {
        texts.iter().map(|t| json!({ "text": t })).collect()
    }

    fn ctx(dir: &Dir) -> Context {
        Context::new(dir.0.join("data"))
    }

    fn run(
        db: &mut store::Db,
        route: &dyn TextRoute,
        items: &[Value],
        glossary: &[(String, String)],
        previous: Option<&Value>,
    ) -> GenResult<Value> {
        let params = json!({});
        let job = Job {
            target: "fr",
            glossary,
            params: &params,
            id: JOB,
            previous,
        };
        run_items(&job, items, Some(route), Some(db))
    }

    fn reserved(db: &store::Db, attempt: i64) -> Option<f64> {
        ledger::attempt_reserved(db, JOB, attempt).unwrap()
    }

    /// `(reserved but unreconciled, actual)` for today.
    fn totals(db: &store::Db) -> (f64, f64) {
        let st = ledger::status(db).unwrap();
        (
            st["reserved_usd"].as_f64().unwrap(),
            st["actual_usd"].as_f64().unwrap(),
        )
    }

    #[test]
    fn cap_below_estimate_refuses_before_any_call() {
        let dir = Dir::new("cap");
        let mut db = store::open(&ctx(&dir)).unwrap();
        ledger::set_cap(&db, 0.025).unwrap();
        let route = FakeRoute::new(0.01, vec![]);
        let err = run(&mut db, &route, &segs(&["a", "b", "c"]), &[], None).unwrap_err();
        assert_eq!(err.code, "spend_cap");
        assert!(err.message.contains("3 segments"), "{}", err.message);
        assert!(err.message.contains("remaining"), "{}", err.message);
        assert_eq!(route.calls.get(), 0);
        assert_eq!(reserved(&db, 1), None);

        ledger::set_cap(&db, 0.0).unwrap();
        let err = run(&mut db, &route, &segs(&["a"]), &[], None).unwrap_err();
        assert_eq!(err.code, "spend_cap");
        assert_eq!(route.calls.get(), 0);
    }

    #[test]
    fn three_successes_reserve_and_settle_each_segment() {
        let dir = Dir::new("ok");
        let mut db = store::open(&ctx(&dir)).unwrap();
        let route = FakeRoute::new(0.01, vec![]);
        let out = run(&mut db, &route, &segs(&["a", "b", "c"]), &[], None).unwrap();
        assert_eq!(out["complete"], json!(true));
        assert_eq!(out["translated_count"], json!(3));
        assert!(out["failed_at"].is_null());
        assert!(out["error"].is_null());
        assert_eq!(out["segments"][1]["text"], json!("T:b"));
        assert_eq!(route.calls.get(), 3);
        for attempt in 1..=3 {
            assert_eq!(reserved(&db, attempt), Some(0.01));
        }
        let (open, actual) = totals(&db);
        assert!(open.abs() < 1e-9);
        assert!((actual - 0.03).abs() < 1e-9);
    }

    #[test]
    fn definite_failure_keeps_paid_segments_and_stops() {
        let dir = Dir::new("fail");
        let mut db = store::open(&ctx(&dir)).unwrap();
        let route = FakeRoute::new(
            0.01,
            vec![Ok("A".to_string()), fail("rejected", Charge::Nothing)],
        );
        let out = run(&mut db, &route, &segs(&["a", "b", "c"]), &[], None).unwrap();
        assert_eq!(out["complete"], json!(false));
        assert_eq!(out["translated_count"], json!(1));
        assert_eq!(out["failed_at"], json!(2));
        assert_eq!(out["error"]["code"], json!("rejected"));
        assert_eq!(out["segments"][0]["text"], json!("A"));
        assert_eq!(out["segments"][1]["translated"], json!(false));
        assert_eq!(route.calls.get(), 2);
        assert_eq!(reserved(&db, 1), Some(0.01));
        assert_eq!(reserved(&db, 2), Some(0.01));
        assert_eq!(reserved(&db, 3), None);
        let (open, actual) = totals(&db);
        assert!(open.abs() < 1e-9);
        assert!((actual - 0.01).abs() < 1e-9);
    }

    #[test]
    fn ambiguous_submit_stays_reserved_and_is_not_retried() {
        let dir = Dir::new("ambiguous");
        let mut db = store::open(&ctx(&dir)).unwrap();
        let route = FakeRoute::new(
            0.01,
            vec![
                Ok("A".to_string()),
                fail("ambiguous_submit", Charge::Unknown),
            ],
        );
        let out = run(&mut db, &route, &segs(&["a", "b", "c"]), &[], None).unwrap();
        assert_eq!(out["complete"], json!(false));
        assert_eq!(out["failed_at"], json!(2));
        assert_eq!(out["segments"][1]["uncertain"], json!(true));
        assert_eq!(route.calls.get(), 2);
        assert_eq!(reserved(&db, 2), Some(0.01));
        assert_eq!(reserved(&db, 3), None);
        let (open, actual) = totals(&db);
        assert!((open - 0.01).abs() < 1e-9);
        assert!((actual - 0.01).abs() < 1e-9);
    }

    #[test]
    fn rerun_with_partial_payload_sends_only_missing_segments() {
        let dir = Dir::new("rerun");
        let mut db = store::open(&ctx(&dir)).unwrap();
        let first_route = FakeRoute::new(
            0.01,
            vec![Ok("A".to_string()), fail("rejected", Charge::Nothing)],
        );
        let first = run(&mut db, &first_route, &segs(&["a", "b", "c"]), &[], None).unwrap();
        assert_eq!(first["failed_at"], json!(2));

        let second_route = FakeRoute::new(0.01, vec![]);
        let second = run(
            &mut db,
            &second_route,
            &segs(&["a", "b", "c"]),
            &[],
            Some(&first),
        )
        .unwrap();
        assert_eq!(second["complete"], json!(true));
        assert_eq!(second["segments"][0]["text"], json!("A"));
        assert_eq!(second_route.calls.get(), 2);
        assert_eq!(reserved(&db, PASS_STRIDE + 1), None);
        assert_eq!(reserved(&db, PASS_STRIDE + 2), Some(0.01));
        assert_eq!(reserved(&db, PASS_STRIDE + 3), Some(0.01));
        let (open, actual) = totals(&db);
        assert!(open.abs() < 1e-9);
        assert!((actual - 0.03).abs() < 1e-9);
    }

    #[test]
    fn glossary_answered_and_empty_segments_are_not_sent() {
        let dir = Dir::new("glossary");
        let mut db = store::open(&ctx(&dir)).unwrap();
        let glossary = vec![("Hello".to_string(), "Bonjour".to_string())];
        let route = FakeRoute::new(0.01, vec![]);
        let out = run(
            &mut db,
            &route,
            &segs(&["Hello", "Hello world", "  "]),
            &glossary,
            None,
        )
        .unwrap();
        assert_eq!(out["complete"], json!(true));
        assert_eq!(out["segments"][0]["text"], json!("Bonjour"));
        assert_eq!(out["segments"][2]["text"], json!("  "));
        assert_eq!(route.calls.get(), 1);
        assert_eq!(reserved(&db, 1), None);
        assert_eq!(reserved(&db, 2), Some(0.01));
        assert_eq!(reserved(&db, 3), None);
    }
}
