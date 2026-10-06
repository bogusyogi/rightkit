//! The engine: snapshots with stable ids, the stale-snapshot guard, the
//! four-tier input ladder, safety gating, and evidence receipts. Generic over
//! `Platform`; everything here is exercised on every host by `MockPlatform`.

use crate::admission::{Admission, Admit, EffectKind, EffectRequest, Settled};
use crate::events::{Counters, DeniedBy, Journal, LifecycleEvent, Metrics};
use crate::in_app::InApp;
use crate::lease::{Admitted, Fence, Lease, LeaseRequest, LeaseStatus, Leases};
use crate::redact::{self, Redactor, ShotContext, ShotReport};
use crate::keys;
use crate::model::*;
use crate::platform::{InputEvent, Platform};
use crate::policy::*;
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How strictly an act verifies the snapshot it names is still current.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guard {
    /// Re-read the tree; any structural change returns `Changed` + a fresh snapshot.
    Structure,
    /// Only the target element must still exist with the same role and name.
    Element,
    Off,
}

#[derive(Debug, Clone)]
pub struct ActOptions {
    /// Lowest tier to try (diagnostics and proofs: start above the semantic tier).
    pub min_tier: Tier,
    /// Highest tier this act may use. `Foreground` is the explicit opt-in.
    pub max_tier: Tier,
    pub guard: Guard,
    /// Risk classes the caller has confirmed with the user.
    pub confirm: Vec<RiskClass>,
    /// Treat any frontmost change caused by tiers 1-3 as an error (and restore).
    pub strict_focus: bool,
    /// Keep (truncated, non-secret) payloads in receipts; default is length only.
    pub log_values: bool,
    /// Sleep after a successful act so the app can react.
    pub settle_ms: u64,
    /// Lease fence `{epoch, seq}`. Required while the app is leased; refused when it is not.
    pub fence: Option<Fence>,
}

impl Default for ActOptions {
    fn default() -> Self {
        ActOptions {
            min_tier: Tier::Semantic,
            max_tier: Tier::Targeted,
            guard: Guard::Structure,
            confirm: vec![],
            strict_focus: false,
            log_values: false,
            settle_ms: 0,
            fence: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ActOutcome {
    pub receipt: Receipt,
    /// Present when the guard found the snapshot stale: nothing was done.
    pub fresh: Option<Snapshot>,
}

impl ActOutcome {
    pub fn done(&self) -> bool {
        self.receipt.result == ResultKind::Done
    }
}

struct Stored<E> {
    snap: Snapshot,
    els: HashMap<String, E>,
    labels: HashMap<String, (String, Option<String>, bool)>,
    frames: HashMap<String, Rect>,
    limits: Limits,
    target: Target,
}

pub struct Automation<P: Platform> {
    pub(crate) p: Arc<P>,
    pub(crate) policy: Arc<Policy>,
    evidence: Mutex<Vec<Arc<dyn EvidenceSink>>>,
    indicator: Mutex<Option<Arc<dyn Indicator>>>,
    in_app: Mutex<Vec<Arc<dyn InApp>>>,
    store: Mutex<VecDeque<Stored<P::El>>>,
    seq: AtomicU64,
    snap_seq: AtomicU64,
    sessions: Mutex<HashMap<i32, String>>,
    session_seq: AtomicU64,
    pub(crate) leases: Leases,
    admission: Mutex<Option<Arc<dyn Admission>>>,
    journals: Mutex<Vec<Arc<dyn Journal>>>,
    redactors: Mutex<Vec<Arc<dyn Redactor>>>,
    pub(crate) counters: Counters,
}

const STORE_CAP: usize = 8;

fn fnv(h: u64, bytes: &[u8]) -> u64 {
    let mut h = h;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}
const FNV0: u64 = 0xcbf2_9ce4_8422_2325;

/// Label used for stable ids: identifier, else name, else description.
fn label_of(r: &RawNode) -> String {
    r.identifier
        .as_deref()
        .filter(|s| !s.is_empty())
        .or(r.name.as_deref().filter(|s| !s.is_empty()))
        .or(r.description.as_deref().filter(|s| !s.is_empty()))
        .unwrap_or("")
        .chars()
        .take(80)
        .collect()
}

impl<P: Platform> Automation<P> {
    pub fn new(platform: P, policy: Policy) -> Self {
        Automation {
            p: Arc::new(platform),
            policy: Arc::new(policy),
            evidence: Mutex::new(vec![]),
            indicator: Mutex::new(None),
            in_app: Mutex::new(vec![]),
            store: Mutex::new(VecDeque::new()),
            seq: AtomicU64::new(0),
            snap_seq: AtomicU64::new(0),
            sessions: Mutex::new(HashMap::new()),
            session_seq: AtomicU64::new(0),
            leases: Leases::default(),
            admission: Mutex::new(None),
            journals: Mutex::new(vec![]),
            redactors: Mutex::new(vec![]),
            counters: Counters::default(),
        }
    }

    pub fn platform(&self) -> &P {
        &self.p
    }
    pub fn policy(&self) -> &Policy {
        &self.policy
    }
    pub fn add_evidence(&self, sink: Arc<dyn EvidenceSink>) {
        self.evidence.lock().unwrap().push(sink);
    }
    pub fn set_indicator(&self, i: Arc<dyn Indicator>) {
        *self.indicator.lock().unwrap() = Some(i);
    }
    /// Install the caller's admission hook (EFF-001). Every effect passes it after
    /// validation and before execution; a denial runs nothing.
    pub fn set_admission(&self, a: Arc<dyn Admission>) {
        *self.admission.lock().unwrap() = Some(a);
    }
    /// Receive structured lifecycle events (start, stop, target exit, denied, lease).
    pub fn add_journal(&self, j: Arc<dyn Journal>) {
        self.journals.lock().unwrap().push(j);
    }
    /// Add a screenshot redaction hook (extra regions to mask in every capture).
    pub fn add_redactor(&self, r: Arc<dyn Redactor>) {
        self.redactors.lock().unwrap().push(r);
    }
    /// Content-free counters.
    pub fn metrics(&self) -> Metrics {
        self.counters.snapshot()
    }

    pub(crate) fn journal(&self, e: LifecycleEvent) {
        for j in self.journals.lock().unwrap().iter() {
            j.event(&e);
        }
    }

    fn approve(&self, req: &EffectRequest) -> std::result::Result<Option<Tier>, Error> {
        let hook = self.admission.lock().unwrap().clone();
        match hook.map(|h| h.approve(req)).unwrap_or(Admit::Allow) {
            Admit::Allow => Ok(None),
            Admit::AllowUpTo(t) => Ok(Some(t)),
            Admit::Deny(why) => Err(Error::Denied(why)),
        }
    }

    pub(crate) fn settle(&self, req: &EffectRequest, out: &Settled) {
        if let Some(h) = self.admission.lock().unwrap().clone() {
            h.settle(req, out);
        }
    }

    pub(crate) fn effect(&self, kind: EffectKind, pid: i32, app_id: &str, action: &str, max_tier: Tier) -> EffectRequest {
        EffectRequest {
            kind,
            session: self.session_for(pid),
            pid,
            app_id: app_id.to_string(),
            action: action.to_string(),
            target: None,
            max_tier,
            risk: None,
            payload_chars: None,
            lease_epoch: self.leases.status(pid).map(|l| l.epoch),
        }
    }

    /// Run the admission hook for a non-act effect, recording a denial.
    pub(crate) fn admit_effect(&self, req: &EffectRequest) -> Result<Option<Tier>> {
        match self.approve(req) {
            Ok(cap) => Ok(cap),
            Err(e) => {
                self.denied(&req.session, req.pid, &req.app_id, &req.action, DeniedBy::Admission, &e.to_string());
                Err(e)
            }
        }
    }

    pub(crate) fn denied(&self, session: &str, pid: i32, app_id: &str, action: &str, by: DeniedBy, reason: &str) {
        self.counters.denied.fetch_add(1, Ordering::SeqCst);
        self.journal(LifecycleEvent::Denied { session: session.into(), pid, app_id: app_id.into(), action: action.into(), by, reason: reason.into() });
    }

    // ------------------------------------------------------------ leases

    /// Take the control lease on `pid`. While it is held, every input to that app
    /// must carry the lease's fence (`ActOptions::fence`).
    pub fn acquire_lease(&self, pid: i32, req: LeaseRequest) -> Result<Lease> {
        let app = self.checked_app(pid)?;
        let (lease, displaced) = self.leases.acquire(pid, &req)?;
        let _ = app;
        self.journal(LifecycleEvent::LeaseGranted { pid, holder: lease.holder.clone(), epoch: lease.epoch, displaced: displaced.map(|d| d.0) });
        Ok(lease)
    }
    pub fn renew_lease(&self, lease: &Lease) -> bool {
        self.leases.renew(lease.pid, lease.epoch)
    }
    pub fn release_lease(&self, lease: &Lease) -> bool {
        let ok = self.leases.release(lease.pid, lease.epoch);
        if ok {
            self.journal(LifecycleEvent::LeaseReleased { pid: lease.pid, epoch: lease.epoch });
        }
        ok
    }
    pub fn lease_status(&self, pid: i32) -> Option<LeaseStatus> {
        self.leases.status(pid)
    }

    /// Check that every app with a session or lease is still running. Emits
    /// `TargetExited` for each that vanished (crash or user quit), ends its
    /// session and drops its lease. Call it from the host's tick or on error.
    pub fn poll_targets(&self) -> Vec<i32> {
        let mut pids: Vec<i32> = self.sessions.lock().unwrap().keys().copied().collect();
        pids.extend(self.leases.all_pids());
        pids.sort_unstable();
        pids.dedup();
        let mut gone = vec![];
        for pid in pids {
            if self.p.app(pid).is_none() {
                let session = self.sessions.lock().unwrap().remove(&pid).unwrap_or_else(|| "none".into());
                let epoch = self.leases.status(pid).map(|l| l.epoch);
                self.leases.drop_pid(pid);
                self.journal(LifecycleEvent::TargetExited { session, pid, app_id: String::new() });
                if let Some(epoch) = epoch {
                    self.journal(LifecycleEvent::LeaseReleased { pid, epoch });
                }
                gone.push(pid);
            }
        }
        gone
    }

    pub fn add_in_app(&self, b: Arc<dyn InApp>) {
        self.in_app.lock().unwrap().push(b);
    }

    pub(crate) fn in_app_bridge(&self, pid: i32) -> Option<Arc<dyn InApp>> {
        self.in_app.lock().unwrap().iter().find(|b| b.handles(pid)).cloned()
    }

    pub(crate) fn primary_meta(&self) -> bool {
        self.p.kind() != PlatformKind::Windows
    }

    // ------------------------------------------------------------ observe

    pub fn status(&self) -> Status {
        self.p.status()
    }
    pub fn apps(&self) -> Vec<AppInfo> {
        self.p.apps()
    }
    pub fn windows(&self, on_screen_only: bool) -> Vec<WindowInfo> {
        self.p.windows(on_screen_only)
    }
    pub fn frontmost(&self) -> Option<FrontApp> {
        self.p.frontmost()
    }
    pub(crate) fn front_pid(&self) -> Option<i32> {
        self.p.frontmost().map(|f| f.pid)
    }

    pub(crate) fn checked_app(&self, pid: i32) -> Result<AppInfo> {
        let app = self.p.app(pid).ok_or_else(|| Error::NotFound(format!("pid {pid}")))?;
        self.policy.check(&app)?;
        Ok(app)
    }

    /// Start (or reuse) a visible session for `pid`; emits `SessionEvent::Started`.
    pub fn begin_session(&self, pid: i32, label: &str) -> Result<String> {
        let app = self.checked_app(pid)?;
        let mut s = self.sessions.lock().unwrap();
        if let Some(id) = s.get(&pid) {
            return Ok(id.clone());
        }
        let id = format!("s{}-{}", now_ms() % 100_000_000, self.session_seq.fetch_add(1, Ordering::SeqCst) + 1);
        s.insert(pid, id.clone());
        drop(s);
        self.counters.sessions_started.fetch_add(1, Ordering::SeqCst);
        self.journal(LifecycleEvent::Started { session: id.clone(), pid, app_id: app.app_id.clone(), label: label.to_string() });
        self.emit(SessionEvent::Started { session: id.clone(), pid, app_id: app.app_id, label: label.to_string() });
        Ok(id)
    }

    pub fn end_session(&self, pid: i32) {
        if let Some(id) = self.sessions.lock().unwrap().remove(&pid) {
            self.journal(LifecycleEvent::Stopped { session: id.clone(), pid });
            self.emit(SessionEvent::Ended { session: id });
        }
    }

    pub(crate) fn session_for(&self, pid: i32) -> String {
        self.sessions.lock().unwrap().get(&pid).cloned().unwrap_or_else(|| "none".into())
    }

    fn emit(&self, e: SessionEvent) {
        if let Some(i) = self.indicator.lock().unwrap().as_ref() {
            i.event(&e);
        }
    }

    fn record(&self, r: &Receipt) {
        for s in self.evidence.lock().unwrap().iter() {
            s.record(r);
        }
        self.emit(SessionEvent::Acted {
            session: r.session.clone(),
            action: r.action.clone(),
            tier: r.tier_used,
            result: r.result,
        });
    }

    /// Read the tree of `target` into a snapshot with stable element ids.
    pub fn snapshot(&self, target: Target, limits: Limits) -> Result<Snapshot> {
        if killed() {
            return Err(Error::Blocked("kill switch engaged".into()));
        }
        let app = self.checked_app(target.pid)?;
        if self.admission.lock().unwrap().is_some() {
            let eff = self.effect(EffectKind::Observe, target.pid, &app.app_id, "snapshot", Tier::Semantic);
            self.admit_effect(&eff)?;
        }
        self.p.wake(target.pid);
        let root = self.p.root(&target)?;
        let window_id = target.window_id.or_else(|| self.p.window_of(&root));
        let focused_el = self.p.focused(target.pid).ok().flatten();

        let mut els: HashMap<String, P::El> = HashMap::new();
        let mut labels = HashMap::new();
        let mut frames = HashMap::new();
        let mut budget = limits.max_nodes;
        let mut truncated = false;
        let mut used: std::collections::HashSet<String> = Default::default();
        let mut focused_id: Option<String> = None;
        let mut fp = FNV0;

        let node = self.walk(
            &root,
            "",
            0,
            &limits,
            &mut budget,
            &mut truncated,
            &mut used,
            &mut els,
            &mut labels,
            &mut frames,
            &focused_el,
            &mut focused_id,
            &mut fp,
        )?;
        let id = format!("snap{}", self.snap_seq.fetch_add(1, Ordering::SeqCst) + 1);
        let snap = Snapshot {
            id,
            pid: target.pid,
            app_id: app.app_id,
            window_id,
            taken_ms: now_ms(),
            fingerprint: fp,
            truncated,
            focused: focused_id,
            root: node,
        };
        let mut st = self.store.lock().unwrap();
        st.push_back(Stored { snap: snap.clone(), els, labels, frames, limits, target });
        while st.len() > STORE_CAP {
            st.pop_front();
        }
        Ok(snap)
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &self,
        el: &P::El,
        parent_id: &str,
        depth: usize,
        limits: &Limits,
        budget: &mut usize,
        truncated: &mut bool,
        used: &mut std::collections::HashSet<String>,
        els: &mut HashMap<String, P::El>,
        labels: &mut HashMap<String, (String, Option<String>, bool)>,
        frames: &mut HashMap<String, Rect>,
        focused: &Option<P::El>,
        focused_id: &mut Option<String>,
        fp: &mut u64,
    ) -> Result<Node> {
        let raw = self.p.read(el).ok_or_else(|| Error::NotFound("element vanished while reading".into()))?;
        *budget = budget.saturating_sub(1);
        let id = self.alloc_id(parent_id, &raw, 0, used);
        self.finish_node(el, raw, id, depth, limits, budget, truncated, used, els, labels, frames, focused, focused_id, fp)
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_node(
        &self,
        el: &P::El,
        raw: RawNode,
        id: String,
        depth: usize,
        limits: &Limits,
        budget: &mut usize,
        truncated: &mut bool,
        used: &mut std::collections::HashSet<String>,
        els: &mut HashMap<String, P::El>,
        labels: &mut HashMap<String, (String, Option<String>, bool)>,
        frames: &mut HashMap<String, Rect>,
        focused: &Option<P::El>,
        focused_id: &mut Option<String>,
        fp: &mut u64,
    ) -> Result<Node> {
        *fp = fnv(*fp, id.as_bytes());
        *fp = fnv(*fp, raw.role.as_bytes());
        *fp = fnv(*fp, &[u8::from(raw.enabled.unwrap_or(true))]);
        els.insert(id.clone(), el.clone());
        labels.insert(
            id.clone(),
            (raw.role.clone(), raw.name.clone().or_else(|| raw.description.clone()).or_else(|| raw.identifier.clone()), raw.secure),
        );
        if let Some(f) = raw.frame {
            frames.insert(id.clone(), f);
        }
        if focused_id.is_none() {
            if let Some(f) = focused {
                if self.p.same(f, el) {
                    *focused_id = Some(id.clone());
                }
            }
        }
        let mut node = Node {
            id: id.clone(),
            role: raw.role,
            native_role: raw.native_role,
            subrole: raw.subrole,
            name: raw.name,
            value: raw.value,
            description: raw.description,
            identifier: raw.identifier,
            enabled: raw.enabled,
            secure: raw.secure,
            frame: raw.frame,
            caps: raw.caps,
            children: vec![],
        };
        if depth >= limits.max_depth {
            return Ok(node);
        }
        let mut ordinals: HashMap<(String, String), usize> = HashMap::new();
        for child in self.p.children(el) {
            if *budget == 0 {
                *truncated = true;
                break;
            }
            let Some(craw) = self.p.read(&child) else { continue };
            *budget -= 1;
            let key = (craw.role.clone(), label_of(&craw));
            let ord = ordinals.entry(key).or_insert(0);
            let n = *ord;
            *ord += 1;
            let cid = self.alloc_id(&id, &craw, n, used);
            node.children.push(self.finish_node(
                &child, craw, cid, depth + 1, limits, budget, truncated, used, els, labels, frames, focused, focused_id, fp,
            )?);
        }
        Ok(node)
    }

    /// Ids hash `(parent id, role, label, ordinal among equal siblings)`: stable
    /// across snapshots while structure and labels are unchanged, never value.
    fn alloc_id(&self, parent: &str, raw: &RawNode, ordinal: usize, used: &mut std::collections::HashSet<String>) -> String {
        let mut h = fnv(FNV0, parent.as_bytes());
        h = fnv(h, raw.role.as_bytes());
        h = fnv(h, label_of(raw).as_bytes());
        h = fnv(h, &(ordinal as u64).to_le_bytes());
        let mut id = format!("e{:08x}", (h ^ (h >> 32)) as u32);
        let mut bump = 0u64;
        while !used.insert(id.clone()) {
            bump += 1;
            h = fnv(h, &bump.to_le_bytes());
            id = format!("e{:08x}", (h ^ (h >> 32)) as u32);
        }
        id
    }

    /// Per-window PNG. Policy- and admission-gated, and **redacted**: secure
    /// fields, credential-labelled fields and every region a [`Redactor`] names are
    /// painted black before the file is returned. Returns the window id.
    pub fn screenshot(&self, target: &Target, path: &Path) -> Result<u64> {
        self.screenshot_report(target, path).map(|r| r.window_id)
    }

    /// Like [`Automation::screenshot`], reporting how many regions were masked.
    /// Fails closed: when a region must be masked and the capture cannot be
    /// rewritten, the file is deleted and an error is returned.
    pub fn screenshot_report(&self, target: &Target, path: &Path) -> Result<ShotReport> {
        if !path.is_absolute() {
            return Err(Error::Invalid("screenshot path must be absolute (never cwd-relative)".into()));
        }
        let app = self.checked_app(target.pid)?;
        let eff = self.effect(EffectKind::Capture, target.pid, &app.app_id, "screenshot", Tier::Semantic);
        self.admit_effect(&eff)?;
        // The same window a default snapshot reads (the app's main window), else the largest on-screen one.
        let wid = match target.window_id.or_else(|| self.p.root(target).ok().and_then(|r| self.p.window_of(&r))) {
            Some(w) => w,
            None => self
                .p
                .windows(false)
                .into_iter()
                .filter(|w| w.pid == target.pid && w.layer == 0 && w.bounds.w > 50.0 && w.bounds.h > 50.0)
                .max_by_key(|w| u8::from(w.on_screen))
                .map(|w| w.window_id)
                .ok_or_else(|| Error::NotFound(format!("window of pid {}", target.pid)))?,
        };
        // Read the tree first (fail closed if it cannot be read in full), then capture.
        let snap = self.snapshot(Target::window(target.pid, wid), Limits { max_depth: 40, max_nodes: 6000 })?;
        self.store.lock().unwrap().retain(|s| s.snap.id != snap.id);
        if snap.truncated {
            return Err(Error::Invalid("accessibility tree too large to verify for redaction; capture refused".into()));
        }
        let bounds = self.p.windows(false).into_iter().find(|w| w.window_id == wid).map(|w| w.bounds);
        self.p.capture(wid, path)?;
        let mut regions = redact::builtin_regions(&snap.root);
        if let Some(b) = bounds {
            let ctx = ShotContext { pid: target.pid, app_id: &app.app_id, window_id: wid, window: b, snapshot: &snap };
            for r in self.redactors.lock().unwrap().iter() {
                regions.extend(r.regions(&ctx));
            }
        }
        let (width, height) = if regions.is_empty() {
            crate::png::dimensions(&std::fs::read(path).unwrap_or_default()).unwrap_or((0, 0))
        } else {
            let Some(b) = bounds else {
                let _ = std::fs::remove_file(path);
                return Err(Error::Invalid("window bounds unknown; capture discarded because regions must be masked".into()));
            };
            match redact::mask_file(path, b, &regions) {
                Ok(d) => d,
                Err(why) => {
                    let _ = std::fs::remove_file(path);
                    return Err(Error::Invalid(format!("screenshot redaction failed ({why}); capture discarded")));
                }
            }
        };
        self.counters.screenshots.fetch_add(1, Ordering::SeqCst);
        self.counters.masked.fetch_add(regions.len() as u64, Ordering::SeqCst);
        self.settle(&eff, &Settled { result: ResultKind::Done, tier_used: None, error: None });
        Ok(ShotReport { window_id: wid, width, height, masked: regions.len() })
    }

    /// The focused element of an app as a one-node summary (no id allocation).
    pub fn focused(&self, pid: i32) -> Result<Option<Node>> {
        self.checked_app(pid)?;
        let Some(el) = self.p.focused(pid)? else { return Ok(None) };
        let Some(raw) = self.p.read(&el) else { return Ok(None) };
        Ok(Some(Node {
            id: "focused".into(),
            role: raw.role,
            native_role: raw.native_role,
            subrole: raw.subrole,
            name: raw.name,
            value: raw.value,
            description: raw.description,
            identifier: raw.identifier,
            enabled: raw.enabled,
            secure: raw.secure,
            frame: raw.frame,
            caps: raw.caps,
            children: vec![],
        }))
    }

    /// Launch an allowlisted app without activating it. The allowlist is checked
    /// before launch when the spec names the app, and again on the launched process.
    pub fn launch(&self, spec: &str) -> Result<AppInfo> {
        let deny = |by: DeniedBy, e: Error| {
            self.denied("none", 0, spec, "launch", by, &e.to_string());
            e
        };
        if killed() {
            return Err(deny(DeniedBy::Policy, Error::Blocked("kill switch engaged".into())));
        }
        // Resolve first, run nothing: an unresolvable or non-allowlisted spec fails closed
        // before any process exists.
        let id = self.p.resolve_launch(spec).map_err(|e| deny(DeniedBy::Policy, e))?;
        let probe = AppInfo { pid: 0, app_id: id, name: String::new(), path: None, started: None };
        self.policy.check(&probe).map_err(|e| deny(DeniedBy::Policy, e))?;
        let eff = self.effect(EffectKind::Launch, 0, spec, "launch", Tier::Semantic);
        self.admit_effect(&eff)?;
        let pid = self.p.launch(spec)?;
        let app = self.p.app(pid).ok_or_else(|| Error::NotFound(format!("launched pid {pid}")))?;
        if let Err(e) = self.policy.check(&app) {
            // Admission already passed on the resolved id; a mismatch means the launch
            // produced something else, so stop it hard (owned) or politely.
            if self.p.kill_owned(pid).is_err() {
                let _ = self.p.terminate(pid);
            }
            return Err(deny(DeniedBy::Policy, e));
        }
        self.settle(&eff, &Settled { result: ResultKind::Done, tier_used: None, error: None });
        Ok(app)
    }

    /// Quit an allowlisted app (used to close what a run opened). Refused while
    /// another controller holds the lease on it.
    pub fn terminate(&self, pid: i32) -> Result<()> {
        let app = self.checked_app(pid)?;
        if let Some(l) = self.leases.status(pid) {
            let e = Error::Blocked(format!("pid {pid} is leased by {} (epoch {}); release the lease first", l.holder, l.epoch));
            self.denied(&self.session_for(pid), pid, &app.app_id, "terminate", DeniedBy::Lease, &e.to_string());
            return Err(e);
        }
        let eff = self.effect(EffectKind::Terminate, pid, &app.app_id, "terminate", Tier::Semantic);
        self.admit_effect(&eff)?;
        self.p.terminate(pid)?;
        self.settle(&eff, &Settled { result: ResultKind::Done, tier_used: None, error: None });
        Ok(())
    }

    // ------------------------------------------------------------ act

    fn with_stored<R>(&self, id: &str, f: impl FnOnce(&Stored<P::El>) -> R) -> Result<R> {
        let st = self.store.lock().unwrap();
        st.iter().rev().find(|s| s.snap.id == id).map(f).ok_or_else(|| Error::UnknownSnapshot(id.to_string()))
    }

    /// Act on `(snapshot, element)`. Always records a receipt, even for refusals.
    ///
    /// Order (EFF-001): validate (kill switch, allowlist, lease fence, freshness,
    /// secure / credential / risk gates) -> approve (admission hook) -> execute
    /// -> settle. A refusal at any step runs nothing.
    pub fn act(&self, snapshot: &str, element: Option<&str>, action: Action, opts: &ActOptions) -> Result<ActOutcome> {
        let mut rc = Receipt {
            seq: self.seq.fetch_add(1, Ordering::SeqCst) + 1,
            session: "none".into(),
            ts_ms: now_ms(),
            pid: 0,
            app_id: String::new(),
            window_id: None,
            snapshot: Some(snapshot.to_string()),
            element: element.map(str::to_string),
            action: action.name().to_string(),
            value: None,
            attempts: vec![],
            tier_used: None,
            frontmost_before: None,
            frontmost_after: None,
            focus_moved: false,
            result: ResultKind::Failed,
            error: None,
            lease: None,
            denied_by: None,
        };
        self.counters.acts.fetch_add(1, Ordering::SeqCst);
        let mut settle_req: Option<EffectRequest> = None;
        let res = self.act_inner(&mut rc, &mut settle_req, snapshot, element, &action, opts);
        if let Some((epoch, seq)) = rc.lease {
            // Processed (done, changed or refused after admission): advance the ack.
            self.leases.ack(rc.pid, epoch, seq);
        }
        match &res {
            Ok(fresh) => {
                rc.result = if fresh.is_some() { ResultKind::Changed } else { ResultKind::Done };
            }
            Err(e) => {
                rc.error = Some(e.to_string());
                let by = match e {
                    Error::Blocked(m) if m.contains("is leased") => Some(DeniedBy::Lease),
                    Error::Blocked(m) if m.contains("secure") || m.contains("credential") => Some(DeniedBy::Safety),
                    Error::Blocked(_) => Some(DeniedBy::Policy),
                    Error::NeedsConfirmation(_) => Some(DeniedBy::Safety),
                    Error::Denied(_) => Some(DeniedBy::Admission),
                    Error::StaleLease { .. } | Error::OutOfOrder { .. } | Error::Replayed { .. } | Error::LeaseHeld { .. } => Some(DeniedBy::Lease),
                    _ => None,
                };
                rc.result = if by.is_some() { ResultKind::Blocked } else { ResultKind::Failed };
                rc.denied_by = by;
                if let Some(by) = by {
                    self.denied(&rc.session, rc.pid, &rc.app_id, &rc.action, by, &e.to_string());
                }
                match e {
                    Error::StaleLease { presented, current } => {
                        self.counters.fenced.fetch_add(1, Ordering::SeqCst);
                        self.journal(LifecycleEvent::LeaseFenced { pid: rc.pid, presented_epoch: *presented, current_epoch: *current });
                    }
                    Error::OutOfOrder { .. } => {
                        self.counters.fenced.fetch_add(1, Ordering::SeqCst);
                    }
                    Error::Replayed { .. } => {
                        self.counters.replayed.fetch_add(1, Ordering::SeqCst);
                    }
                    _ => {}
                }
                if rc.result == ResultKind::Failed && self.p.app(rc.pid).is_none() && rc.pid != 0 {
                    // The target went away mid-act: tell the journal, end the session, drop the lease.
                    self.poll_targets();
                }
            }
        }
        match rc.result {
            ResultKind::Done => {
                self.counters.acts_done.fetch_add(1, Ordering::SeqCst);
                if let Some(t) = rc.tier_used {
                    self.counters.by_tier[t as usize - 1].fetch_add(1, Ordering::SeqCst);
                }
            }
            ResultKind::Failed => {
                self.counters.acts_failed.fetch_add(1, Ordering::SeqCst);
            }
            _ => {}
        }
        self.record(&rc);
        if let Some(req) = settle_req {
            self.settle(&req, &Settled { result: rc.result, tier_used: rc.tier_used, error: rc.error.clone() });
        }
        res.map(|fresh| ActOutcome { receipt: rc, fresh })
    }

    fn act_inner(
        &self,
        rc: &mut Receipt,
        settle_req: &mut Option<EffectRequest>,
        snapshot: &str,
        element: Option<&str>,
        action: &Action,
        opts: &ActOptions,
    ) -> Result<Option<Snapshot>> {
        if killed() {
            return Err(Error::Blocked("kill switch engaged".into()));
        }
        let (pid, app_id, window_id, target, limits, stored_fp) =
            self.with_stored(snapshot, |s| (s.snap.pid, s.snap.app_id.clone(), s.snap.window_id, s.target.clone(), s.limits, s.snap.fingerprint))?;
        rc.pid = pid;
        rc.app_id = app_id;
        rc.window_id = window_id;
        rc.session = self.session_for(pid);
        self.checked_app(pid)?;
        // Lease fence: a leased app only takes inputs from the current epoch, in order.
        if let Admitted::Fenced { epoch, seq } = self.leases.admit(pid, opts.fence)? {
            rc.lease = Some((epoch, seq));
        }

        let (el, label, frame) = match element {
            Some(id) => self.with_stored(snapshot, |s| {
                s.els.get(id).cloned().map(|e| (e, s.labels.get(id).cloned(), s.frames.get(id).copied()))
            })?
            .map(|(e, l, f)| (Some(e), l, f))
            .ok_or_else(|| Error::NotFound(format!("element {id} in {snapshot}")))?,
            None => (None, None, None),
        };

        // Freshness guard.
        match opts.guard {
            Guard::Off => {}
            Guard::Element => {
                if let (Some(e), Some((role, name, _))) = (&el, &label) {
                    let live = self.p.read(e);
                    let same = live.as_ref().is_some_and(|r| {
                        r.role == *role && r.name.clone().or_else(|| r.description.clone()).or_else(|| r.identifier.clone()) == *name
                    });
                    if !same {
                        return Ok(Some(self.snapshot(target, limits)?));
                    }
                }
            }
            Guard::Structure => {
                let fresh = self.snapshot(target, limits)?;
                if fresh.fingerprint != stored_fp {
                    return Ok(Some(fresh));
                }
                // Same structure: drop the duplicate so the caller's snapshot id survives.
                self.store.lock().unwrap().pop_back();
            }
        }

        // Risk and secure-field gates.
        let secure = label.as_ref().is_some_and(|l| l.2);
        let risk_label: Option<String> = match action {
            Action::MenuPress(p) => p.last().cloned(),
            Action::Press | Action::Click { .. } => label.as_ref().and_then(|l| l.1.clone()),
            _ => None,
        };
        if let Some(l) = risk_label {
            if let Some(class) = classify_risk(&l) {
                if !opts.confirm.contains(&class) {
                    return Err(Error::NeedsConfirmation(class));
                }
            }
        }
        if secure && matches!(action, Action::SetValue(_) | Action::TypeText(_) | Action::Keys(_)) {
            return Err(Error::Blocked("target is a secure (password) field".into()));
        }
        if matches!(action, Action::TypeText(_) | Action::Keys(_)) && el.is_none() {
            // Typing at the focus: refuse when the focused field is secure or a credential field.
            if let Ok(Some(f)) = self.p.focused(pid) {
                if let Some(r) = self.p.read(&f) {
                    if r.secure {
                        return Err(Error::Blocked("focused field is a secure (password) field".into()));
                    }
                    let l = [&r.name, &r.description, &r.identifier].iter().filter_map(|x| x.as_deref()).collect::<Vec<_>>().join(" ");
                    if credential_label(&l) {
                        return Err(Error::Blocked("focused field is labelled as a credential field; credentials are never typed".into()));
                    }
                }
            }
        }
        // Credentials are never typed: not by payload shape, not into a credential-labelled field.
        if let Action::SetValue(v) | Action::TypeText(v) = action {
            if looks_secret(v) {
                return Err(Error::Blocked("payload looks like a credential; credentials are never typed".into()));
            }
            if let Some((role, name, _)) = &label {
                if matches!(role.as_str(), "textfield" | "textarea" | "combobox") && credential_label(name.as_deref().unwrap_or("")) {
                    return Err(Error::Blocked("target is labelled as a credential field; credentials are never typed".into()));
                }
            }
        }
        rc.value = action.payload().map(|v| redact(v, secure, opts.log_values));

        // Approve (EFF-001): last gate before any side effect.
        let mut max_tier = opts.max_tier;
        {
            let mut eff = self.effect(EffectKind::Act, pid, &rc.app_id, action.name(), opts.max_tier);
            eff.target = label.as_ref().map(|l| format!("{}:{}", l.0, l.1.clone().unwrap_or_default()));
            eff.risk = match action {
                Action::MenuPress(p) => p.last().and_then(|l| classify_risk(l)),
                Action::Press | Action::Click { .. } => label.as_ref().and_then(|l| l.1.as_deref()).and_then(classify_risk),
                _ => None,
            };
            eff.payload_chars = action.payload().map(|v| v.chars().count());
            eff.lease_epoch = rc.lease.map(|l| l.0);
            if let Some(cap) = self.approve(&eff)? {
                max_tier = max_tier.min(cap);
            }
            *settle_req = Some(eff);
        }

        let front0 = self.front_pid();
        rc.frontmost_before = front0;

        // The ladder.
        let mut last_err: Option<Error> = None;
        let mut done = false;
        for tier in [Tier::Semantic, Tier::InApp, Tier::Targeted, Tier::Foreground] {
            if tier > max_tier {
                break;
            }
            if tier < opts.min_tier {
                continue;
            }
            if killed() {
                return Err(Error::Blocked("kill switch engaged".into()));
            }
            match self.try_tier(tier, pid, window_id, el.as_ref(), frame, action, front0) {
                Ok(detail) => {
                    rc.attempts.push(Attempt { tier, ok: true, detail });
                    rc.tier_used = Some(tier);
                    done = true;
                    break;
                }
                Err(f) => {
                    rc.attempts.push(Attempt { tier, ok: false, detail: f.error.to_string() });
                    let stop = f.stop;
                    last_err = Some(f.error);
                    if stop {
                        break;
                    }
                }
            }
        }
        if opts.settle_ms > 0 {
            std::thread::sleep(Duration::from_millis(opts.settle_ms));
        }
        let front1 = self.front_pid();
        rc.frontmost_after = front1;
        rc.focus_moved = rc.tier_used != Some(Tier::Foreground) && front0.is_some() && front0 != front1;
        if rc.focus_moved && opts.strict_focus {
            if let Some(b) = front0 {
                self.restore_front(b);
            }
            return Err(Error::FocusMoved { before: front0, after: front1 });
        }
        if done {
            Ok(None)
        } else {
            Err(match last_err {
                Some(Error::Unsupported(s)) => Error::NoTier(format!("{} ({s}) within {}", action.name(), max_tier.label())),
                Some(e) => e,
                None => Error::NoTier(action.name().to_string()),
            })
        }
    }

    /// Bring `pid` to the front and confirm it. Activation is cooperative on
    /// current macOS, so ask once, poll, and re-ask a few times within ~1.2 s.
    pub(crate) fn restore_front(&self, pid: i32) -> bool {
        if self.front_pid() == Some(pid) {
            return true;
        }
        for round in 0..5 {
            self.p.activate(pid);
            for _ in 0..8 {
                std::thread::sleep(Duration::from_millis(30));
                if self.front_pid() == Some(pid) {
                    return true;
                }
            }
            let _ = round;
        }
        self.front_pid() == Some(pid)
    }

    /// Run a sequence of acts; the kill switch is honoured between steps.
    pub fn act_batch(&self, steps: &[(String, Option<String>, Action)], opts: &ActOptions) -> Vec<Result<ActOutcome>> {
        let mut out = vec![];
        for (snap, el, action) in steps {
            if killed() {
                out.push(Err(Error::Blocked("kill switch engaged".into())));
                break;
            }
            let r = self.act(snap, el.as_deref(), action.clone(), opts);
            let stop = r.as_ref().map(|o| !o.done()).unwrap_or(true);
            out.push(r);
            if stop {
                break;
            }
        }
        out
    }

    // ------------------------------------------------------------ tiers

    fn events_for(&self, action: &Action, frame: Option<Rect>, window_center: Option<(f64, f64)>) -> std::result::Result<Vec<InputEvent>, TierFail> {
        let fail = |e: Error| TierFail { error: e, stop: false };
        let center = frame.map(|f| f.center()).or(window_center);
        Ok(match action {
            Action::Press => {
                let (x, y) = center.ok_or_else(|| fail(Error::Unsupported("no frame to click")))?;
                vec![InputEvent::Click { x, y, button: Button::Left, count: 1 }]
            }
            Action::Click { point, button, count } => {
                let (x, y) = point.or(center).ok_or_else(|| fail(Error::Unsupported("no point to click")))?;
                vec![InputEvent::Click { x, y, button: *button, count: *count }]
            }
            Action::TypeText(t) => vec![InputEvent::Text(t.clone())],
            Action::SetValue(t) => {
                let all = keys::parse_chord("mod+a", self.primary_meta()).map_err(fail)?;
                vec![InputEvent::Key(all), InputEvent::Text(t.clone())]
            }
            Action::Keys(spec) => {
                keys::parse_sequence(spec, self.primary_meta()).map_err(fail)?.into_iter().map(InputEvent::Key).collect()
            }
            Action::Scroll { dx, dy } => {
                let (x, y) = center.ok_or_else(|| fail(Error::Unsupported("no point to scroll at")))?;
                vec![InputEvent::Scroll { x, y, dx: *dx, dy: *dy }]
            }
            Action::Focus | Action::MenuPress(_) => return Err(fail(Error::Unsupported("needs a semantic tier"))),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn try_tier(
        &self,
        tier: Tier,
        pid: i32,
        window: Option<u64>,
        el: Option<&P::El>,
        frame: Option<Rect>,
        action: &Action,
        front0: Option<i32>,
    ) -> std::result::Result<String, TierFail> {
        let skip = |s: &'static str| TierFail { error: Error::Unsupported(s), stop: false };
        match tier {
            Tier::Semantic => match action {
                Action::MenuPress(path) => self.p.menu_press(pid, path).map(|_| "menu AXPress".to_string()).map_err(TierFail::soft),
                Action::TypeText(text) => self.semantic_type(pid, el, text),
                Action::Press | Action::SetValue(_) | Action::Scroll { .. } | Action::Focus => {
                    let e = el.ok_or_else(|| TierFail { error: Error::Invalid(format!("{} needs an element", action.name())), stop: true })?;
                    self.p.semantic(e, action).map_err(TierFail::soft)
                }
                Action::Click { point: None, button: Button::Left, count: 1 } => match el {
                    Some(e) => self.p.semantic(e, &Action::Press).map_err(TierFail::soft),
                    None => Err(skip("click needs an element or a point")),
                },
                _ => Err(skip("no semantic form")),
            },
            Tier::InApp => {
                let bridge = self.in_app.lock().unwrap().iter().find(|b| b.handles(pid)).cloned();
                let Some(bridge) = bridge else { return Err(skip("no in-app channel for this pid")) };
                let evs = match action {
                    Action::TypeText(t) => vec![InputEvent::Text(t.clone())],
                    Action::Keys(_) => self.events_for(action, None, None)?,
                    // In-app clicks use viewport coordinates, so only explicit points are valid.
                    Action::Click { point: Some((x, y)), button, count } => vec![InputEvent::Click { x: *x, y: *y, button: *button, count: *count }],
                    _ => return Err(skip("in-app channel cannot do this")),
                };
                let mut last = String::new();
                for ev in &evs {
                    last = bridge.perform(pid, ev).map_err(|e| TierFail { error: e, stop: false })?;
                }
                Ok(last)
            }
            Tier::Targeted | Tier::Foreground => {
                let wc = window.and_then(|w| self.p.windows(false).into_iter().find(|x| x.window_id == w)).map(|w| w.bounds.center());
                let evs = self.events_for(action, frame, wc)?;
                if tier == Tier::Targeted {
                    if matches!(action, Action::SetValue(_)) {
                        if let Some(e) = el {
                            let _ = self.p.semantic(e, &Action::Focus);
                        }
                    }
                    let mut n = 0;
                    for ev in &evs {
                        self.p.post(pid, window, ev).map_err(|e| TierFail { error: e, stop: n > 0 })?;
                        n += 1;
                        if matches!(ev, InputEvent::Key(_)) && evs.len() > 1 {
                            std::thread::sleep(Duration::from_millis(35));
                        }
                    }
                    let unverifiable = evs.iter().any(|e| matches!(e, InputEvent::Click { .. } | InputEvent::Scroll { .. }));
                    Ok(format!("{n} event(s) posted to pid {pid}{}", if unverifiable { " (delivery unverifiable: pointer events can be ignored by background windows)" } else { "" }))
                } else {
                    if !self.restore_front(pid) {
                        return Err(TierFail { error: Error::Platform(0, "could not activate target"), stop: false });
                    }
                    let mut n = 0;
                    for ev in &evs {
                        self.p.foreground(pid, ev).map_err(|e| TierFail { error: e, stop: n > 0 })?;
                        n += 1;
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    if let Some(b) = front0 {
                        if b != pid {
                            self.restore_front(b);
                        }
                    }
                    Ok(format!("{n} foreground event(s); focus restored"))
                }
            }
        }
    }

    /// Tier-1 text insertion: selection-exact native write, confirmed by readback.
    fn semantic_type(&self, pid: i32, el: Option<&P::El>, text: &str) -> std::result::Result<String, TierFail> {
        let target = match el {
            Some(e) => e.clone(),
            None => self.p.focused(pid).ok().flatten().ok_or(TierFail { error: Error::Unsupported("no focused element"), stop: false })?,
        };
        if !self.p.native_write_capable(&target) {
            return Err(TierFail { error: Error::Unsupported("element has no selection-exact native write"), stop: false });
        }
        let baseline = self.p.field_text(&target).ok_or(TierFail { error: Error::Unsupported("field unreadable"), stop: false })?;
        self.p.native_write(&target, text).map_err(TierFail::soft)?;
        if self.confirm_readback(&target, &baseline, text, Duration::from_millis(500)) {
            Ok("selected-text write, read-back confirmed".into())
        } else {
            // Acknowledged but unproven: never escalate to a second insertion.
            Err(TierFail { error: Error::Platform(0, "native write acknowledged but not confirmed"), stop: true })
        }
    }

    pub(crate) fn confirm_readback(&self, el: &P::El, baseline: &crate::proof::FieldText, needle: &str, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(cur) = self.p.field_text(el) {
                if crate::proof::confirm_insertion(&baseline.text, baseline.selection, &cur.text, needle) {
                    return true;
                }
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// A failed tier attempt. `stop` means a side effect may have happened, so the
/// ladder must not climb (a second insertion could double the text).
pub(crate) struct TierFail {
    pub error: Error,
    pub stop: bool,
}
impl TierFail {
    fn soft(e: Error) -> Self {
        TierFail { error: e, stop: false }
    }
}
