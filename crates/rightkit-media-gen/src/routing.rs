//! Request-level routing from saved preferences (PENDING 56; execution plan G2).
//!
//! A request that names no provider and model is routed here: the saved
//! selection for its kind (`studio.models.route.set`, stored under
//! [`SELECTIONS_KEY`]) and the saved routing preferences (stored under
//! [`PREFERENCES_KEY`] in the same settings table) pick one route, and every
//! other route is returned with the reason it was not picked.
//!
//! Four states are kept distinct per candidate route:
//!
//! - `configured`: a key is stored (or in the environment), or none is needed;
//! - `installed`: a keyless local route reports its model or tool present
//!   (`Provider::runtime_ready`); `null` when the adapter has no check;
//! - `qualified`: the catalogue entry exists and its price is no older than
//!   [`STALE_AFTER_DAYS`]; `null` when the model has no dated catalogue entry
//!   (a static adapter model: nothing is claimed either way);
//! - `usable`: configured, not known uninstalled, not known unqualified, and
//!   the estimate is not refused by the daily or monthly spend cap.
//!
//! Rules (see [`decide`]):
//!
//! 1. A request that names provider and model is never overridden; routing
//!    only reports the alternatives, and the named route's own blockers.
//! 2. A saved selection for the kind is honoured when usable; otherwise the
//!    result says why and falls to the next rule.
//! 3. `privacy: local_only` excludes cloud routes; they are never used
//!    silently. `spend: free_only` excludes every route that spends money.
//! 4. A paid route is never chosen unless the request, a saved selection or
//!    the `spend: cheapest` preference names paid spending; otherwise the
//!    result is `route_choice_required` with the alternatives and reasons.
//! 5. Ties are broken by estimate, then provider id, then model id.
//!
//! `quality` and `time` are stored and echoed but do not order routes: the
//! catalogue holds no quality or latency facts to order them by.

use crate::catalogue::{self, Catalogue, STALE_AFTER_DAYS};
use crate::providers::{GenRequest, Kind, Provider};
use crate::{credentials, ledger, store, Context, GenError, GenResult};
use serde_json::{json, Value};
use std::cmp::Ordering;

/// Settings key `route_set` writes: `{capability: {route, model, revision, set_at}}`.
pub const SELECTIONS_KEY: &str = "models.routes";
/// Settings key for the routing preferences written by `media route-prefs`.
pub const PREFERENCES_KEY: &str = "routing.preferences";

pub const PRIVACY_VALUES: &[&str] = &["any", "local_only"];
pub const SPEND_VALUES: &[&str] = &["ask", "free_only", "cheapest"];
pub const QUALITY_VALUES: &[&str] = &["best", "balanced", "economy"];
pub const TIME_VALUES: &[&str] = &["fastest", "balanced", "relaxed"];

// ------------------------------------------------------------------ preferences

/// Saved routing preferences. Defaults: any, ask, balanced, balanced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preferences {
    /// `any` | `local_only` (cloud routes excluded).
    pub privacy: String,
    /// `ask` (paid spend needs a choice) | `free_only` | `cheapest` (the cheapest usable paid route may be chosen).
    pub spend: String,
    /// `best` | `balanced` | `economy`. Stored; does not order routes.
    pub quality: String,
    /// `fastest` | `balanced` | `relaxed`. Stored; does not order routes.
    pub time: String,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            privacy: "any".into(),
            spend: "ask".into(),
            quality: "balanced".into(),
            time: "balanced".into(),
        }
    }
}

impl Preferences {
    /// Parse `{privacy?, spend?, quality?, time?}` over `base`; unknown keys and values are refused.
    pub fn merged(base: &Preferences, v: &Value) -> GenResult<Preferences> {
        let mut out = base.clone();
        let Some(o) = v.as_object() else {
            return Err(GenError::new(
                "invalid_input",
                "routing preferences must be an object",
            ));
        };
        for (key, val) in o {
            let (slot, allowed): (&mut String, &[&str]) = match key.as_str() {
                "privacy" => (&mut out.privacy, PRIVACY_VALUES),
                "spend" => (&mut out.spend, SPEND_VALUES),
                "quality" => (&mut out.quality, QUALITY_VALUES),
                "time" => (&mut out.time, TIME_VALUES),
                other => {
                    return Err(GenError::new(
                        "invalid_input",
                        format!("unknown routing preference {other:?}; accepted: privacy, spend, quality, time"),
                    ))
                }
            };
            match val.as_str().filter(|s| allowed.contains(s)) {
                Some(s) => *slot = s.to_string(),
                None => {
                    return Err(GenError::new(
                        "invalid_input",
                        format!("{key} must be one of {allowed:?}"),
                    ))
                }
            }
        }
        Ok(out)
    }

    pub fn to_value(&self) -> Value {
        json!({
            "privacy": self.privacy, "spend": self.spend, "quality": self.quality, "time": self.time,
            "ordering_note": "quality and time are recorded but do not order routes: the catalogue holds no quality or latency facts",
        })
    }

    fn local_only(&self) -> bool {
        self.privacy == "local_only"
    }
}

/// A saved `route_set` selection for one capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// `auto` | `local` | `api` | `rented`.
    pub route: String,
    pub model: Option<String>,
}

impl Selection {
    pub fn from_value(v: &Value) -> Option<Selection> {
        let route = v.get("route")?.as_str()?.to_string();
        let model = v
            .get("model")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
            .map(String::from);
        Some(Selection { route, model })
    }

    /// `auto` with no model names nothing.
    fn names_route(&self) -> bool {
        self.route != "auto" || self.model.is_some()
    }

    fn matches(&self, c: &Candidate) -> bool {
        let class = match self.route.as_str() {
            "local" => !c.cloud,
            "api" => c.cloud,
            "auto" => true,
            _ => false,
        };
        class && self.model.as_deref().is_none_or(|m| m == c.model)
    }

    fn label(&self) -> String {
        match &self.model {
            Some(m) => format!("{} {m}", self.route),
            None => self.route.clone(),
        }
    }

    fn to_value(&self) -> Value {
        json!({"route": self.route, "model": self.model})
    }
}

// ------------------------------------------------------------------ the pure decision

/// What the request names. `kind` is the modality word, used only in reasons.
#[derive(Debug, Clone, Default)]
pub struct RouteRequest {
    pub kind: String,
    pub provider: Option<String>,
    pub model: Option<String>,
}

/// One route that serves the request kind, with its four states and estimate.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub provider: String,
    pub model: String,
    /// True when the provider needs a key (a cloud route).
    pub cloud: bool,
    pub configured: bool,
    pub installed: Option<bool>,
    pub qualified: Option<bool>,
    /// `Some` when the estimate would be refused by a spend cap: the reason text.
    pub cap_refusal: Option<String>,
    pub est_usd: f64,
    /// `request` (the adapter's estimate for this request) | `default_output` (the model's default-size estimate).
    pub est_source: &'static str,
}

impl Candidate {
    /// A route that spends money: a cloud route, or any route with a non-zero estimate.
    pub fn paid(&self) -> bool {
        self.cloud || self.est_usd > 0.0
    }

    pub fn blockers(&self) -> Vec<String> {
        let mut b = Vec::new();
        if !self.configured {
            b.push(format!("no key for {}", self.provider));
        }
        if self.installed == Some(false) {
            b.push("model not installed".to_string());
        }
        if self.qualified == Some(false) {
            b.push(format!(
                "price older than {STALE_AFTER_DAYS} days (unqualified)"
            ));
        }
        if let Some(c) = &self.cap_refusal {
            b.push(c.clone());
        }
        b
    }

    pub fn usable(&self) -> bool {
        self.blockers().is_empty()
    }

    fn id(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub candidate: Candidate,
    /// False when a request narrowing or a preference excluded the route.
    pub eligible: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Chosen,
    /// A usable paid route exists but nothing names paid spending: the user must choose.
    ChoiceRequired,
    NoUsableRoute,
    /// A bare model id offered by several providers and no saved selection resolves it.
    Ambiguous,
}

impl Outcome {
    pub fn name(self) -> &'static str {
        match self {
            Outcome::Chosen => "chosen",
            Outcome::ChoiceRequired => "route_choice_required",
            Outcome::NoUsableRoute => "no_usable_route",
            Outcome::Ambiguous => "ambiguous_route",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub outcome: Outcome,
    /// `request` | `saved_selection` | `saved_selection_resolves_ambiguity` | `free_route` | `spend_preference` | `none`.
    pub basis: &'static str,
    pub chosen: Option<Entry>,
    /// Every other candidate: usable and eligible first, then the rest; each in (estimate, provider, model) order.
    pub alternatives: Vec<Entry>,
    /// One sentence on the outcome.
    pub note: String,
    /// Set when a saved selection existed but could not be used.
    pub selection_note: Option<String>,
}

struct Row {
    c: Candidate,
    narrowed_out: bool,
    ex: Option<String>,
}

impl Row {
    fn usable(&self) -> bool {
        self.ex.is_none() && self.c.usable()
    }
}

fn order(a: &Candidate, b: &Candidate) -> Ordering {
    a.est_usd
        .total_cmp(&b.est_usd)
        .then_with(|| a.provider.cmp(&b.provider))
        .then_with(|| a.model.cmp(&b.model))
}

fn usd(v: f64) -> String {
    format!("${v:.4}")
}

fn row_reason(r: &Row, chosen: Option<&Candidate>, basis: &str, choice_required: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(p) = &r.ex {
        parts.push(p.clone());
    }
    parts.extend(r.c.blockers());
    if !parts.is_empty() {
        return parts.join("; ");
    }
    match chosen {
        Some(ch) => {
            if r.c.paid() && !ch.paid() {
                format!(
                    "paid route ({}); a usable free route was chosen",
                    usd(r.c.est_usd)
                )
            } else if r.c.est_usd > ch.est_usd + 1e-9 {
                format!(
                    "more expensive than {} by {}",
                    ch.id(),
                    usd(r.c.est_usd - ch.est_usd)
                )
            } else if r.c.est_usd < ch.est_usd - 1e-9 {
                let why = if basis == "saved_selection" || basis == "request" {
                    "the chosen route is the one named"
                } else {
                    "lower priority"
                };
                format!(
                    "{} cheaper than {}; not chosen: {why}",
                    usd(ch.est_usd - r.c.est_usd),
                    ch.id()
                )
            } else {
                format!(
                    "same price as {}; ordered after it by provider and model",
                    ch.id()
                )
            }
        }
        None if choice_required => {
            format!(
                "usable paid route ({}); spend needs your choice before any is used",
                usd(r.c.est_usd)
            )
        }
        None => "usable".to_string(),
    }
}

fn finish(
    mut rows: Vec<Row>,
    chosen_idx: Option<usize>,
    outcome: Outcome,
    basis: &'static str,
    reason: String,
    selection_note: Option<String>,
) -> Decision {
    let chosen_row = chosen_idx.map(|i| rows.remove(i));
    let chosen_c = chosen_row.as_ref().map(|r| r.c.clone());
    let choice_required = outcome == Outcome::ChoiceRequired;
    // Usable and eligible first; the sort is stable, so the (estimate, provider, model) order holds inside each group.
    let mut keyed: Vec<(bool, Entry)> = rows
        .iter()
        .map(|r| {
            (
                r.usable(),
                Entry {
                    candidate: r.c.clone(),
                    eligible: r.ex.is_none(),
                    reason: row_reason(r, chosen_c.as_ref(), basis, choice_required),
                },
            )
        })
        .collect();
    keyed.sort_by_key(|(u, _)| !*u);
    let alternatives: Vec<Entry> = keyed.into_iter().map(|(_, e)| e).collect();
    let chosen = chosen_row.map(|r| {
        let mut text = reason.clone();
        let b = r.c.blockers();
        if !b.is_empty() {
            text = format!("{text}; will be refused: {}", b.join("; "));
        }
        Entry {
            candidate: r.c,
            eligible: true,
            reason: text,
        }
    });
    Decision {
        outcome,
        basis,
        chosen,
        alternatives,
        note: reason,
        selection_note,
    }
}

/// The routing decision. Pure: the same inputs always give the same result.
pub fn decide(
    req: &RouteRequest,
    prefs: &Preferences,
    selection: Option<&Selection>,
    candidates: &[Candidate],
) -> Decision {
    let mut cands = candidates.to_vec();
    cands.sort_by(order);
    let np = req.provider.as_deref().filter(|s| !s.is_empty());
    let nm = req.model.as_deref().filter(|s| !s.is_empty());
    let explicit = np.is_some() && nm.is_some();
    // `route_set` stores Studio model ids (for example a local TTS model id) that are not provider
    // route ids. A `local` selection whose model matches no route keeps only its class, which is
    // free; an `api` selection never loosens, since that could spend on a different model.
    let selection_owned: Option<Selection> = selection.map(|s| {
        let mut s = s.clone();
        if s.route == "local"
            && s.model
                .as_ref()
                .is_some_and(|m| !candidates.iter().any(|c| c.model == *m))
        {
            s.model = None;
        }
        s
    });
    let selection = selection_owned.as_ref();

    let rows: Vec<Row> = cands
        .into_iter()
        .map(|c| {
            let mut narrowed_out = false;
            let mut ex: Option<String> = None;
            if !explicit {
                if let Some(p) = np.filter(|p| c.provider != *p) {
                    narrowed_out = true;
                    ex = Some(format!("not the requested provider {p}"));
                } else if let Some(m) = nm.filter(|m| c.model != *m) {
                    narrowed_out = true;
                    ex = Some(format!("not the requested model {m}"));
                } else if prefs.local_only() && c.cloud {
                    ex = Some("privacy preference excludes cloud routes".to_string());
                } else if prefs.spend == "free_only" && c.paid() {
                    ex = Some("spend preference is free_only; this route spends money".to_string());
                }
            }
            Row {
                c,
                narrowed_out,
                ex,
            }
        })
        .collect();

    // Rule 1: a fully named route is never overridden.
    if let (Some(p), Some(m)) = (np, nm) {
        return match rows
            .iter()
            .position(|r| r.c.provider == p && r.c.model == m)
        {
            Some(i) => {
                let mut note = "named in the request; routing never overrides it".to_string();
                if prefs.local_only() && rows[i].c.cloud {
                    note.push_str(
                        "; privacy preference is local_only but the request named this cloud route",
                    );
                }
                if prefs.spend == "free_only" && rows[i].c.paid() {
                    note.push_str(
                        "; spend preference is free_only but the request named this paid route",
                    );
                }
                finish(rows, Some(i), Outcome::Chosen, "request", note, None)
            }
            None => finish(
                rows,
                None,
                Outcome::NoUsableRoute,
                "request",
                format!("{p}/{m} is not offered for {}", req.kind),
                None,
            ),
        };
    }

    // A provider or a model alone narrows the request but does not complete it.
    if np.is_some() || nm.is_some() {
        let pool: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| !r.narrowed_out)
            .map(|(i, _)| i)
            .collect();
        if pool.is_empty() {
            return finish(
                rows,
                None,
                Outcome::NoUsableRoute,
                "request",
                format!("no {} route matches the request", req.kind),
                None,
            );
        }
        if pool.len() == 1 {
            let only = pool[0];
            if let Some(ex) = rows[only].ex.clone() {
                return finish(rows, None, Outcome::NoUsableRoute, "request", ex, None);
            }
            return finish(
                rows,
                Some(only),
                Outcome::Chosen,
                "request",
                "the only route matching the request".into(),
                None,
            );
        }
        if let Some(m) = nm {
            if let Some(sel) = selection.filter(|s| s.names_route()) {
                let matching: Vec<usize> = pool
                    .iter()
                    .copied()
                    .filter(|&i| rows[i].ex.is_none() && sel.matches(&rows[i].c))
                    .collect();
                if matching.len() == 1 {
                    let reason = format!(
                        "model {m} is offered by several providers; the saved selection ({}) picks this one",
                        sel.label()
                    );
                    return finish(
                        rows,
                        Some(matching[0]),
                        Outcome::Chosen,
                        "saved_selection_resolves_ambiguity",
                        reason,
                        None,
                    );
                }
            }
            let names: Vec<String> = pool.iter().map(|&i| rows[i].c.provider.clone()).collect();
            let reason = format!(
                "model {m} is offered by {}; pass provider explicitly",
                names.join(", ")
            );
            return finish(rows, None, Outcome::Ambiguous, "request", reason, None);
        }
        let reason = format!(
            "{} offers several {} models; name one",
            np.unwrap_or(""),
            req.kind
        );
        return finish(rows, None, Outcome::ChoiceRequired, "request", reason, None);
    }

    // Rule 2: the saved selection for the kind.
    let mut selection_note: Option<String> = None;
    if let Some(sel) = selection.filter(|s| s.names_route()) {
        if sel.route == "rented" {
            selection_note = Some(
                "saved selection is rented: rented workers are not provider routes (use studio.models.worker.action)"
                    .to_string(),
            );
        } else {
            let hit = rows.iter().position(|r| r.usable() && sel.matches(&r.c));
            if let Some(i) = hit {
                let reason = format!("saved selection for {}: {}", req.kind, sel.label());
                return finish(
                    rows,
                    Some(i),
                    Outcome::Chosen,
                    "saved_selection",
                    reason,
                    None,
                );
            }
            let why: Vec<String> = rows
                .iter()
                .filter(|r| sel.matches(&r.c))
                .map(|r| {
                    let mut parts: Vec<String> = r.ex.iter().cloned().collect();
                    parts.extend(r.c.blockers());
                    format!("{}: {}", r.c.id(), parts.join("; "))
                })
                .collect();
            let why = if why.is_empty() {
                "no route matches it".to_string()
            } else {
                why.join(" | ")
            };
            selection_note = Some(format!(
                "saved selection ({}) is unusable ({why}); fell back",
                sel.label()
            ));
        }
    }

    // Rules 3 and 4: free usable routes first; paid only on a named spend preference.
    if let Some(i) = rows.iter().position(|r| r.usable() && !r.c.paid()) {
        let free = rows.iter().filter(|r| r.usable() && !r.c.paid()).count();
        let reason = if free == 1 {
            "the only usable free route; no spend".to_string()
        } else {
            format!("lowest-estimate of {free} usable free routes (ties by provider, then model); no spend")
        };
        return finish(
            rows,
            Some(i),
            Outcome::Chosen,
            "free_route",
            reason,
            selection_note,
        );
    }
    if let Some(i) = rows.iter().position(Row::usable) {
        if prefs.spend == "cheapest" {
            let reason = format!(
                "spend preference cheapest: lowest-estimate usable paid route ({})",
                usd(rows[i].c.est_usd)
            );
            return finish(
                rows,
                Some(i),
                Outcome::Chosen,
                "spend_preference",
                reason,
                selection_note,
            );
        }
        let n = rows.iter().filter(|r| r.usable()).count();
        let reason = format!(
            "no usable free route and {n} usable paid route(s); nothing names paid spending (saved selection or spend preference cheapest), so the choice is yours"
        );
        return finish(
            rows,
            None,
            Outcome::ChoiceRequired,
            "none",
            reason,
            selection_note,
        );
    }
    finish(
        rows,
        None,
        Outcome::NoUsableRoute,
        "none",
        format!(
            "no usable {} route; see each alternative's reason",
            req.kind
        ),
        selection_note,
    )
}

fn entry_json(e: &Entry) -> Value {
    let c = &e.candidate;
    json!({
        "provider": c.provider,
        "model": c.model,
        "cloud": c.cloud,
        "states": {
            "configured": c.configured,
            "installed": c.installed,
            "qualified": c.qualified,
            "usable": c.usable(),
        },
        "est_usd": c.est_usd,
        "est_source": c.est_source,
        "eligible": e.eligible,
        "reason": e.reason,
    })
}

impl Decision {
    /// `{kind, outcome, route_choice_required, basis, chosen, alternatives, preferences, selection, selection_note, note}`.
    pub fn to_value(
        &self,
        kind: &str,
        prefs: &Preferences,
        selection: Option<&Selection>,
    ) -> Value {
        json!({
            "kind": kind,
            "outcome": self.outcome.name(),
            "route_choice_required": self.outcome == Outcome::ChoiceRequired,
            "basis": self.basis,
            "chosen": self.chosen.as_ref().map(entry_json),
            "alternatives": self.alternatives.iter().map(entry_json).collect::<Vec<_>>(),
            "preferences": prefs.to_value(),
            "selection": selection.map(Selection::to_value),
            "selection_note": self.selection_note,
            "note": self.note,
        })
    }

    /// The compact record echoed in a dry run and stored on the job.
    fn record(&self) -> Value {
        let chosen = self.chosen.as_ref();
        json!({
            "basis": self.basis,
            "reason": chosen.map(|c| c.reason.clone()).unwrap_or_else(|| self.note.clone()),
            "provider": chosen.map(|c| c.candidate.provider.clone()),
            "model": chosen.map(|c| c.candidate.model.clone()),
            "est_usd": chosen.map(|c| c.candidate.est_usd),
            "selection_note": self.selection_note,
            "alternatives_considered": self.alternatives.len(),
        })
    }
}

// ------------------------------------------------------------------ gathering real state

fn parse_kind(word: &str) -> GenResult<Kind> {
    serde_json::from_value::<Kind>(json!(word))
        .map_err(|_| GenError::new("unknown_kind", format!("unknown kind {word:?}")))
}

fn load_json(ctx: &Context, key: &str) -> GenResult<Option<Value>> {
    let db = store::open(ctx)?;
    Ok(db
        .read(|c| store::settings().get(c, key).map_err(store::settings_err))
        .map_err(store::je)?
        .and_then(|s| serde_json::from_str(&s).ok()))
}

pub fn load_preferences(ctx: &Context) -> GenResult<Preferences> {
    match load_json(ctx, PREFERENCES_KEY)? {
        None => Ok(Preferences::default()),
        // A stored value that no longer parses must not silently relax privacy: refuse.
        Some(v) => Preferences::merged(&Preferences::default(), &v).map_err(|e| {
            GenError::new(
                "invalid_preferences",
                format!("saved routing preferences are unusable: {}", e.message),
            )
        }),
    }
}

pub fn load_selection(ctx: &Context, kind_word: &str) -> GenResult<Option<Selection>> {
    Ok(load_json(ctx, SELECTIONS_KEY)?
        .and_then(|routes| routes.get(kind_word).cloned())
        .and_then(|v| Selection::from_value(&v)))
}

/// Every route that serves `kind`, with real state. Never contacts a provider: key presence is a
/// passive check, `runtime_ready` reads files, and only cloud adapters' own `estimate_usd` runs.
pub fn gather(
    ctx: &Context,
    registry: &[Box<dyn Provider>],
    kind: Kind,
    prompt: &str,
    params: &Value,
) -> GenResult<Vec<Candidate>> {
    let cat = Catalogue::get();
    let today = catalogue::today();
    let db = store::open(ctx)?;
    let mut out = Vec::new();
    for p in registry {
        // Local Blender renders a caller-supplied .glb (`params.source`); with no source it cannot serve a
        // request, so it is a route only when the request names one.
        if p.id() == "local-blender" && params.get("source").is_none() {
            continue;
        }
        let models: Vec<_> = p.models().iter().filter(|mi| mi.kind == kind).collect();
        if models.is_empty() {
            continue;
        }
        let cloud = p.needs_key();
        let configured = !cloud || credentials::status(ctx, p.id());
        let installed = p.runtime_ready().map(|r| r.is_ok());
        for mi in models {
            let qualified = cat.find(p.id(), mi.model).map(|e| !e.is_stale_on(today));
            let (est_usd, est_source) = if cloud {
                let req = GenRequest {
                    kind,
                    provider: p.id().to_string(),
                    model: mi.model.to_string(),
                    prompt: prompt.to_string(),
                    params: params.clone(),
                };
                match p.estimate_usd(&req) {
                    Ok(v) if v.is_finite() && v >= 0.0 => (v, "request"),
                    _ => (mi.est_usd, "default_output"),
                }
            } else {
                (mi.est_usd, "default_output")
            };
            let cap_refusal = if est_usd > 0.0 {
                match ledger::check(&db, est_usd) {
                    Ok(()) => None,
                    Err(e) if e.code == "spend_cap" => Some(
                        if e.message.contains("monthly cap") {
                            "would exceed the monthly cap"
                        } else if e.message.contains("device allowance") {
                            "would exceed this device's spend allowance"
                        } else {
                            "would exceed the daily cap"
                        }
                        .to_string(),
                    ),
                    Err(e) => return Err(e),
                }
            } else {
                None
            };
            out.push(Candidate {
                provider: p.id().to_string(),
                model: mi.model.to_string(),
                cloud,
                configured,
                installed,
                qualified,
                cap_refusal,
                est_usd,
                est_source,
            });
        }
    }
    Ok(out)
}

struct Plan {
    kind_word: String,
    prefs: Preferences,
    selection: Option<Selection>,
    decision: Decision,
}

fn plan(ctx: &Context, registry: &[Box<dyn Provider>], input: &Value) -> GenResult<Plan> {
    let word = input
        .get("kind")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| GenError::new("invalid_input", "missing field: kind"))?;
    let kind = parse_kind(word)?;
    let mut prefs = load_preferences(ctx)?;
    // A request may tighten privacy for itself; it never loosens the saved preference.
    match input.get("privacy").and_then(Value::as_str) {
        None => {}
        Some("local_only") => prefs.privacy = "local_only".into(),
        Some("any") => {}
        Some(other) => {
            return Err(GenError::new(
                "invalid_input",
                format!("privacy must be one of {PRIVACY_VALUES:?}, got {other:?}"),
            ))
        }
    }
    let selection = load_selection(ctx, word)?;
    let prompt = input
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or("route probe");
    let params = input.get("params").cloned().unwrap_or_else(|| json!({}));
    let candidates = gather(ctx, registry, kind, prompt, &params)?;
    let req = RouteRequest {
        kind: word.to_string(),
        provider: input
            .get("provider")
            .and_then(Value::as_str)
            .map(String::from),
        model: input.get("model").and_then(Value::as_str).map(String::from),
    };
    let decision = decide(&req, &prefs, selection.as_ref(), &candidates);
    Ok(Plan {
        kind_word: word.to_string(),
        prefs,
        selection,
        decision,
    })
}

/// Saved preferences the engine stores but cannot apply, named in every `media route` result.
pub const PREFERENCES_NOT_APPLIED: &[&str] = &["quality", "time"];
/// Why [`PREFERENCES_NOT_APPLIED`] are not applied.
pub const PREFERENCES_NOT_APPLIED_REASON: &str =
    "the catalogue records no quality or latency facts";

/// `media.route` `{kind, provider?, model?, prompt?, params?, privacy?}` →
/// `{kind, outcome, route_choice_required, basis, chosen, alternatives:[{provider, model, states:{configured,
/// installed, qualified, usable}, est_usd, reason, …}], preferences, selection, selection_note, note,
/// preferences_not_applied, preferences_not_applied_reason}`.
/// Submits nothing, reserves nothing, contacts no provider.
pub fn route_command(
    ctx: &Context,
    registry: &[Box<dyn Provider>],
    input: &Value,
) -> GenResult<Value> {
    let p = plan(ctx, registry, input)?;
    let mut out = p
        .decision
        .to_value(&p.kind_word, &p.prefs, p.selection.as_ref());
    out["preferences_not_applied"] = json!(PREFERENCES_NOT_APPLIED);
    out["preferences_not_applied_reason"] = json!(PREFERENCES_NOT_APPLIED_REASON);
    Ok(out)
}

/// `media.route_prefs` `{privacy?, spend?, quality?, time?}`: saves the given fields over the saved
/// preferences and returns them; no field returns the saved preferences unchanged.
pub fn prefs_command(ctx: &Context, input: &Value) -> GenResult<Value> {
    let current = load_preferences(ctx)?;
    let given = input.as_object().is_some_and(|o| !o.is_empty());
    if !given {
        return Ok(json!({"preferences": current.to_value(), "saved_under": PREFERENCES_KEY}));
    }
    let next = Preferences::merged(&current, input)?;
    let db = store::open(ctx)?;
    crate::ledger::audit_setting(
        &db,
        "media.route_prefs",
        &json!({"old": current.to_value(), "new": next.to_value()}),
    )?;
    let preferences = json!({"privacy": next.privacy, "spend": next.spend, "quality": next.quality, "time": next.time}).to_string();
    db.write(|c| {
        store::settings()
            .set(c, PREFERENCES_KEY, &preferences)
            .map_err(store::settings_err)
    })
    .map_err(store::je)?;
    Ok(json!({"preferences": next.to_value(), "saved_under": PREFERENCES_KEY}))
}

fn refusal(code: &str, d: &Decision, p: &Plan) -> GenError {
    let usable: Vec<String> = d
        .alternatives
        .iter()
        .filter(|e| e.eligible && e.candidate.usable())
        .map(|e| format!("{} ({})", e.candidate.id(), usd(e.candidate.est_usd)))
        .collect();
    let mut message = d.note.clone();
    if !usable.is_empty() {
        message.push_str(&format!("; usable: {}", usable.join(", ")));
    }
    if code == "route_choice_required" {
        message.push_str(
            "; name provider and model, set a saved selection (studio.models.route.set), or set the spend preference to cheapest (media route-prefs)",
        );
    }
    GenError::new(code, message).with_details(d.to_value(
        &p.kind_word,
        &p.prefs,
        p.selection.as_ref(),
    ))
}

/// The routing record the engine made for one request. It is the only way a `route` reaches the
/// generate path, and it travels beside the request (`api::generate_routed_with`), never inside it,
/// so a caller's own `route` key cannot carry one. The field is private: [`route_generate`] alone
/// creates a record.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineRoute(Value);

impl EngineRoute {
    /// The record as echoed in a dry run and stored on the job: basis, reason, chosen route, alternatives.
    pub fn value(&self) -> &Value {
        &self.0
    }
}

/// Refuse a `route` key a caller sent, whatever its value (even `null`). Every generate path ends in
/// `api::generate_with`, which calls this; `route_generate` calls it first, so the CLI refuses the key
/// instead of stripping it.
pub fn refuse_caller_route(input: &Value) -> GenResult<()> {
    if input.get("route").is_some() {
        return Err(GenError::new(
            "invalid_input",
            "route is set by the engine; remove the route key from the request",
        ));
    }
    Ok(())
}

/// Remove an engine `route` from a SAVED request before it is resubmitted. A request saved from an
/// earlier engine run can hold the `route` that run recorded, and the caller-key refusal would then
/// reject its own resume. The route is recomputed at each submission, so the saved copy is dropped.
pub fn strip_saved_route(saved: &mut Value) {
    if let Some(o) = saved.as_object_mut() {
        o.remove("route");
    }
}

/// Called by `media generate` before route validation. Refuses a caller-sent `route` (see
/// [`refuse_caller_route`]) and removes any client-sent `privacy` key, then:
///
/// - provider and model both named, or a provider alone: nothing changes and no record is made;
/// - a bare model offered by several providers: a saved selection that picks one fills in the
///   provider and the reason is recorded; otherwise nothing changes and the caller's
///   `ambiguous_route` stands;
/// - neither named: the chosen route fills `provider` and `model`, and the returned [`EngineRoute`]
///   carries the basis and reason; `route_choice_required` / `no_usable_route` are returned as errors
///   with the alternatives in `details`.
///
/// The record is returned, never written into `body`: the caller hands it to `api::generate_routed_with`.
pub fn route_generate(
    ctx: &Context,
    registry: &[Box<dyn Provider>],
    body: &mut Value,
) -> GenResult<Option<EngineRoute>> {
    refuse_caller_route(body)?;
    let privacy = body.get("privacy").cloned();
    if let Some(o) = body.as_object_mut() {
        o.remove("privacy");
    }
    let Some(word) = body
        .get("kind")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
    else {
        return Ok(None);
    };
    let Ok(kind) = parse_kind(&word) else {
        return Ok(None);
    };
    let has = |k: &str| {
        body.get(k)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
    };
    let (has_provider, has_model) = (has("provider"), has("model"));
    if has_provider {
        return Ok(None);
    }
    if has_model {
        let model = body["model"].as_str().unwrap_or_default().to_string();
        let served = registry
            .iter()
            .filter(|p| {
                p.models()
                    .iter()
                    .any(|mi| mi.model == model && mi.kind == kind)
            })
            .count();
        if served < 2 {
            return Ok(None);
        }
    }
    let mut request = body.clone();
    if let (Some(v), Some(o)) = (privacy, request.as_object_mut()) {
        o.insert("privacy".into(), v);
    }
    let p = plan(ctx, registry, &request)?;
    let d = &p.decision;
    match d.outcome {
        Outcome::Chosen => {
            let Some(c) = &d.chosen else {
                return Ok(None);
            };
            body["provider"] = json!(c.candidate.provider);
            body["model"] = json!(c.candidate.model);
            Ok(Some(EngineRoute(d.record())))
        }
        // The caller's own ambiguity error stands.
        Outcome::Ambiguous => Ok(None),
        Outcome::ChoiceRequired => Err(refusal("route_choice_required", d, &p)),
        Outcome::NoUsableRoute => Err(refusal("no_usable_route", d, &p)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(provider: &str, model: &str, installed: bool) -> Candidate {
        Candidate {
            provider: provider.into(),
            model: model.into(),
            cloud: false,
            configured: true,
            installed: Some(installed),
            qualified: None,
            cap_refusal: None,
            est_usd: 0.0,
            est_source: "default_output",
        }
    }

    fn cloud(provider: &str, model: &str, key: bool, est: f64) -> Candidate {
        Candidate {
            provider: provider.into(),
            model: model.into(),
            cloud: true,
            configured: key,
            installed: None,
            qualified: None,
            cap_refusal: None,
            est_usd: est,
            est_source: "request",
        }
    }

    fn req(provider: Option<&str>, model: Option<&str>) -> RouteRequest {
        RouteRequest {
            kind: "voice".into(),
            provider: provider.map(String::from),
            model: model.map(String::from),
        }
    }

    fn sel(route: &str, model: Option<&str>) -> Selection {
        Selection {
            route: route.into(),
            model: model.map(String::from),
        }
    }

    fn find<'a>(d: &'a Decision, provider: &str) -> &'a Entry {
        d.alternatives
            .iter()
            .find(|e| e.candidate.provider == provider)
            .expect("alternative present")
    }

    #[test]
    fn keyless_local_route_wins_and_cloud_alternatives_say_no_key() {
        let c = [
            local("local-qts", "qts", true),
            cloud("elevenlabs", "v4", false, 0.02),
        ];
        let d = decide(&req(None, None), &Preferences::default(), None, &c);
        assert_eq!(d.outcome, Outcome::Chosen);
        assert_eq!(d.chosen.as_ref().unwrap().candidate.provider, "local-qts");
        assert_eq!(d.basis, "free_route");
        let e = find(&d, "elevenlabs");
        assert!(e.reason.contains("no key"), "{}", e.reason);
        assert!(!e.candidate.usable());
    }

    #[test]
    fn explicit_provider_and_model_is_never_overridden() {
        let c = [
            local("local-qts", "qts", true),
            cloud("elevenlabs", "v4", false, 0.02),
        ];
        let prefs = Preferences {
            privacy: "local_only".into(),
            ..Preferences::default()
        };
        let d = decide(
            &req(Some("elevenlabs"), Some("v4")),
            &prefs,
            Some(&sel("local", None)),
            &c,
        );
        assert_eq!(d.outcome, Outcome::Chosen);
        let ch = d.chosen.unwrap();
        assert_eq!(ch.candidate.provider, "elevenlabs");
        assert_eq!(d.basis, "request");
        assert!(ch.reason.contains("never overrides"), "{}", ch.reason);
        assert!(
            ch.reason.contains("privacy preference is local_only"),
            "{}",
            ch.reason
        );
        assert!(
            ch.reason.contains("no key"),
            "blockers are reported: {}",
            ch.reason
        );
        assert_eq!(d.alternatives.len(), 1);
    }

    #[test]
    fn explicit_route_not_offered_is_reported_not_substituted() {
        let c = [local("local-qts", "qts", true)];
        let d = decide(
            &req(Some("fal"), Some("nope")),
            &Preferences::default(),
            None,
            &c,
        );
        assert_eq!(d.outcome, Outcome::NoUsableRoute);
        assert!(d.chosen.is_none());
    }

    #[test]
    fn saved_selection_is_honoured_when_usable() {
        let c = [
            local("local-qts", "qts", true),
            cloud("elevenlabs", "v4", true, 0.02),
        ];
        let d = decide(
            &req(None, None),
            &Preferences::default(),
            Some(&sel("api", Some("v4"))),
            &c,
        );
        assert_eq!(d.basis, "saved_selection");
        assert_eq!(d.chosen.as_ref().unwrap().candidate.provider, "elevenlabs");
        assert!(
            find(&d, "local-qts").reason.contains("cheaper"),
            "{}",
            find(&d, "local-qts").reason
        );
    }

    #[test]
    fn unusable_saved_selection_explains_and_falls_back() {
        let c = [
            local("local-qts", "qts", true),
            cloud("elevenlabs", "v4", false, 0.02),
        ];
        let d = decide(
            &req(None, None),
            &Preferences::default(),
            Some(&sel("api", Some("v4"))),
            &c,
        );
        assert_eq!(d.chosen.as_ref().unwrap().candidate.provider, "local-qts");
        let note = d.selection_note.expect("note");
        assert!(
            note.contains("saved selection") && note.contains("no key"),
            "{note}"
        );
    }

    #[test]
    fn privacy_excludes_cloud_even_when_usable() {
        let c = [
            local("local-qts", "qts", false),
            cloud("elevenlabs", "v4", true, 0.02),
        ];
        let prefs = Preferences {
            privacy: "local_only".into(),
            spend: "cheapest".into(),
            ..Preferences::default()
        };
        let d = decide(&req(None, None), &prefs, Some(&sel("api", None)), &c);
        assert_eq!(
            d.outcome,
            Outcome::NoUsableRoute,
            "never silently falls to cloud"
        );
        let e = find(&d, "elevenlabs");
        assert!(!e.eligible);
        assert!(
            e.reason
                .contains("privacy preference excludes cloud routes"),
            "{}",
            e.reason
        );
        assert!(find(&d, "local-qts").reason.contains("model not installed"));
    }

    #[test]
    fn paid_only_requires_a_choice_unless_spend_is_cheapest() {
        let c = [
            cloud("fal", "a", true, 0.10),
            cloud("wavespeed", "b", true, 0.05),
        ];
        let d = decide(&req(None, None), &Preferences::default(), None, &c);
        assert_eq!(d.outcome, Outcome::ChoiceRequired);
        assert!(d.chosen.is_none());
        assert_eq!(
            d.alternatives[0].candidate.provider, "wavespeed",
            "cheapest first"
        );
        assert!(d.alternatives[0].reason.contains("spend needs your choice"));

        let prefs = Preferences {
            spend: "cheapest".into(),
            ..Preferences::default()
        };
        let d = decide(&req(None, None), &prefs, None, &c);
        assert_eq!(d.outcome, Outcome::Chosen);
        assert_eq!(d.basis, "spend_preference");
        assert_eq!(d.chosen.as_ref().unwrap().candidate.provider, "wavespeed");
        let fal = find(&d, "fal");
        assert!(
            fal.reason
                .contains("more expensive than wavespeed/b by $0.0500"),
            "{}",
            fal.reason
        );
    }

    #[test]
    fn free_only_excludes_paid_routes() {
        let c = [
            local("local-qts", "qts", true),
            cloud("elevenlabs", "v4", true, 0.02),
        ];
        let prefs = Preferences {
            spend: "free_only".into(),
            ..Preferences::default()
        };
        let d = decide(&req(None, None), &prefs, Some(&sel("api", None)), &c);
        assert_eq!(d.chosen.as_ref().unwrap().candidate.provider, "local-qts");
        assert!(find(&d, "elevenlabs").reason.contains("free_only"));
    }

    #[test]
    fn stale_price_and_cap_make_a_route_unusable_with_reasons() {
        let mut stale = cloud("fal", "a", true, 0.10);
        stale.qualified = Some(false);
        let mut capped = cloud("wavespeed", "b", true, 0.05);
        capped.cap_refusal = Some("would exceed the daily cap".into());
        let d = decide(
            &req(None, None),
            &Preferences {
                spend: "cheapest".into(),
                ..Preferences::default()
            },
            None,
            &[stale, capped],
        );
        assert_eq!(d.outcome, Outcome::NoUsableRoute);
        assert!(find(&d, "fal").reason.contains("price older than 60 days"));
        assert!(find(&d, "wavespeed")
            .reason
            .contains("would exceed the daily cap"));
    }

    #[test]
    fn ties_break_by_provider_then_model() {
        let c = [
            local("b-local", "m", true),
            local("a-local", "z", true),
            local("a-local", "y", true),
        ];
        let d = decide(&req(None, None), &Preferences::default(), None, &c);
        let ch = d.chosen.as_ref().unwrap();
        assert_eq!(
            (ch.candidate.provider.as_str(), ch.candidate.model.as_str()),
            ("a-local", "y")
        );
        assert!(find(&d, "b-local").reason.contains("same price"));
        // Input order does not matter.
        let mut rev = c.to_vec();
        rev.reverse();
        assert_eq!(
            decide(&req(None, None), &Preferences::default(), None, &rev)
                .chosen
                .unwrap()
                .candidate
                .model,
            "y"
        );
    }

    #[test]
    fn bare_model_on_two_providers_is_ambiguous_unless_a_selection_resolves_it() {
        let c = [
            cloud("fal", "m", true, 0.1),
            cloud("wavespeed", "m", true, 0.1),
            cloud("fal", "other", true, 0.1),
        ];
        let d = decide(&req(None, Some("m")), &Preferences::default(), None, &c);
        assert_eq!(d.outcome, Outcome::Ambiguous);
        assert!(
            d.note.contains("fal, wavespeed") && d.note.contains("pass provider explicitly"),
            "{}",
            d.note
        );

        // A selection by model alone matches both providers: still ambiguous.
        let d = decide(
            &req(None, Some("m")),
            &Preferences::default(),
            Some(&sel("api", Some("m"))),
            &c,
        );
        assert_eq!(d.outcome, Outcome::Ambiguous);

        // A selection that distinguishes them (here by keyless vs cloud) resolves it and says so.
        let c = [local("local-x", "m", true), cloud("fal", "m", true, 0.1)];
        let d = decide(
            &req(None, Some("m")),
            &Preferences::default(),
            Some(&sel("local", None)),
            &c,
        );
        assert_eq!(d.outcome, Outcome::Chosen);
        assert_eq!(d.basis, "saved_selection_resolves_ambiguity");
        assert!(d.chosen.unwrap().reason.contains("saved selection"));
    }

    #[test]
    fn bare_model_with_one_provider_is_chosen_as_named() {
        let c = [cloud("fal", "m", true, 0.1), cloud("fal", "n", true, 0.1)];
        let d = decide(&req(None, Some("m")), &Preferences::default(), None, &c);
        assert_eq!(d.outcome, Outcome::Chosen);
        assert_eq!(d.basis, "request");
    }

    #[test]
    fn preferences_validate_and_merge() {
        let base = Preferences::default();
        let p = Preferences::merged(&base, &json!({"privacy": "local_only"})).unwrap();
        assert_eq!(
            (p.privacy.as_str(), p.spend.as_str()),
            ("local_only", "ask")
        );
        assert!(Preferences::merged(&base, &json!({"privacy": "sometimes"})).is_err());
        assert!(Preferences::merged(&base, &json!({"colour": "red"})).is_err());
    }

    #[test]
    fn caller_route_key_is_refused_whatever_its_value() {
        for forged in [
            json!({"basis": "free_route", "reason": "forged"}),
            json!("x"),
            Value::Null,
        ] {
            let input = json!({"kind": "image", "route": forged});
            let err = refuse_caller_route(&input).expect_err("any caller route key is refused");
            assert_eq!(err.code, "invalid_input");
            assert!(
                err.message.contains("route is set by the engine"),
                "{}",
                err.message
            );
        }
        assert!(refuse_caller_route(&json!({"kind": "image", "prompt": "a cat"})).is_ok());
    }

    #[test]
    fn saved_request_strips_only_the_stored_engine_route() {
        let mut saved =
            json!({"kind": "image", "model": "m", "route": {"basis": "free_route"}, "scope": null});
        strip_saved_route(&mut saved);
        assert_eq!(saved, json!({"kind": "image", "model": "m", "scope": null}));
        let mut bare = json!({"kind": "image"});
        strip_saved_route(&mut bare);
        assert_eq!(bare, json!({"kind": "image"}));
    }

    #[test]
    fn not_applied_preferences_are_stored_preferences() {
        // Each name the route output says is not applied must be a stored preference field.
        let stored = Preferences::default().to_value();
        for name in PREFERENCES_NOT_APPLIED {
            assert!(stored.get(*name).is_some(), "{name} is a stored preference");
        }
        assert_eq!(PREFERENCES_NOT_APPLIED, ["quality", "time"]);
        assert!(PREFERENCES_NOT_APPLIED_REASON.contains("no quality or latency facts"));
    }
}
