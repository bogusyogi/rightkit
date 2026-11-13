// Display copy for enums. Every state has a text label; tone is a secondary cue only.
import type {
  AnalyticsSource,
  Approval,
  BriefState,
  BriefType,
  DispatchOutcome,
  GateDisposition,
  GateVerdict,
  ContentMediaKind,
  ContentState,
  ContentVariantKind,
  ContentVerdict,
  Entitlement,
  HeartbeatState,
  InboxKind,
  InboxState,
  PacingReason,
  HookTestRole,
  HookTestState,
  JobFormat,
  JobState,
  LinkPlacement,
  OptimiseState,
  ProofStatus,
  PublishProgressState,
  PublishResolutionDecision,
  RejectReason,
  SocialPlatform,
  TokenHealth,
} from "../data/index.js";
import { daysUntil, formatCount, isSocialPlatform, remaining } from "../data/index.js";
import { hostAppName, hostProducerApps } from "../appName.js";

/** The ordinary name of each account platform. Exhaustive over SocialPlatform: a new id fails to compile until it is named. */
export const PLATFORM_LABEL: Record<SocialPlatform, string> = {
  instagram: "Instagram",
  facebook: "Facebook",
  threads: "Threads",
  youtube: "YouTube",
  tiktok: "TikTok",
  x: "X",
  linkedin: "LinkedIn",
  pinterest: "Pinterest",
  bluesky: "Bluesky",
  mastodon: "Mastodon",
};

/** Draft-only platform the engine accepts for manual drafts (DRAFT_ONLY_PLATFORMS, crates/protocol/src/drafts.rs:23). */
const DRAFT_ONLY_LABEL = new Map<string, string>([["reddit", "Reddit"]]);

/**
 * The display name for any platform id a stored row carries. A known id gets its name. An id the desktop does
 * not know (a newer engine) is shown with its raw id and the words "unknown platform", never dropped.
 */
export function platformName(platform: string): string {
  if (isSocialPlatform(platform)) return PLATFORM_LABEL[platform];
  return DRAFT_ONLY_LABEL.get(platform) ?? `${platform} (unknown platform)`;
}

export const FORMAT_LABEL: Record<JobFormat, string> = {
  reel: "Reel",
  carousel: "Carousel",
  photo: "Photo",
  short: "Short",
  video: "Video",
  text_post: "Text post",
  remake: "Remake (draft)",
};

/** Suggestion states. Text carries the meaning; the tone is a secondary cue. */
export const OPTIMISE_STATE_LABEL: Record<OptimiseState, { text: string; tone: Tone }> = {
  proposed: { text: "Waiting for your decision", tone: "warn" },
  accepted: { text: "Accepted: draft calendar entry created", tone: "ok" },
  dismissed: { text: "Dismissed", tone: "neutral" },
};

export type Tone = "neutral" | "accent" | "ok" | "warn" | "bad";

/** Hook test states. An open set is read on the daemon host, so the label says so. */
export const HOOK_STATE_LABEL: Record<HookTestState, { text: string; tone: Tone }> = {
  open: { text: "Open: not decided yet", tone: "warn" },
  decided: { text: "Decided", tone: "ok" },
};

/** Opening roles in a decided hook test. Null (not decided) is written by the screen. */
export const HOOK_ROLE_LABEL: Record<HookTestRole, string> = {
  promoted: "Promoted: the winner",
  archived: "Archived",
};

export const STATE_LABEL: Record<JobState, { text: string; tone: Tone }> = {
  draft: { text: "Draft", tone: "neutral" },
  media_pending: { text: "Media pending", tone: "warn" },
  ready: { text: "Media ready", tone: "accent" },
  scheduled: { text: "Scheduled", tone: "ok" },
  publishing: { text: "Publishing", tone: "accent" },
  published: { text: "Published", tone: "ok" },
  outcome_unknown: { text: "Outcome unknown", tone: "bad" },
  failed: { text: "Failed", tone: "bad" },
  late: { text: "Late", tone: "warn" },
  paused: { text: "Paused", tone: "warn" },
};

/** The daemon's publish job state, shown beside the calendar state. Text carries the meaning. */
export const PUBLISH_STATE_LABEL: Record<PublishProgressState, { text: string; tone: Tone }> = {
  queued: { text: "Queued on the daemon", tone: "neutral" },
  running: { text: "Publishing now", tone: "accent" },
  waiting: { text: "Waiting to retry", tone: "warn" },
  published: { text: "Published by the daemon", tone: "ok" },
  failed: { text: "Publish failed", tone: "bad" },
  outcome_unknown: { text: "Outcome unknown", tone: "bad" },
  cancelled: { text: "Publish cancelled", tone: "neutral" },
};

/** A person's recorded decision on an outcome_unknown job, in words. */
export const PUBLISH_RESOLUTION_LABEL: Record<PublishResolutionDecision, string> = {
  posted: "recorded as posted by hand",
  not_posted: "recorded as not posted",
};

/** The plain explanation shown on every outcome_unknown failure. The wording is fixed. */
export function outcomeUnknownExplanation(): string {
  return `The platform may have accepted this post. ${hostAppName()} will not retry until the daemon has checked.`;
}

export const REJECT_LABEL: Record<RejectReason, string> = {
  wrong_content: "Wrong content",
  wrong_account: "Wrong account",
  timing: "Timing",
  media_issue: "Media issue",
  other: "Other (note required)",
};

export const MEDIA_STATUS_LABEL = {
  ready: "Ready",
  uploading: "Uploading",
  missing: "Missing",
} as const;

export function approvalText(approval: Approval): { text: string; tone: Tone } {
  switch (approval.status) {
    case "none":
      return { text: "No approval requested", tone: "neutral" };
    case "pending":
      return { text: "Awaiting approval", tone: "warn" };
    case "approved":
      return { text: `Approved at revision ${approval.approvedRevision}`, tone: "ok" };
    case "rejected":
      return { text: `Rejected: ${REJECT_LABEL[approval.reason].replace(" (note required)", "")}`, tone: "bad" };
    case "invalidated":
      return { text: "Approval invalidated: re-approval needed", tone: "bad" };
  }
}

export function tokenText(token: TokenHealth, nowUtc: string): { text: string; tone: Tone; mark: string } {
  switch (token.status) {
    case "valid":
      return { text: `Valid, expires in ${daysUntil(token.expiresOn, nowUtc)} days`, tone: "ok", mark: "OK" };
    case "expiring": {
      const days = daysUntil(token.expiresOn, nowUtc);
      return { text: `Expires in ${days} day${days === 1 ? "" : "s"}. Reconnect before then.`, tone: "warn", mark: "Expiring" };
    }
    case "refresh_failed":
      return { text: "Refresh failed. Publishing is blocked until reconnect.", tone: "bad", mark: "Failed" };
    case "revoked":
      return { text: "Revoked at the provider. Reconnect required.", tone: "bad", mark: "Revoked" };
  }
}

export const ENTITLEMENT_LABEL: Record<Entitlement, { text: string; hint: string }> = {
  sandbox_mechanics: { text: "Sandbox mechanics", hint: "Tested with test users only. Not visible to the audience." },
  review_submitted: { text: "Review submitted", hint: "Submitted to the platform. Not yet audience-visible." },
  audience_visible: { text: "Audience-visible", hint: "Posts reach the public audience." },
};

export function cooldownText(until: string | null, nowUtc: string): string {
  if (!until) return "None";
  const left = remaining(until, nowUtc);
  return left === "now" ? "Cooldown has ended" : `Waiting, ${left} left`;
}

export function countText(n: number): string {
  return formatCount(n);
}

/** Content record states. Withdrawn is final; expired is derived from the clock and never stored. */
export const CONTENT_STATE_LABEL: Record<ContentState, { text: string; tone: Tone }> = {
  active: { text: "Active", tone: "ok" },
  withdrawn: { text: "Withdrawn: final", tone: "bad" },
  expired: { text: "Expired: cannot be planned", tone: "warn" },
};

export const CONTENT_MEDIA_KIND_LABEL: Record<ContentMediaKind, string> = {
  image: "Image",
  video: "Video",
};

export const CONTENT_VARIANT_KIND_LABEL: Record<ContentVariantKind, string> = {
  caption: "Caption",
  cta: "Call to action",
};

/** The engine's experiment verdict in words. Neither verdict names a winner. */
export const CONTENT_VERDICT_LABEL: Record<ContentVerdict, { text: string; tone: Tone }> = {
  insufficient_evidence: { text: "Insufficient evidence: no winner is shown", tone: "warn" },
  evidence_sufficient: { text: "Evidence sufficient: the engine names no winner", tone: "ok" },
};

/** The text gate's overall result. The words say what happens next; the tone is a secondary cue only. */
export const GATE_DISPOSITION_LABEL: Record<GateDisposition, { text: string; tone: Tone }> = {
  pass: { text: "Pass: no rule blocks this script", tone: "ok" },
  hold: { text: "Hold: a person must decide", tone: "warn" },
  block: { text: "Block: a rule stops this script", tone: "bad" },
};

/** One rule's verdict in words. */
export const GATE_VERDICT_LABEL: Record<GateVerdict, string> = {
  pass: "Pass",
  fail: "Fail",
  inconclusive: "Inconclusive",
  unverified: "Unverified",
  hold: "Hold",
  not_applicable: "Not applicable",
};

/** Outgoing brief states. An issued brief has no delivery yet; a partial one has only some of what was requested. */
export const BRIEF_STATE_LABEL: Record<BriefState, { text: string; tone: Tone }> = {
  issued: { text: "Issued: no delivery yet", tone: "warn" },
  partial: { text: "Partly delivered: some deliverables were skipped or refused", tone: "warn" },
  delivered: { text: "Delivered", tone: "ok" },
};

/** Dispatch attempt outcomes. Each names what happened to the run, in words. */
export const DISPATCH_OUTCOME_LABEL: Record<DispatchOutcome, { text: string; tone: Tone }> = {
  running: { text: "Running now", tone: "accent" },
  delivered: { text: "Delivered", tone: "ok" },
  partial: { text: "Partly delivered: the engine skipped or refused some deliverables", tone: "warn" },
  awaiting_approval: {
    // Read when shown, so the host's producer names (set at startup) are in it.
    get text() {
      const apps = hostProducerApps();
      return `Waiting for approval in the producer's app${apps ? ` (${apps})` : ""}: approve the assets there, then run the brief again`;
    },
    tone: "warn",
  },
  engine_failed: { text: "Engine failed", tone: "bad" },
  timed_out: { text: "Timed out: the run was killed", tone: "bad" },
  delivery_invalid: { text: "Finished, but no usable envelope", tone: "bad" },
  engine_missing: { text: "Engine not found: nothing ran", tone: "warn" },
  interrupted: { text: "Interrupted: the result is not known", tone: "bad" },
};

/** Heartbeat states in words. "Stale" and "unreadable" are never shown as alive. */
export const HEARTBEAT_STATE_LABEL: Record<HeartbeatState, { text: string; tone: Tone }> = {
  alive: { text: "Alive", tone: "ok" },
  stale: { text: "Stale: older than the threshold", tone: "bad" },
  missing: { text: "Missing: no heartbeat has been recorded", tone: "bad" },
  unreadable: { text: "Unreadable: the file could not be read", tone: "bad" },
};

/** Proof statuses in words. Only an approved proof can be cited by a script; the tone is a secondary cue only. */
export const PROOF_STATUS_LABEL: Record<ProofStatus, { text: string; tone: Tone }> = {
  collected: { text: "Collected: not approved, so no script can cite it", tone: "neutral" },
  approved: { text: "Approved: a script can cite it", tone: "ok" },
  withdrawn: { text: "Withdrawn: no script can cite it", tone: "bad" },
  superseded: { text: "Superseded: replaced by another proof", tone: "neutral" },
};

/** Typed brief types in words. The type is text; no colour is used. */
export const BRIEF_TYPE_LABEL: Record<BriefType, string> = {
  talking_head: "Talking head",
  faceless_cartoon_short: "Faceless cartoon short",
  explainer: "Explainer",
  carousel: "Carousel",
  ugc_character_review: "UGC character review",
  product_pitch: "Product pitch",
  product_demo: "Product demo",
};

/** Where a platform places a link, in words. "none" means the platform carries no link. */
export const LINK_PLACEMENT_LABEL: Record<LinkPlacement, string> = {
  caption: "In the caption",
  bio: "In the bio",
  first_comment: "In the first comment",
  none: "None: this platform carries no link",
};

/** A stored metric the platform does not provide. It is never shown as zero. */
export const ANALYTICS_UNAVAILABLE = "Not available from this platform";

/** Where a stored number came from, in words. An API row names its pull elsewhere; a manual row names its import. */
export function analyticsSourceText(source: AnalyticsSource, importId: string | null): string {
  if (source === "api") return "from the platform's API";
  return importId === null ? "from a file the owner exported" : `from a file the owner exported (import ${importId})`;
}

/** A post's age at an observation: the exact seconds, then the same span in days, hours and minutes. */
export function postAgeText(seconds: number): string {
  const days = Math.floor(seconds / 86_400);
  const hours = Math.floor((seconds % 86_400) / 3_600);
  const minutes = Math.floor((seconds % 3_600) / 60);
  return `${seconds} seconds (${span(days, "day")}, ${span(hours, "hour")}, ${span(minutes, "minute")})`;
}

/** Inbox event kinds in words. The kind is text; no colour is used. */
export const INBOX_KIND_LABEL: Record<InboxKind, string> = {
  comment: "Comment",
  message: "Message",
  mention: "Mention",
  other: "Other",
};

/** Inbox event states in words. The state is set by a person on the server; this screen only shows it. */
export const INBOX_STATE_LABEL: Record<InboxState, { text: string; tone: Tone }> = {
  open: { text: "Open: not handled yet", tone: "warn" },
  handled: { text: "Handled", tone: "ok" },
  ignored: { text: "Ignored", tone: "neutral" },
};

/** Why a publish job may not run now, in words. Every reason the daemon can give has a line here. */
export const PACING_REASON_LABEL: Record<PacingReason, string> = {
  not_due: "Not due yet",
  outcome_unknown_on_account:
    "Held: another job on this account has an unknown outcome, and a person must resolve it",
  account_not_ready: "Account not ready to publish",
  account_paused: "Account paused by a person",
  rate_limited_until: "The platform asked us to wait",
  daily_cap: "Daily cap reached for this account",
  account_gap: "Too soon after the last post on this account",
};

function isInboxKind(kind: string): kind is InboxKind {
  return Object.prototype.hasOwnProperty.call(INBOX_KIND_LABEL, kind);
}

function isInboxState(state: string): state is InboxState {
  return Object.prototype.hasOwnProperty.call(INBOX_STATE_LABEL, state);
}

/** An inbox kind as stored. A kind the desktop does not show is named as such, never dropped. */
export function inboxKindText(kind: string): string {
  return isInboxKind(kind) ? INBOX_KIND_LABEL[kind] : `Kind not shown by the desktop: ${kind}`;
}

/** An inbox state as stored. A state the desktop does not show is named as such. */
export function inboxStateText(state: string): { text: string; tone: Tone } {
  return isInboxState(state) ? INBOX_STATE_LABEL[state] : { text: `State not shown by the desktop: ${state}`, tone: "bad" };
}

function span(count: number, word: string): string {
  return `${count} ${word}${count === 1 ? "" : "s"}`;
}
