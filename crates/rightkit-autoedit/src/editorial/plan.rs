//! The editorial plan: analysis, per-item review and resolution into one
//! undoable timeline action group.

use std::collections::BTreeMap;

use crate::cuts::{
    apply_group, plan_cuts, self_talk, soft_filler_gate, undo_group, ActionGroup, AppliedGroup,
    CutPlan, Decision, KeptRange, ManualRemoval, RealiseOptions, Removal, RemovalReason,
    RuleConfig, TimeMap, Verdict,
};
use rightkit_timeline::{Fps, Ticks, TimelineStore};
use rightkit_transcript::Transcript;
use serde::{Deserialize, Serialize};

use crate::editorial::beats::{self, Beat};
use crate::editorial::config::EditorialConfig;
use crate::editorial::error::EditorialError;
use crate::editorial::item::{
    word_draft, Category, Draft, EditorialItem, Evidence, ItemAction, ItemKind, Tier,
};
use crate::editorial::retake::TakeGroup;
use crate::editorial::util::{first_word_at, join, words_of, Wd};
use crate::editorial::{deadair, disfluency, retake};

pub const SCHEMA: &str = "cutright.editorial.v1";
const TICKS_PER_MS: i64 = 705_600;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditorialPlan {
    pub schema: String,
    /// `ed-<8 hex>` over the items and configuration.
    pub id: String,
    pub source_blake3: String,
    pub fps: Fps,
    pub source_duration_ms: u64,
    pub config: EditorialConfig,
    /// Proposed edits in source-time order.
    pub items: Vec<EditorialItem>,
    /// Narrative structure of the transcript (annotation; never reorders).
    pub beats: Vec<Beat>,
    /// Retake groups: every take ranked and explained; the chosen one is kept.
    pub take_groups: Vec<TakeGroup>,
    /// Retake cases the deterministic rules kept and marked for a later judge
    /// (self-talk and soft-filler escalations). Nothing here is cut.
    #[serde(default)]
    pub retake_escalations: Vec<RetakeEscalation>,
}

/// A retake case kept by a deterministic rule and marked for escalation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetakeEscalation {
    /// `self_talk` or `soft_filler`.
    pub rule: String,
    /// First word index (transcript word order).
    pub first: usize,
    /// Exclusive end word index.
    pub end: usize,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    pub reason: String,
}

fn escalation(
    words: &[Wd],
    rule: &str,
    first: usize,
    end: usize,
    reason: String,
) -> RetakeEscalation {
    RetakeEscalation {
        rule: rule.to_string(),
        first,
        end,
        start_ms: words[first].start,
        end_ms: words[end - 1].end,
        text: join(words, first, end),
        reason,
    }
}

/// Accept / reject verdicts by item id. Items without a verdict follow their
/// tier: `Auto` accepted, `Suggest` rejected.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Review {
    verdicts: BTreeMap<String, bool>,
}

impl Review {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn accept(&mut self, id: &str) -> &mut Self {
        self.verdicts.insert(id.to_string(), true);
        self
    }

    pub fn reject(&mut self, id: &str) -> &mut Self {
        self.verdicts.insert(id.to_string(), false);
        self
    }

    pub fn accept_all(plan: &EditorialPlan) -> Self {
        Self {
            verdicts: plan.items.iter().map(|i| (i.id.clone(), true)).collect(),
        }
    }

    pub fn reject_all(plan: &EditorialPlan) -> Self {
        Self {
            verdicts: plan.items.iter().map(|i| (i.id.clone(), false)).collect(),
        }
    }

    /// Accepts or rejects every item of one category.
    pub fn set_category(
        &mut self,
        plan: &EditorialPlan,
        category: Category,
        accept: bool,
    ) -> &mut Self {
        for i in plan.items.iter().filter(|i| i.category() == category) {
            self.verdicts.insert(i.id.clone(), accept);
        }
        self
    }

    /// Keeps take `index` of `group_id` and removes its siblings (overrides
    /// the verdicts of the group's retake items; everything else is untouched).
    pub fn pick_take(
        &mut self,
        plan: &EditorialPlan,
        group_id: &str,
        index: usize,
    ) -> Result<&mut Self, EditorialError> {
        let group = plan
            .take_groups
            .iter()
            .find(|g| g.id == group_id)
            .filter(|g| index < g.takes.len())
            .ok_or_else(|| EditorialError::UnknownItem(format!("{group_id}#{index}")))?;
        for t in &group.takes {
            if !t.item_id.is_empty() {
                self.verdicts.insert(t.item_id.clone(), t.index != index);
            }
        }
        Ok(self)
    }

    pub fn is_accepted(&self, item: &EditorialItem) -> bool {
        self.verdicts
            .get(&item.id)
            .copied()
            .unwrap_or_else(|| item.default_accepted())
    }

    fn validate(&self, plan: &EditorialPlan) -> Result<(), EditorialError> {
        match self
            .verdicts
            .keys()
            .find(|id| !plan.items.iter().any(|i| &i.id == *id))
        {
            Some(id) => Err(EditorialError::UnknownItem(id.clone())),
            None => Ok(()),
        }
    }
}

/// What one item did in a resolved edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemOutcome {
    pub item_id: String,
    pub accepted: bool,
    /// Source milliseconds of this item's extent that the edit removes.
    pub removed_ms: i64,
    /// Indices into `ActionGroup::actions` of the clip actions at the cut(s)
    /// this item produced (empty when rejected or nothing was removed).
    pub action_indices: Vec<usize>,
}

/// An editorial plan resolved under a review.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedEdit {
    pub cut_plan: CutPlan,
    /// The whole edit as one ordered, undoable action group.
    pub group: ActionGroup,
    pub time_map: TimeMap,
    pub outcomes: Vec<ItemOutcome>,
}

impl ResolvedEdit {
    /// Applies the group to `store`; all-or-nothing.
    pub fn apply(&self, store: &mut TimelineStore) -> Result<AppliedGroup, EditorialError> {
        Ok(apply_group(store, &self.group)?)
    }

    /// Undoes an applied group; it must still be the latest edit.
    pub fn undo(store: &mut TimelineStore, applied: &AppliedGroup) -> Result<(), EditorialError> {
        Ok(undo_group(store, applied)?)
    }
}

impl EditorialPlan {
    pub fn item(&self, id: &str) -> Option<&EditorialItem> {
        self.items.iter().find(|i| i.id == id)
    }

    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).expect("plan serializes")
    }

    /// Resolves the plan under `review` into a cut plan and one action group.
    pub fn resolve(
        &self,
        transcript: &Transcript,
        envelope: Option<&[f32]>,
        review: &Review,
        opts: &RealiseOptions,
    ) -> Result<ResolvedEdit, EditorialError> {
        if transcript.source_blake3 != self.source_blake3 {
            return Err(EditorialError::SourceMismatch {
                expected: self.source_blake3.clone(),
                found: transcript.source_blake3.clone(),
            });
        }
        review.validate(self)?;
        let accepted: Vec<bool> = self.items.iter().map(|i| review.is_accepted(i)).collect();
        let mut cfg = self.config.cuts.clone();
        cfg.remove_fillers = false;
        cfg.retake.enabled = false;
        let mut word_items: Vec<usize> = Vec::new();
        for (n, it) in self.items.iter().enumerate() {
            if let (true, ItemAction::RemoveWords { first, end }) = (accepted[n], &it.action) {
                cfg.manual_removals.push(ManualRemoval {
                    start_ms: transcript.words[*first].start_ms,
                    end_ms: transcript.words[end - 1].end_ms,
                });
                word_items.push(n);
            }
        }
        let silence: Vec<usize> = (0..self.items.len())
            .filter(|&n| matches!(self.items[n].action, ItemAction::ShortenPause { .. }))
            .collect();
        let silence_accepted = silence.iter().filter(|&&n| accepted[n]).count();
        if !silence.is_empty() && silence_accepted == 0 {
            cfg.remove_silence = false;
        }
        let mut plan = match plan_cuts(transcript, envelope, &cfg, self.fps) {
            Ok(plan) => plan,
            // A multi-file edit can replace all speech from a losing source.
            // Keep the single-source cut engine's NothingKept guard; accept an
            // empty contribution only with explicit accepted cross-file evidence
            // & word coverage from accepted editorial removals.
            Err(crate::cuts::CutError::NothingKept)
                if word_items.iter().any(|&n| {
                    self.items[n].kind == ItemKind::CrossFileRetake
                        && self.items[n]
                            .evidence
                            .kept_source
                            .as_deref()
                            .is_some_and(|s| s != self.source_blake3)
                }) && (0..transcript.words.len()).all(|i| {
                    word_items.iter().any(|&n| {
                        matches!(self.items[n].action, ItemAction::RemoveWords { first, end }
                            if i >= first && i < end)
                    })
                }) =>
            {
                CutPlan {
                    preset: cfg.preset,
                    source: rightkit_timeline::SourceId::new(self.source_blake3.clone())
                        .map_err(|_| crate::cuts::CutError::SourceId(self.source_blake3.clone()))?,
                    fps: self.fps,
                    source_duration_ms: transcript.duration_ms,
                    kept: Vec::new(),
                    kept_words: Vec::new(),
                    removed: Vec::new(),
                    decisions: word_items
                        .iter()
                        .map(|&n| {
                            let item = &self.items[n];
                            Decision {
                                kind: RemovalReason::Manual,
                                start_ms: item.start_ms,
                                end_ms: item.end_ms,
                                text: item.evidence.text.clone(),
                                kept_text: item.evidence.kept_text.clone(),
                                detail: item.reason.clone(),
                            }
                        })
                        .collect(),
                }
            }
            Err(error) => return Err(error.into()),
        };

        // Name the manual removals after what the items are.
        let mut used = vec![false; plan.decisions.len()];
        for &n in &word_items {
            let it = &self.items[n];
            let ItemAction::RemoveWords { first, end } = it.action else {
                continue;
            };
            let (s, e) = (
                transcript.words[first].start_ms as i64,
                transcript.words[end - 1].end_ms as i64,
            );
            if let Some(d) = (0..plan.decisions.len()).find(|&d| {
                !used[d]
                    && plan.decisions[d].kind == RemovalReason::Manual
                    && plan.decisions[d].start_ms == s
                    && plan.decisions[d].end_ms <= e
            }) {
                used[d] = true;
                plan.decisions[d].kind = match it.kind {
                    ItemKind::Filler => RemovalReason::Filler,
                    ItemKind::Retake | ItemKind::MarkedRetake | ItemKind::CrossFileRetake => {
                        RemovalReason::Retake
                    }
                    _ => RemovalReason::Manual,
                };
                plan.decisions[d].detail = format!("{}: {}", kind_name(it.kind), it.reason);
                if let Some(k) = &it.evidence.kept_text {
                    plan.decisions[d].kept_text = Some(k.clone());
                }
            }
        }

        let fps = self.fps;
        let duration = Ticks(transcript.duration_ms as i64 * TICKS_PER_MS);
        let mut kept: Vec<(Ticks, Ticks)> = plan
            .kept
            .iter()
            .map(|k| (k.source_in, k.source_out))
            .collect();
        if silence_accepted > 0 {
            for &n in &silence {
                if let (false, ItemAction::ShortenPause { start_ms, end_ms }) =
                    (accepted[n], &self.items[n].action)
                {
                    let (s, e) = inward(fps, *start_ms, *end_ms, duration);
                    if e > s {
                        kept.push((s, e));
                    }
                }
            }
            kept.sort();
            kept = merge(kept);
        }
        // Frame alignment may extend a kept neighbour into an accepted losing
        // take when no frame boundary fits between the words. Clamp cross-file
        // removals to their exact transcript extent after pause restoration:
        // both video & linked audio trims must exclude every losing word.
        for &n in &word_items {
            if self.items[n].kind == ItemKind::CrossFileRetake {
                let ItemAction::RemoveWords { first, end } = self.items[n].action else {
                    continue;
                };
                let s = Ticks::from_millis(transcript.words[first].start_ms as i64);
                let e = Ticks::from_millis(transcript.words[end - 1].end_ms as i64);
                kept = subtract(kept, s, e);
            }
        }
        for (n, it) in self.items.iter().enumerate() {
            if let (true, ItemAction::RemoveRange { start_ms, end_ms }) = (accepted[n], &it.action)
            {
                let (s, e) = inward(fps, *start_ms, *end_ms, duration);
                if e > s {
                    kept = subtract(kept, s, e);
                    plan.decisions.push(Decision {
                        kind: RemovalReason::Silence,
                        start_ms: *start_ms,
                        end_ms: *end_ms,
                        text: String::new(),
                        kept_text: None,
                        detail: format!("{}: {}", kind_name(it.kind), it.reason),
                    });
                }
            }
        }
        plan.kept = kept
            .into_iter()
            .filter(|(a, b)| b > a)
            .map(|(a, b)| KeptRange {
                source_in: a,
                source_out: b,
            })
            .collect();
        plan.decisions.sort_by(|a, b| {
            (a.start_ms, a.end_ms, a.kind, &a.text).cmp(&(b.start_ms, b.end_ms, b.kind, &b.text))
        });
        plan.removed = removals(&plan.kept, duration, &plan.decisions);

        let group = plan.group(opts);
        let time_map = plan.time_map();
        let stride = 1 + usize::from(opts.with_audio);
        let gaps = gaps_ms(&plan.kept, duration);
        let outcomes = self
            .items
            .iter()
            .enumerate()
            .map(|(n, it)| {
                let mut removed_ms = 0;
                let mut idx: Vec<usize> = Vec::new();
                for (g, &(a, b)) in gaps.iter().enumerate() {
                    let ov = (it.end_ms.min(b) - it.start_ms.max(a)).max(0);
                    if ov > 0 {
                        removed_ms += ov;
                        if accepted[n] {
                            // gap g sits between kept[g-1] and kept[g]
                            for k in [g.wrapping_sub(1), g] {
                                if k < plan.kept.len() {
                                    idx.extend((0..stride).map(|s| k * stride + s));
                                }
                            }
                        }
                    }
                }
                idx.sort();
                idx.dedup();
                ItemOutcome {
                    item_id: it.id.clone(),
                    accepted: accepted[n],
                    removed_ms,
                    action_indices: idx,
                }
            })
            .collect();
        Ok(ResolvedEdit {
            cut_plan: plan,
            group,
            time_map,
            outcomes,
        })
    }
}

impl EditorialPlan {
    /// Appends extra items (e.g. cross-file retakes discovered after the
    /// per-source pass), re-derives item ids and rehashes the plan id.
    pub fn extend(&mut self, mut extra: Vec<EditorialItem>) {
        self.items.append(&mut extra);
        self.items.sort_by(|a, b| {
            (a.start_ms, a.end_ms, a.kind, &a.reason)
                .cmp(&(b.start_ms, b.end_ms, b.kind, &b.reason))
        });
        assign_ids(&mut self.items);
        self.id = String::new();
        let hash = blake3::hash(&self.to_json());
        self.id = format!("ed-{}", &hash.to_hex()[..8]);
    }

    /// Resolves a transcript-free plan (visual/silent footage): the timeline
    /// starts as the whole source and accepted [`ItemAction::RemoveRange`]
    /// items subtract from it, frame-aligned inward. Word-level actions are
    /// meaningless here and are ignored.
    pub fn resolve_visual(
        &self,
        review: &Review,
        opts: &RealiseOptions,
    ) -> Result<ResolvedEdit, EditorialError> {
        review.validate(self)?;
        let fps = self.fps;
        let duration = Ticks(self.source_duration_ms as i64 * TICKS_PER_MS);
        if duration.0 <= 0 {
            return Err(crate::cuts::CutError::NothingKept.into());
        }
        let accepted: Vec<bool> = self.items.iter().map(|i| review.is_accepted(i)).collect();
        let mut kept: Vec<(Ticks, Ticks)> = vec![(Ticks::ZERO, duration)];
        let mut decisions: Vec<Decision> = Vec::new();
        for (n, it) in self.items.iter().enumerate() {
            let ItemAction::RemoveRange { start_ms, end_ms } = it.action else {
                continue;
            };
            if !accepted[n] {
                continue;
            }
            let (s, e) = inward(fps, start_ms, end_ms, duration);
            if e <= s {
                continue;
            }
            kept = subtract(kept, s, e);
            decisions.push(Decision {
                kind: match it.kind {
                    ItemKind::Retake | ItemKind::MarkedRetake | ItemKind::CrossFileRetake => {
                        RemovalReason::Retake
                    }
                    ItemKind::DeadSpace | ItemKind::WeakSegment => RemovalReason::Visual,
                    _ => RemovalReason::Manual,
                },
                start_ms,
                end_ms,
                text: String::new(),
                kept_text: it.evidence.kept_text.clone(),
                detail: format!("{}: {}", kind_name(it.kind), it.reason),
            });
        }
        let source = rightkit_timeline::SourceId::new(self.source_blake3.clone())
            .map_err(|_| crate::cuts::CutError::SourceId(self.source_blake3.clone()))?;
        let mut plan = CutPlan {
            preset: self.config.cuts.preset,
            source,
            fps,
            source_duration_ms: self.source_duration_ms,
            kept: kept
                .into_iter()
                .filter(|(a, b)| b > a)
                .map(|(a, b)| KeptRange {
                    source_in: a,
                    source_out: b,
                })
                .collect(),
            kept_words: Vec::new(),
            removed: Vec::new(),
            decisions,
        };
        if plan.kept.is_empty() {
            return Err(crate::cuts::CutError::NothingKept.into());
        }
        plan.decisions.sort_by(|a, b| {
            (a.start_ms, a.end_ms, a.kind, &a.text).cmp(&(b.start_ms, b.end_ms, b.kind, &b.text))
        });
        plan.removed = removals(&plan.kept, duration, &plan.decisions);
        let group = plan.group(opts);
        let time_map = plan.time_map();
        let stride = 1 + usize::from(opts.with_audio);
        let gaps = gaps_ms(&plan.kept, duration);
        let outcomes = self
            .items
            .iter()
            .enumerate()
            .map(|(n, it)| {
                let mut removed_ms = 0;
                let mut idx: Vec<usize> = Vec::new();
                for (g, &(a, b)) in gaps.iter().enumerate() {
                    let ov = (it.end_ms.min(b) - it.start_ms.max(a)).max(0);
                    if ov > 0 {
                        removed_ms += ov;
                        if accepted[n] {
                            for k in [g.wrapping_sub(1), g] {
                                if k < plan.kept.len() {
                                    idx.extend((0..stride).map(|s| k * stride + s));
                                }
                            }
                        }
                    }
                }
                idx.sort();
                idx.dedup();
                ItemOutcome {
                    item_id: it.id.clone(),
                    accepted: accepted[n],
                    removed_ms,
                    action_indices: idx,
                }
            })
            .collect();
        Ok(ResolvedEdit {
            cut_plan: plan,
            group,
            time_map,
            outcomes,
        })
    }
}

/// Analyses `transcript` (and the optional per-10 ms RMS `envelope`) into a
/// reviewable plan. Pure and deterministic.
pub fn analyze(
    transcript: &Transcript,
    envelope: Option<&[f32]>,
    cfg: &EditorialConfig,
    fps: Fps,
) -> Result<EditorialPlan, EditorialError> {
    transcript.validate().map_err(crate::cuts::CutError::from)?;
    let words = words_of(transcript);
    // The cut engine's own decisions are the baseline the editorial items build on.
    let baseline = plan_cuts(transcript, envelope, &cfg.cuts, fps)?;
    let punctuated = retake::is_punctuated(&words);
    let mut drafts: Vec<Draft> = Vec::new();

    // Soft-filler gate and self-talk: escalated cases are listed, never cut. A Keep or
    // Escalate gate verdict vetoes the baseline filler it covers.
    let rule_cfg = RuleConfig::default();
    let gate = soft_filler_gate(&transcript.words, &rule_cfg);
    let mut retake_escalations: Vec<RetakeEscalation> = Vec::new();
    for h in self_talk(&transcript.words, &rule_cfg) {
        if h.verdict == Verdict::Escalate {
            retake_escalations.push(escalation(&words, "self_talk", h.first, h.end, h.reason));
        }
    }
    for h in gate.iter().filter(|h| h.verdict == Verdict::Escalate) {
        retake_escalations.push(escalation(
            &words,
            "soft_filler",
            h.first,
            h.end,
            h.reason.clone(),
        ));
    }

    for d in &baseline.decisions {
        match d.kind {
            RemovalReason::Silence => {
                let position = if d.detail.starts_with("leading") {
                    "before the first word"
                } else if d.detail.starts_with("trailing") {
                    "after the last word"
                } else if d.detail.starts_with("pause ") {
                    "between words"
                } else {
                    continue; // inherent to a word-removal cut, not a separate edit
                };
                drafts.push(Draft {
                    kind: ItemKind::Silence,
                    tier: Tier::Auto,
                    confidence: 0.95,
                    start_ms: d.start_ms,
                    end_ms: d.end_ms,
                    reason: d.detail.clone(),
                    evidence: Evidence {
                        notes: vec![position.to_string()],
                        signals: {
                            let mut sig = std::collections::BTreeMap::new();
                            sig.insert("removed_ms".to_string(), (d.end_ms - d.start_ms) as f64);
                            // Full pause length (kept + removed), parsed from "pause N ms ...".
                            if let Some(n) = d
                                .detail
                                .strip_prefix("pause ")
                                .and_then(|r| r.split_whitespace().next())
                                .and_then(|n| n.parse::<i64>().ok())
                            {
                                sig.insert("pause_ms".to_string(), n as f64);
                            }
                            sig
                        },
                        ..Evidence::default()
                    },
                    action: ItemAction::ShortenPause {
                        start_ms: d.start_ms,
                        end_ms: d.end_ms,
                    },
                    take: None,
                });
            }
            RemovalReason::Filler => {
                let first = first_word_at(&words, d.start_ms);
                let end = first_word_at(&words, d.end_ms);
                if first < end {
                    if gate
                        .iter()
                        .any(|h| h.verdict != Verdict::Cut && h.first < end && first < h.end)
                    {
                        continue; // the soft-filler gate keeps or escalates this filler
                    }
                    let context = d.detail.starts_with("isolated");
                    drafts.push(word_draft(
                        &words,
                        ItemKind::Filler,
                        first,
                        end,
                        Tier::Auto,
                        if context { 0.7 } else { 0.9 },
                        d.detail.clone(),
                    ));
                }
            }
            RemovalReason::Retake | RemovalReason::Manual | RemovalReason::Visual => {}
        }
    }

    let mut rt = retake::detect(&words, &baseline.decisions, envelope, cfg, punctuated);
    let mut take_groups = std::mem::take(&mut rt.groups);
    drafts.extend(rt.drafts);
    drafts.extend(retake::marked(&words, &cfg.retake, &rt.protected));
    drafts.extend(disfluency::detect(&words, &cfg.disfluency));
    drafts.extend(deadair::detect(transcript, &words, envelope, cfg));

    let mut items: Vec<EditorialItem> = drafts.into_iter().map(Draft::into_item).collect();
    items.sort_by(|a, b| {
        (a.start_ms, a.end_ms, a.kind, &a.reason).cmp(&(b.start_ms, b.end_ms, b.kind, &b.reason))
    });
    // Word-removal items must not be empty or out of range.
    items.retain(|i| match i.action {
        ItemAction::RemoveWords { first, end } => first < end && end <= words.len(),
        _ => i.end_ms > i.start_ms,
    });
    assign_ids(&mut items);
    cover(&mut items);
    let beats = beats::segment(&words, transcript.duration_ms as i64, &cfg.beats);
    for it in &mut items {
        it.beat_id = beats
            .iter()
            .rev()
            .find(|b| b.start_ms <= it.start_ms)
            .or(beats.first())
            .map(|b| b.id.clone());
    }
    retake_escalations.sort_by_key(|e| (e.first, e.end));
    let mut plan = EditorialPlan {
        schema: SCHEMA.to_string(),
        id: String::new(),
        source_blake3: transcript.source_blake3.clone(),
        fps,
        source_duration_ms: transcript.duration_ms,
        config: cfg.clone(),
        items,
        beats,
        take_groups: Vec::new(),
        retake_escalations,
    };
    for g in &mut take_groups {
        for t in &mut g.takes {
            if let Some(it) = plan.items.iter().find(|i| {
                i.take
                    .as_ref()
                    .is_some_and(|r| r.group_id == g.id && r.index == t.index)
            }) {
                t.item_id = it.id.clone();
            }
        }
    }
    plan.take_groups = take_groups;
    let hash = blake3::hash(&plan.to_json());
    plan.id = format!("ed-{}", &hash.to_hex()[..8]);
    Ok(plan)
}

pub(crate) fn kind_name(k: ItemKind) -> String {
    serde_json::to_value(k)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

pub(crate) fn assign_ids(items: &mut [EditorialItem]) {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for it in items.iter_mut() {
        let key = format!(
            "{}:{}:{}:{:?}",
            kind_name(it.kind),
            it.start_ms,
            it.end_ms,
            it.action
        );
        let hash = blake3::hash(key.as_bytes());
        let base = format!("ed-{}-{}", kind_name(it.kind), &hash.to_hex()[..8]);
        let n = seen.entry(base.clone()).or_insert(0);
        *n += 1;
        it.id = if *n == 1 { base } else { format!("{base}-{n}") };
    }
}

/// Marks items whose words another item already removes.
fn cover(items: &mut [EditorialItem]) {
    let spans: Vec<Option<(usize, usize)>> = items.iter().map(EditorialItem::word_span).collect();
    for a in 0..items.len() {
        let Some((af, ae)) = spans[a] else { continue };
        let mut best: Option<usize> = None;
        for b in 0..items.len() {
            let Some((bf, be)) = spans[b] else { continue };
            if a == b || !(bf <= af && ae <= be) {
                continue;
            }
            let bigger = (be - bf) > (ae - af);
            if !bigger && items[b].kind.priority() >= items[a].kind.priority() {
                continue;
            }
            let better = best.is_none_or(|c| {
                (items[b].kind.priority(), be - bf)
                    < (items[c].kind.priority(), spans[c].map_or(0, |s| s.1 - s.0))
            });
            if better {
                best = Some(b);
            }
        }
        if let Some(b) = best {
            items[a].covered_by = Some(items[b].id.clone());
        }
    }
}

fn ceil_frame(fps: Fps, t: Ticks) -> Ticks {
    let mut f = fps.ticks_to_frame(t).max(0);
    while fps.frame_to_ticks(f) < t {
        f += 1;
    }
    while f > 0 && fps.frame_to_ticks(f - 1) >= t {
        f -= 1;
    }
    fps.frame_to_ticks(f)
}

fn floor_frame(fps: Fps, t: Ticks) -> Ticks {
    let mut f = fps.ticks_to_frame(t).max(0);
    while f > 0 && fps.frame_to_ticks(f) > t {
        f -= 1;
    }
    while fps.frame_to_ticks(f + 1) <= t {
        f += 1;
    }
    fps.frame_to_ticks(f)
}

/// The largest frame-aligned range inside `[start_ms, end_ms]`.
fn inward(fps: Fps, start_ms: i64, end_ms: i64, duration: Ticks) -> (Ticks, Ticks) {
    let s = ceil_frame(fps, Ticks(start_ms.max(0) * TICKS_PER_MS));
    let e_raw = Ticks((end_ms * TICKS_PER_MS).min(duration.0));
    let e = if e_raw >= duration {
        e_raw
    } else {
        floor_frame(fps, e_raw)
    };
    (s, e)
}

fn merge(sorted: Vec<(Ticks, Ticks)>) -> Vec<(Ticks, Ticks)> {
    let mut out: Vec<(Ticks, Ticks)> = Vec::new();
    for (a, b) in sorted {
        match out.last_mut() {
            Some(l) if a <= l.1 => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

fn subtract(kept: Vec<(Ticks, Ticks)>, s: Ticks, e: Ticks) -> Vec<(Ticks, Ticks)> {
    let mut out = Vec::new();
    for (a, b) in kept {
        if b <= s || a >= e {
            out.push((a, b));
            continue;
        }
        if a < s {
            out.push((a, s));
        }
        if b > e {
            out.push((e, b));
        }
    }
    out
}

fn gaps_ms(kept: &[KeptRange], duration: Ticks) -> Vec<(i64, i64)> {
    let ms = |t: Ticks| t.to_units(1000);
    let mut gaps = Vec::new();
    let mut cursor = Ticks::ZERO;
    for k in kept {
        gaps.push((ms(cursor), ms(k.source_in)));
        cursor = k.source_out;
    }
    gaps.push((ms(cursor), ms(duration)));
    gaps
}

fn removals(kept: &[KeptRange], duration: Ticks, decisions: &[Decision]) -> Vec<Removal> {
    let mut gaps: Vec<(Ticks, Ticks)> = Vec::new();
    let mut cursor = Ticks::ZERO;
    for k in kept {
        if k.source_in > cursor {
            gaps.push((cursor, k.source_in));
        }
        cursor = k.source_out;
    }
    if duration > cursor {
        gaps.push((cursor, duration));
    }
    gaps.into_iter()
        .map(|(s, e)| {
            let ids: Vec<usize> = decisions
                .iter()
                .enumerate()
                .filter(|(_, d)| d.start_ms * TICKS_PER_MS < e.0 && d.end_ms * TICKS_PER_MS > s.0)
                .map(|(i, _)| i)
                .collect();
            let reason = ids
                .iter()
                .map(|&i| decisions[i].kind)
                .max()
                .unwrap_or(RemovalReason::Silence);
            Removal {
                source_start: s,
                source_end: e,
                reason,
                decisions: ids,
            }
        })
        .collect()
}
