//! Text delivery to the focused field with HeardRight-grade semantics, brand
//! neutral: pin the editor, veto secure fields, take a baseline read, climb the
//! tier ladder, and call it inserted only on an attributable read-back delta.
//! A posted key or a successful setter is never proof, and a posted-but-unproven
//! insertion is never retried (no double insertion).

use crate::admission::EffectKind;
use crate::engine::Automation;
use crate::lease::Admitted;
use crate::keys;
use crate::model::*;
use crate::platform::{InputEvent, Platform};
use crate::policy::{credential_label, killed, looks_secret, Attempt};
use crate::proof::FieldText;
use std::time::{Duration, Instant};

/// The exact editor retained at capture time. A pid alone cannot identify a
/// field after focus changes; a later field's text is not insertion proof.
#[derive(Clone)]
pub struct FocusPin<E> {
    pub pid: i32,
    pub app_id: String,
    pub window_id: Option<u64>,
    /// Process start time: identity across pid reuse.
    pub started: Option<u64>,
    pub editor: E,
    pub role: String,
    /// `None` when the role could not prove either way (unknown never vetoes).
    pub secure: Option<bool>,
    pub captured_ms: u64,
    /// Fingerprint of the editor's text at pin time (never the text), for rule 3.
    pub fingerprint: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct DeliveryOptions {
    /// Highest tier. `Foreground` (clipboard paste with the app activated) is opt-in.
    pub max_tier: Tier,
    pub send_enter: bool,
    /// Leave the text on the clipboard when nothing could be delivered (opt-in).
    pub fallback_copy: bool,
    /// Climb after a tier that provably changed nothing (never after an unproven change).
    pub escalate_when_nothing_landed: bool,
    pub readback_ms: u64,
    /// Lease fence `{epoch, seq}`; required while the app is leased.
    pub fence: Option<crate::lease::Fence>,
}

impl Default for DeliveryOptions {
    fn default() -> Self {
        DeliveryOptions {
            max_tier: Tier::Targeted,
            send_enter: false,
            fallback_copy: false,
            escalate_when_nothing_landed: true,
            readback_ms: 800,
            fence: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Why {
    EmptyText,
    NotTrusted,
    Blocked(String),
    SecureTarget,
    FocusChanged,
    NoTierAllowed,
    ClipboardLossy,
    ClipboardUnavailable,
    PostFailed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeliveryOutcome {
    /// Confirmed by an attributable read-back delta.
    Inserted { tier: Tier },
    /// Posted, but no proof could be gathered. Never retried, never submitted.
    SentUnconfirmed { tier: Tier, reason: String },
    NotDelivered(Why),
    /// Not delivered; the text was left on the clipboard because the caller opted in.
    CopiedFallback(Why),
}

#[derive(Debug, Clone)]
pub struct DeliveryRecord {
    pub outcome: DeliveryOutcome,
    pub attempts: Vec<Attempt>,
    pub steps: Vec<(String, u64)>,
    pub chars: usize,
    pub enter_sent: bool,
    pub total_ms: u64,
}

impl DeliveryRecord {
    pub fn inserted(&self) -> bool {
        matches!(self.outcome, DeliveryOutcome::Inserted { .. })
    }
}

enum Verdict {
    Confirmed,
    NothingLanded,
    Unproven(String),
}

impl<P: Platform> Automation<P> {
    /// Capture the focused editor of `pid` (default: the frontmost app).
    pub fn capture_pin(&self, pid: Option<i32>) -> Result<FocusPin<P::El>> {
        let pid = match pid {
            Some(p) => p,
            None => self.front_pid().ok_or_else(|| Error::NotFound("frontmost app".into()))?,
        };
        let app = self.checked_app(pid)?;
        let editor = self.p.focused(pid)?.ok_or_else(|| Error::NotFound("focused element".into()))?;
        let raw = self.p.read(&editor).ok_or_else(|| Error::NotFound("focused element vanished".into()))?;
        Ok(FocusPin {
            pid,
            app_id: app.app_id,
            window_id: self.p.window_of(&editor),
            started: app.started,
            secure: Some(raw.secure),
            role: raw.role,
            fingerprint: self.p.field_text(&editor).map(|f| crate::proof::text_fingerprint(&f.text)),
            editor,
            captured_ms: now_ms(),
        })
    }

    /// Spec 5.1 rule 3: the pinned editor is gone (the app destroyed and
    /// re-created its composer). Returns a pin on the replacement only when it is
    /// provably the same box: original positively gone on two reads, same process
    /// instance and window, positively non-secure, identical text fingerprint.
    pub fn replacement_pin(&self, pin: &FocusPin<P::El>) -> Option<FocusPin<P::El>> {
        if self.p.read(&pin.editor).is_some() {
            return None;
        }
        std::thread::sleep(Duration::from_millis(40));
        let gone = self.p.read(&pin.editor).is_none();
        let app = self.p.app(pin.pid)?;
        if app.started != pin.started || self.policy.check(&app).is_err() {
            return None;
        }
        let cand = self.capture_pin(Some(pin.pid)).ok()?;
        let same = cand.window_id == pin.window_id && cand.started == pin.started;
        crate::proof::replacement_editor_may_receive(gone, same, cand.secure, cand.fingerprint, pin.fingerprint).then_some(cand)
    }

    /// The pinned editor is still the live focused editor of the same process.
    pub fn pin_is_current(&self, pin: &FocusPin<P::El>) -> bool {
        let Some(app) = self.p.app(pin.pid) else { return false };
        if app.started != pin.started || self.policy.check(&app).is_err() {
            return false;
        }
        if self.p.read(&pin.editor).is_none() {
            return false;
        }
        matches!(self.p.focused(pin.pid), Ok(Some(f)) if self.p.same(&f, &pin.editor))
    }

    pub fn deliver_text(&self, pin: &FocusPin<P::El>, text: &str, opts: &DeliveryOptions) -> DeliveryRecord {
        let started = Instant::now();
        let mut rec = DeliveryRecord {
            outcome: DeliveryOutcome::NotDelivered(Why::NoTierAllowed),
            attempts: vec![],
            steps: vec![],
            chars: text.chars().count(),
            enter_sent: false,
            total_ms: 0,
        };
        self.counters.deliveries.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let outcome = match self.delivery_gate(pin, text, opts) {
            Err(why) => DeliveryOutcome::NotDelivered(why),
            Ok((lease, eff, cap)) => {
                let mut o = opts.clone();
                if let Some(c) = cap {
                    o.max_tier = o.max_tier.min(c);
                }
                let out = self.deliver_inner(pin, text, &o, &mut rec);
                if let Some((epoch, seq)) = lease {
                    self.leases.ack(pin.pid, epoch, seq);
                }
                let result = match &out {
                    DeliveryOutcome::Inserted { .. } => crate::policy::ResultKind::Done,
                    DeliveryOutcome::SentUnconfirmed { .. } => crate::policy::ResultKind::Done,
                    DeliveryOutcome::NotDelivered(Why::Blocked(_)) | DeliveryOutcome::NotDelivered(Why::SecureTarget) => crate::policy::ResultKind::Blocked,
                    _ => crate::policy::ResultKind::Failed,
                };
                let tier_used = match &out {
                    DeliveryOutcome::Inserted { tier } | DeliveryOutcome::SentUnconfirmed { tier, .. } => Some(*tier),
                    _ => None,
                };
                if let Some(eff) = eff {
                    self.settle(&eff, &crate::admission::Settled { result, tier_used, error: None });
                }
                out
            }
        };
        rec.outcome = match outcome {
            DeliveryOutcome::NotDelivered(reason) if opts.fallback_copy && Self::copyable(&reason) => {
                if self.p.clip_write(text).is_some() {
                    DeliveryOutcome::CopiedFallback(reason)
                } else {
                    DeliveryOutcome::NotDelivered(reason)
                }
            }
            o => o,
        };
        rec.total_ms = started.elapsed().as_millis() as u64;
        rec
    }

    /// Validate -> approve for one delivery: lease fence, credential gates, admission.
    #[allow(clippy::type_complexity)]
    fn delivery_gate(&self, pin: &FocusPin<P::El>, text: &str, opts: &DeliveryOptions) -> std::result::Result<(Option<(u64, u64)>, Option<crate::admission::EffectRequest>, Option<Tier>), Why> {
        use crate::events::DeniedBy;
        let session = self.session_for(pin.pid);
        let refuse = |by: DeniedBy, e: Error| {
            self.denied(&session, pin.pid, &pin.app_id, "deliver", by, &e.to_string());
            Why::Blocked(e.to_string())
        };
        if looks_secret(text) {
            return Err(refuse(DeniedBy::Safety, Error::Blocked("payload looks like a credential; credentials are never typed".into())));
        }
        if let Some(raw) = self.p.read(&pin.editor).filter(|r| !r.secure && pin.secure != Some(true)) {
            // (A positively secure field is vetoed by `deliver_inner` as `SecureTarget`.)
            let l = [&raw.name, &raw.description, &raw.identifier].iter().filter_map(|x| x.as_deref()).collect::<Vec<_>>().join(" ");
            if credential_label(&l) {
                return Err(refuse(DeniedBy::Safety, Error::Blocked("target is labelled as a credential field; credentials are never typed".into())));
            }
        }
        let lease = match self.leases.admit(pin.pid, opts.fence) {
            Ok(Admitted::Fenced { epoch, seq }) => Some((epoch, seq)),
            Ok(Admitted::Unleased) => None,
            Err(e) => {
                if matches!(e, Error::Replayed { .. }) {
                    self.counters.replayed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                return Err(refuse(DeniedBy::Lease, e));
            }
        };
        let mut eff = self.effect(EffectKind::Deliver, pin.pid, &pin.app_id, "deliver_text", opts.max_tier);
        eff.payload_chars = Some(text.chars().count());
        eff.lease_epoch = lease.map(|l| l.0);
        match self.admit_effect(&eff) {
            Ok(cap) => Ok((lease, Some(eff), cap)),
            Err(e) => {
                if let Some((epoch, seq)) = lease {
                    self.leases.ack(pin.pid, epoch, seq);
                }
                Err(Why::Blocked(e.to_string()))
            }
        }
    }

    fn copyable(r: &Why) -> bool {
        !matches!(r, Why::EmptyText | Why::Blocked(_) | Why::ClipboardLossy | Why::ClipboardUnavailable)
    }

    fn deliver_inner(&self, pin: &FocusPin<P::El>, text: &str, opts: &DeliveryOptions, rec: &mut DeliveryRecord) -> DeliveryOutcome {
        use DeliveryOutcome::*;
        let t0 = Instant::now();
        let step = |rec: &mut DeliveryRecord, name: &str| rec.steps.push((name.to_string(), t0.elapsed().as_millis() as u64));

        if text.trim().is_empty() {
            return NotDelivered(Why::EmptyText);
        }
        if killed() {
            return NotDelivered(Why::Blocked("kill switch engaged".into()));
        }
        if !self.p.status().accessibility {
            return NotDelivered(Why::NotTrusted);
        }
        step(rec, "accessibility_check");
        if let Err(e) = self.checked_app(pin.pid) {
            return NotDelivered(Why::Blocked(e.to_string()));
        }
        if !self.pin_is_current(pin) {
            return NotDelivered(Why::FocusChanged);
        }
        step(rec, "pin_revalidated");
        // Positive secure-field evidence is the only security veto; unknown never vetoes.
        let live_secure = self.p.read(&pin.editor).map(|r| r.secure);
        if pin.secure == Some(true) || live_secure == Some(true) {
            return NotDelivered(Why::SecureTarget);
        }
        let baseline: Option<FieldText> = self.p.field_text(&pin.editor);
        step(rec, if baseline.is_some() { "baseline_captured" } else { "baseline_unavailable" });
        let readback = Duration::from_millis(opts.readback_ms);

        let mut last_fail: Option<Why> = None;
        let mut inserted_tier: Option<Tier> = None;

        // Tier 1: selection-exact native write.
        if opts.max_tier >= Tier::Semantic {
            if let Some(b) = baseline.as_ref().filter(|_| self.p.native_write_capable(&pin.editor)) {
                match self.p.native_write(&pin.editor, text) {
                    Ok(()) => {
                        let ok = self.confirm_readback(&pin.editor, b, text, readback);
                        rec.attempts.push(Attempt { tier: Tier::Semantic, ok, detail: "selected-text write".into() });
                        step(rec, "native_write");
                        if ok {
                            inserted_tier = Some(Tier::Semantic);
                        } else {
                            return SentUnconfirmed { tier: Tier::Semantic, reason: "native_write_unconfirmed".into() };
                        }
                    }
                    Err(e) => rec.attempts.push(Attempt { tier: Tier::Semantic, ok: false, detail: e.to_string() }),
                }
            } else {
                rec.attempts.push(Attempt { tier: Tier::Semantic, ok: false, detail: "no selection-exact native write for this editor".into() });
            }
        }

        // Tier 2 and 3: events that carry the text.
        for tier in [Tier::InApp, Tier::Targeted] {
            if inserted_tier.is_some() || tier > opts.max_tier {
                continue;
            }
            let ev = InputEvent::Text(text.to_string());
            let sent = match tier {
                Tier::InApp => {
                    let b = self.in_app_for(pin.pid);
                    match b {
                        Some(b) => b.perform(pin.pid, &ev),
                        None => Err(Error::Unsupported("no in-app channel")),
                    }
                }
                _ => self.p.post(pin.pid, pin.window_id, &ev).map(|_| "text events posted".to_string()),
            };
            match sent {
                Err(Error::Unsupported(s)) => rec.attempts.push(Attempt { tier, ok: false, detail: s.into() }),
                Err(e) => {
                    rec.attempts.push(Attempt { tier, ok: false, detail: e.to_string() });
                    last_fail = Some(Why::PostFailed(e.to_string()));
                }
                Ok(detail) => {
                    step(rec, "text_posted");
                    match self.verdict(pin, baseline.as_ref(), text, readback) {
                        Verdict::Confirmed => {
                            rec.attempts.push(Attempt { tier, ok: true, detail });
                            inserted_tier = Some(tier);
                        }
                        Verdict::NothingLanded if opts.escalate_when_nothing_landed => {
                            rec.attempts.push(Attempt { tier, ok: false, detail: "posted but nothing landed".into() });
                        }
                        Verdict::NothingLanded => return SentUnconfirmed { tier, reason: "nothing_landed".into() },
                        Verdict::Unproven(why) => {
                            rec.attempts.push(Attempt { tier, ok: false, detail: why.clone() });
                            return SentUnconfirmed { tier, reason: why };
                        }
                    }
                }
            }
        }

        // Tier 4: clipboard paste with the app activated (explicit opt-in).
        if inserted_tier.is_none() && opts.max_tier >= Tier::Foreground {
            match self.paste_foreground(pin, baseline.as_ref(), text, readback, rec) {
                Ok(()) => inserted_tier = Some(Tier::Foreground),
                Err(Err4::Done(o)) => return o,
                Err(Err4::Fail(r)) => last_fail = Some(r),
            }
        }

        let Some(tier) = inserted_tier else {
            return NotDelivered(last_fail.unwrap_or(Why::NoTierAllowed));
        };

        if opts.send_enter {
            // Receipt of insertion already happened; Enter only if the pinned editor is still current.
            if self.pin_is_current(pin) {
                if let Ok(c) = keys::parse_chord("return", self.primary_meta()) {
                    let ev = InputEvent::Key(c);
                    let res = if tier == Tier::Foreground {
                        self.p.foreground(pin.pid, &ev)
                    } else if opts.max_tier >= Tier::Targeted {
                        self.p.post(pin.pid, pin.window_id, &ev)
                    } else {
                        Err(Error::Unsupported("enter needs tier 3"))
                    };
                    rec.enter_sent = res.is_ok();
                    step(rec, if rec.enter_sent { "enter_sent" } else { "enter_failed" });
                }
            } else {
                step(rec, "enter_skipped_focus_changed");
            }
        }
        Inserted { tier }
    }

    fn in_app_for(&self, pid: i32) -> Option<std::sync::Arc<dyn crate::in_app::InApp>> {
        self.in_app_bridge(pid)
    }

    fn verdict(&self, pin: &FocusPin<P::El>, baseline: Option<&FieldText>, text: &str, timeout: Duration) -> Verdict {
        let Some(b) = baseline else { return Verdict::Unproven("readback_unavailable".into()) };
        if self.confirm_readback(&pin.editor, b, text, timeout) {
            return Verdict::Confirmed;
        }
        match self.p.field_text(&pin.editor) {
            Some(cur) if cur.text == b.text => Verdict::NothingLanded,
            Some(_) => Verdict::Unproven("delta_not_attributable".into()),
            None => Verdict::Unproven("readback_lost".into()),
        }
    }

    fn paste_foreground(
        &self,
        pin: &FocusPin<P::El>,
        baseline: Option<&FieldText>,
        text: &str,
        readback: Duration,
        rec: &mut DeliveryRecord,
    ) -> std::result::Result<(), Err4> {
        use DeliveryOutcome::*;
        let fail = |r: Why| Err4::Fail(r);
        let front0 = self.front_pid();
        // A lossy snapshot makes a destructive paste unsafe: refuse before touching the clipboard.
        let Some(snap) = self.p.clip_snapshot() else { return Err(fail(Why::ClipboardUnavailable)) };
        if !snap.lossless {
            return Err(fail(Why::ClipboardLossy));
        }
        let Some(staged) = self.p.clip_write(text) else { return Err(fail(Why::ClipboardUnavailable)) };
        let restore = |p: &Self| {
            p.p.clip_restore(&snap, staged);
        };
        if !self.pin_is_current(pin) || !self.restore_front(pin.pid) {
            restore(self);
            return Err(fail(Why::FocusChanged));
        }
        let chord = match keys::parse_chord("mod+v", self.primary_meta()) {
            Ok(c) => c,
            Err(_) => {
                restore(self);
                return Err(fail(Why::PostFailed("paste chord".into())));
            }
        };
        let posted = self.p.foreground(pin.pid, &InputEvent::Key(chord));
        if let Err(e) = posted {
            restore(self);
            self.give_back_front(front0, pin.pid);
            rec.attempts.push(Attempt { tier: Tier::Foreground, ok: false, detail: e.to_string() });
            return Err(fail(Why::PostFailed(e.to_string())));
        }
        let verdict = self.verdict(pin, baseline, text, readback);
        self.give_back_front(front0, pin.pid);
        match verdict {
            Verdict::Confirmed => {
                // Restore only if nobody else wrote the clipboard meanwhile.
                self.p.clip_restore(&snap, staged);
                rec.attempts.push(Attempt { tier: Tier::Foreground, ok: true, detail: "clipboard paste, read-back confirmed, clipboard restored".into() });
                Ok(())
            }
            Verdict::NothingLanded => {
                restore(self);
                rec.attempts.push(Attempt { tier: Tier::Foreground, ok: false, detail: "paste landed nothing".into() });
                Err(fail(Why::PostFailed("paste landed nothing".into())))
            }
            Verdict::Unproven(why) => {
                // Staged text stays on the clipboard so the user can recover it.
                rec.attempts.push(Attempt { tier: Tier::Foreground, ok: false, detail: why.clone() });
                Err(Err4::Done(SentUnconfirmed { tier: Tier::Foreground, reason: why }))
            }
        }
    }

    fn give_back_front(&self, front0: Option<i32>, target: i32) {
        if let Some(b) = front0 {
            if b != target {
                self.restore_front(b);
            }
        }
    }
}

enum Err4 {
    Done(DeliveryOutcome),
    Fail(Why),
}
