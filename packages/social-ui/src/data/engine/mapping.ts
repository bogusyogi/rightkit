// Pure mapping from engine payloads to the operator types. Every field the engine does not hold is
// set here to the value named in docs/design/p9-desktop-screens-v1.md ("Engine gaps"). Nothing in
// this file reads a clock, the network or the shell.
import type {
  Account,
  Approval,
  DraftKind,
  DraftState,
  Entitlement,
  FailureItem,
  JobFormat,
  ManualDraft,
  JobState,
  Lateness,
  MediaItem,
  MediaKind,
  PauseRecord,
  PublishJob,
  PublishProgress,
  PublishProgressState,
  PublishJobRecord,
  PublishJobResolution,
  PublishResolutionDecision,
  RejectReason,
  SocialPlatform,
  TokenHealth,
  HeartbeatState,
  HeartbeatStatus,
  RestoreReceipt,
  RestoreReceiptList,
  InboxEvent,
  InboxKind,
  InboxRaw,
  InboxState,
  InboxStats,
  InboxStatsGroup,
  PacingBlockedJob,
  PacingJob,
  PacingReason,
  PublishNextView,
  PublishQuota,
  UnreadableRow,
} from "../types.js";
import type {
  OptimiseProposal,
  OptimiseState,
  SpendAllowance,
  SpendPrice,
  SpendReport,
} from "../types.js";
import type {
  Campaign,
  CampaignAttribution,
  ChannelLink,
  HookTestOpening,
  HookTestRole,
  HookTestSet,
  HookTestState,
  TrackedLink,
  ContentDetail,
  ContentExperimentSummary,
  ContentMediaItem,
  ContentMediaKind,
  ContentRecord,
  ContentState,
  ContentVariant,
  ContentVariantCount,
  ContentVariantKind,
  ContentVerdict,
  DigestApplication,
  DigestEntry,
  ResearchDigest,
  BriefState,
  BriefSummary,
  DeliveryEnvelope,
  DispatchAttempt,
  DispatchOutcome,
  DispatchSkipped,
  GateCheck,
  GateDisposition,
  GatePolicy,
  GateRecord,
  GateVerdict,
  JsonValue,
  OutgoingBrief,
  OutgoingBriefDetail,
  ChannelMemoryDetail,
  ChannelMemoryRevision,
  ProofRecord,
  ProofStatus,
  BriefType,
  BriefVariant,
  ExplainerBeats,
  LinkPlacement,
  SceneSpec,
  TypedBrief,
  AnalyticsMetricState,
  AnalyticsObservation,
  AnalyticsPost,
  AnalyticsSource,
  AnalyticsSummaryMetric,
  MetricImport,
} from "../types.js";
import { isKnownPlatform, isSocialPlatform } from "../platforms.js";
import { EngineReadError } from "./errors.js";
import type {
  EngineAccount,
  EngineCampaign,
  EngineDraft,
  EngineEntry,
  EngineHookTestOpening,
  EngineHookTestSet,
  EngineLinkAttribution,
  EngineMediaItem,
  EngineOptimiseProposal,
  EnginePublishJob,
  EnginePublishResolution,
  EngineInboxEvent,
  EngineInboxGet,
  EngineInboxStats,
  EngineInboxStatsGroup,
  EnginePublishNext,
  EnginePublishNextBlocked,
  EnginePublishNextJob,
  EnginePublishQuota,
  EngineScope,
  EngineSpendAllowance,
  EngineSpendPrice,
  EngineSpendReport,
  EngineTrackedLink,
  EngineDigestApplication,
  EngineDigestEntry,
  EngineDigestRecord,
  EngineContentExperimentSummary,
  EngineContentGet,
  EngineContentMediaItem,
  EngineContentRecord,
  EngineContentVariant,
  EngineContentVariantCount,
  EngineGatePolicy,
  EngineGateCheck,
  EngineGateRecord,
  EngineBriefOut,
  EngineDeliveryRecord,
  EngineDispatchAttempt,
  EngineMemoryGet,
  EngineMemoryRevisionRow,
  EngineProofRow,
  EngineBriefVariant,
  EngineExplainerBeats,
  EngineSceneSpec,
  EngineTypedBrief,
  EngineAnalyticsObservation,
  EngineAnalyticsSummary,
  EngineMetricImportRecord,
  EngineHeartbeatStatus,
  EngineRestoreReceipt,
  EngineRestoreReceiptList,
} from "./protocol.js";
import { hostAppName } from "../../appName.js";

// "remake" is the format an accepted optimise suggestion gives its draft calendar entry.
const FORMATS: readonly JobFormat[] = ["reel", "carousel", "photo", "short", "video", "text_post", "remake"];
const JOB_STATES: readonly JobState[] = [
  "draft",
  "media_pending",
  "ready",
  "scheduled",
  "published",
  "failed",
  "late",
  "paused",
];
const REJECT_REASONS: readonly RejectReason[] = ["wrong_content", "wrong_account", "timing", "media_issue", "other"];
/** The engine's default preparation window (calendar.list without prep_minutes). */
const PREP_MINUTES = 30;

function isOneOf<T extends string>(list: readonly T[], value: string): value is T {
  return (list as readonly string[]).includes(value);
}

function mappingError(message: string): EngineReadError {
  return new EngineReadError("engine_mapping", message);
}

/** A platform id as stored, when the desktop shows it (an account or draft-only platform). Any other id is an engine_mapping error. */
function platformText(platform: string, field: string): string {
  if (isKnownPlatform(platform)) return platform;
  throw mappingError(`${field} platform "${platform}" is not shown by the desktop yet`);
}

// ---- Keys -------------------------------------------------------------------------------------
// A PublishJob and its account carry the engine scope inside their ids, so a later call can rebuild
// the scope without a lookup. Each part is URI-encoded, then the parts are joined with "/".

/** The desktop's account key: the engine scope (venture, channel, account). */
export function accountKey(scope: EngineScope): string {
  return [scope.venture, scope.channel, scope.account].map((part) => encodeURIComponent(part)).join("/");
}

/** The desktop's job id: the account key plus the engine entry id. */
export function jobKey(scope: EngineScope, entryId: string): string {
  return `${accountKey(scope)}/${encodeURIComponent(entryId)}`;
}

/** Reverses jobKey. Null when the id is not one this data source issued. */
export function parseJobKey(id: string): { scope: EngineScope; entryId: string } | null {
  const parts = id.split("/");
  if (parts.length !== 4) return null;
  try {
    const [venture, channel, account, entryId] = parts.map((part) => decodeURIComponent(part));
    if (!venture || !channel || !account || !entryId) return null;
    return { scope: { venture, channel, account }, entryId };
  } catch {
    return null;
  }
}

/**
 * The desktop's account id: the engine scope plus the platform. The engine keys an account by scope
 * and platform. Jobs carry only the scope (`accountKey`), so a job does not name one of its
 * channel's accounts (see "Engine gaps" in the design doc).
 */
export function accountId(scope: EngineScope, platform: string): string {
  return `${accountKey(scope)}/${encodeURIComponent(platform)}`;
}

/** Reverses accountId. Null when the id is not one this data source issued. */
export function parseAccountId(id: string): { scope: EngineScope; platform: string } | null {
  const parts = id.split("/");
  if (parts.length !== 4) return null;
  try {
    const [venture, channel, account, platform] = parts.map((part) => decodeURIComponent(part));
    if (!venture || !channel || !account || !platform) return null;
    return { scope: { venture, channel, account }, platform };
  } catch {
    return null;
  }
}

/**
 * A calendar entry reference (a suggestion's, a link's, a digest application's, an attribution row's). It is the
 * entry's job key, the id PublishJob.id carries, so parseJobKey reads it back. Null when the engine names no entry.
 */
function calendarJobKey(scope: EngineScope, entryId: string | null): string | null {
  return entryId === null ? null : jobKey(scope, entryId);
}

// ---- Manual drafts ----------------------------------------------------------------------------
// A draft id has the same shape as a job id: the engine scope plus the engine's own draft id, so a
// later call rebuilds the scope without a lookup.

const DRAFT_STATES: readonly DraftState[] = ["drafted", "approved", "posted_by_operator", "skipped", "stale"];
const DRAFT_KINDS: readonly DraftKind[] = ["post", "comment", "reply"];

/** The desktop's draft id: the engine scope plus the engine draft id. */
export function draftKey(scope: EngineScope, draftId: string): string {
  return jobKey(scope, draftId);
}

/** Reverses draftKey. Null when the id is not one this data source issued. */
export function parseDraftKey(id: string): { scope: EngineScope; draftId: string } | null {
  const key = parseJobKey(id);
  return key ? { scope: key.scope, draftId: key.entryId } : null;
}

function draftState(state: string): DraftState {
  if (isOneOf(DRAFT_STATES, state)) return state;
  throw mappingError(`draft state "${state}" is not shown by the desktop yet`);
}

function draftKind(kind: string): DraftKind {
  if (isOneOf(DRAFT_KINDS, kind)) return kind;
  throw mappingError(`draft kind "${kind}" is not shown by the desktop yet`);
}

/** An engine draft as the desktop's ManualDraft. A state or kind the desktop does not show is an engine_mapping error. */
export function draftOf(engine: EngineDraft): ManualDraft {
  return {
    id: draftKey(engine.scope, engine.draft_id),
    channelLabel: `${engine.scope.venture} / ${engine.scope.channel} / ${engine.scope.account}`,
    platform: platformText(engine.platform, "draft"),
    kind: draftKind(engine.kind),
    target: engine.target,
    title: engine.title,
    body: engine.body,
    bodyChars: engine.body_chars,
    disclosureRequired: engine.disclosure_required,
    state: draftState(engine.state),
    revision: engine.revision,
    postedUrl: engine.posted_url,
    postedAtUtc: engine.posted_at,
    skipReason: engine.skip_reason,
    sourceNote: engine.source_note,
    updatedAtUtc: engine.updated_at,
  };
}

// ---- Instants and digests ---------------------------------------------------------------------

/** The engine's instant form: YYYY-MM-DDTHH:MM:SSZ (no milliseconds). */
export function engineInstant(iso: string): string {
  return new Date(iso).toISOString().replace(/\.\d{3}Z$/, "Z");
}

/**
 * FNV-1a over the caption's UTF-16 code units. The engine stores no caption digest, so this is a
 * display fingerprint computed here. It is not the daemon's SHA-256 and cannot be compared with it.
 */
export function captionFingerprint(caption: string): string {
  let hash = 0x811c9dc5;
  for (let i = 0; i < caption.length; i += 1) {
    hash ^= caption.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return `fnv1a:${hash.toString(16).padStart(8, "0")}`;
}

// ---- Rejections -------------------------------------------------------------------------------
// The engine stores one reason string. The desktop writes "<code>: <note>" (or just "<code>") and
// reads it back the same way. A string it did not write maps to "other" with the text as the note.

export function encodeRejection(code: RejectReason, note: string): string {
  const trimmed = note.trim();
  return trimmed === "" ? code : `${code}: ${trimmed}`;
}

export function parseRejection(text: string): { reason: RejectReason; note: string | null } {
  const sep = text.indexOf(": ");
  const head = sep === -1 ? text : text.slice(0, sep);
  if (isOneOf(REJECT_REASONS, head)) {
    return { reason: head, note: sep === -1 ? null : text.slice(sep + 2) };
  }
  return { reason: "other", note: text === "" ? null : text };
}

// ---- Entry fields -----------------------------------------------------------------------------

function required<T>(value: T | null, field: string, entry: EngineEntry): T {
  if (value === null) throw mappingError(`entry ${entry.entry_id} has no ${field}`);
  return value;
}

function jobFormat(format: string): JobFormat {
  if (isOneOf(FORMATS, format)) return format;
  throw mappingError(`format "${format}" is not shown by the desktop yet`);
}

function jobState(state: string): JobState {
  if (isOneOf(JOB_STATES, state)) return state;
  throw mappingError(`state "${state}" is not shown by the desktop yet`);
}

function mediaKind(kind: string): MediaKind {
  if (kind === "video" || kind === "image") return kind;
  throw mappingError(`media kind "${kind}" is not shown by the desktop yet`);
}

/**
 * The engine stores no media status. "missing" is the fail-closed value: the screens then treat the
 * media as not ready and do not offer approval. It is not a claim that the file is absent.
 */
function mediaItem(item: EngineMediaItem): MediaItem {
  return {
    hash: item.hash,
    kind: mediaKind(item.kind),
    durationSeconds: item.duration_ms === null ? null : item.duration_ms / 1000,
    durationMs: item.duration_ms === null ? null : countOf(item.duration_ms, "media duration"),
    dimensions: null,
    status: "missing",
    uploadPercent: null,
  };
}

/**
 * approvedAtUtc is the entry's updated_at. The engine does not store the approval time, so a pause
 * or resume after approval moves this value. The invalidation cause is not stored by the engine;
 * "caption_changed" is a placeholder that no screen shows.
 */
function approvalOf(entry: EngineEntry): Approval {
  switch (entry.approval_state) {
    case "none":
      return { status: "none" };
    case "pending":
      return { status: "pending" };
    case "approved":
      return {
        status: "approved",
        approvedRevision: required(entry.approved_entry_revision, "approved_entry_revision", entry),
        policyRevision: required(entry.policy_revision, "policy_revision", entry),
        approvedAtUtc: entry.updated_at,
      };
    case "rejected": {
      const { reason, note } = parseRejection(entry.rejection_reason ?? "");
      return { status: "rejected", reason, note };
    }
    case "invalidated":
      return {
        status: "invalidated",
        approvedRevision: required(entry.approved_entry_revision, "approved_entry_revision", entry),
        policyRevision: required(entry.policy_revision, "policy_revision", entry),
        cause: "caption_changed",
      };
    default:
      throw mappingError(`approval state "${entry.approval_state}" is not shown by the desktop yet`);
  }
}

/** Late is derived by the engine from the preparation window; the deadline is recomputed here. */
function latenessOf(entry: EngineEntry): Lateness | null {
  if (entry.effective_state !== "late") return null;
  const deadline = Date.parse(entry.scheduled_at) - PREP_MINUTES * 60_000;
  return { mediaNotReadyBy: engineInstant(new Date(deadline).toISOString()), policy: "held_for_review" };
}

/**
 * One engine entry as the operator's PublishJob. Fields the engine does not hold are set as the
 * design doc's "Engine gaps" section records: null or empty means "not reported", not "none".
 */
export function entryToJob(entry: EngineEntry): PublishJob {
  return {
    id: jobKey(entry.scope, entry.entry_id),
    // The account this entry belongs to, built exactly as accountOf builds an Account id (scope plus platform).
    // A job whose account the accounts list lacks keeps this id; it matches no Account row.
    accountId: accountId(entry.scope, entry.platform),
    briefId: entry.brief_id,
    format: jobFormat(entry.format),
    revision: entry.revision,
    caption: entry.caption,
    captionDigest: captionFingerprint(entry.caption),
    firstComment: null,
    settings: [],
    mediaItems: entry.media.map((item) => mediaItem(item)),
    scheduledAtUtc: entry.scheduled_at,
    timeZone: entry.timezone,
    dst: { kind: "none" },
    state: jobState(entry.effective_state),
    approval: approvalOf(entry),
    policyRevision: entry.policy_revision ?? null,
    lateness: latenessOf(entry),
    conflict: null,
  };
}

// ---- Accounts ---------------------------------------------------------------------------------
// The engine holds scope, platform, handle, connection, entitlements, cooldown and pause. The UI
// type also needs a time zone, a venture name, a display name, an entitlement stage and a colour
// band; the engine holds none of those, so each is set below to a named fallback (see the report in
// docs/CLI-POSTRIGHT.md, "Accounts"). A value the engine does not report is never shown as valid.

/**
 * The platforms the Account type covers are SOCIAL_PLATFORMS, the engine's PLATFORMS (crates/protocol/src/accounts.rs).
 * An account on any other platform is an unmapped account: its error is typed `unknown_platform`, and
 * listAccounts keeps it apart from the mapped accounts.
 */
function socialPlatform(platform: string): SocialPlatform {
  if (isSocialPlatform(platform)) return platform;
  throw new EngineReadError("unknown_platform", `platform "${platform}" is not shown by the desktop yet`);
}

/**
 * An engine account the desktop cannot map yet: its platform, or a state it does not show. It is
 * kept with its typed error so the list is not silently shorter than the engine's.
 */
export interface UnmappedAccount {
  /** The desktop account id the account would have had (accountId). */
  id: string;
  platform: string;
  error: EngineReadError;
}

/** The milliseconds of an engine instant. A value that does not parse as an instant is a mapping error, never a guess. */
function instantMs(value: string, field: string): number {
  const ms = Date.parse(value);
  if (Number.isNaN(ms)) throw mappingError(`${field} "${value}" is not an instant`);
  return ms;
}

/**
 * Token health from the connection state and the expiry. A connected account with no reported
 * expiry, or an expired token, reads as refresh_failed: the desktop does not claim a valid token it
 * cannot see. not_connected maps to revoked, the closest existing value. `unknown` fails closed.
 */
function tokenHealth(engine: EngineAccount, nowUtc: string): TokenHealth {
  switch (engine.connection_state) {
    case "connected": {
      const expires = engine.token_expires_at;
      if (expires === null || instantMs(expires, "token expiry") <= instantMs(nowUtc, "clock")) {
        return { status: "refresh_failed" };
      }
      return { status: "valid", expiresOn: expires.slice(0, 10) };
    }
    case "not_connected":
      return { status: "revoked" };
    case "reauth_required":
    case "unknown":
      return { status: "refresh_failed" };
    default:
      throw mappingError(`connection state "${engine.connection_state}" is not shown by the desktop yet`);
  }
}

/**
 * The single entitlement stage the screens show: the first of the three independent states that has
 * not passed. When all three have passed, the stage is audience_visible.
 */
function entitlementOf(engine: EngineAccount): Entitlement {
  if (engine.sandbox_mechanics !== "passed") return "sandbox_mechanics";
  if (engine.review_submission !== "passed") return "review_submitted";
  return "audience_visible";
}

/** A pause record only while paused. The engine stores no actor, so the desktop is named (the only writer). */
function pauseOf(engine: EngineAccount): PauseRecord | null {
  if (!engine.paused) return null;
  if (engine.paused_at === null) {
    throw mappingError(`account ${engine.platform} is paused without a pause time`);
  }
  return { by: "desktop", atUtc: engine.paused_at, reason: engine.paused_reason ?? "" };
}

/** A stable colour band (0 to 5) from the account id. Derived here, not reported by the engine. */
function bandOf(id: string): number {
  let hash = 0x811c9dc5;
  for (let i = 0; i < id.length; i += 1) {
    hash ^= id.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash % 6;
}

/**
 * One engine account as the operator's Account. `nowUtc` is the caller's clock (the data source
 * passes its own); this function reads no clock. A cooldown reads as active only while it is after
 * `nowUtc`.
 */
export function accountOf(engine: EngineAccount, nowUtc: string): Account {
  const id = accountId(engine.scope, engine.platform);
  const until = engine.quota_cooldown_until;
  const cooldown = until !== null && instantMs(until, "quota cooldown") > instantMs(nowUtc, "clock") ? until : null;
  return {
    id,
    // Engine gap: the venture has no name in the engine, so its id is shown.
    venture: { id: engine.scope.venture, name: engine.scope.venture },
    platform: socialPlatform(engine.platform),
    handle: engine.handle,
    // Engine gap: no display name is stored for most accounts, so the handle is shown.
    displayName: engine.display_name === "" ? engine.handle : engine.display_name,
    // The handle is the fixture's platform account id, so its last four characters are the suffix.
    idSuffix: engine.handle.slice(-4),
    // Engine gap: the engine holds no time zone for an account. UTC is a named fallback, not a fact.
    timeZone: "UTC",
    band: bandOf(id),
    readiness: {
      token: tokenHealth(engine, nowUtc),
      // Engine gap: the engine stores no re-authorisation date.
      reauthNeededBy: null,
      entitlement: entitlementOf(engine),
      quotaCooldownUntil: cooldown,
      // The engine stores only a dated instant, so a cooldown is shown as dated.
      quotaCooldownSource: cooldown === null ? null : "dated_retry_after",
      paused: pauseOf(engine),
    },
  };
}

// ---- Publish jobs -----------------------------------------------------------------------------
// The engine's publish job is read-only here. Its seven states map one to one onto
// PublishProgressState. Publish reads (publish.get, publish.list) run in every engine role, so these
// functions run on the engine's real jobs. Writes stay daemon-only. See "Publish jobs" in the design doc.

const PUBLISH_STATES: readonly PublishProgressState[] = [
  "queued",
  "running",
  "waiting",
  "published",
  "failed",
  "outcome_unknown",
  "cancelled",
];

/** The engine's latest error code as one plain sentence: what happened. Unlisted codes use the engine's message. */
const PUBLISH_CAUSE: Partial<Record<string, string>> = {
  token_expired: "The platform rejected the account's token, so the post did not go out.",
  platform_rejected: "The platform refused this post.",
  container_failed: "The platform could not process the uploaded media.",
  container_timeout: "The platform did not finish processing the media in time.",
  retries_exhausted: `The platform kept failing or limiting requests, so ${hostAppName()} stopped.`,
  approval_invalidated: "The post changed or lost its approval before it went out.",
  account_paused: "The account was paused before the post went out.",
};

/** The single next action for each latest error code. The daemon host runs any retry; the desktop never does. */
const PUBLISH_NEXT: Partial<Record<string, string>> = {
  token_expired: "Reconnect this account on the daemon host. Nothing is retried until then.",
  platform_rejected: "Change the post, then approve the new revision.",
  container_failed: "Check the media on the daemon host, then approve the post again.",
  container_timeout: "Check the account on the platform, then approve the post again if it should go out.",
  retries_exhausted: "Check the account on the platform before approving the post again.",
  approval_invalidated: "Review the current revision and approve it again if it should go out.",
  account_paused: "Resume the account, then approve the post again.",
};

/**
 * The Failures card shows the required explanation ("The platform may have accepted this post...")
 * for every outcome_unknown item, so this summary states only the fact.
 */
const OUTCOME_UNKNOWN_SUMMARY = "The daemon's publish attempt ended without a confirmed result.";
const OUTCOME_UNKNOWN_NEXT = "Wait for the daemon's platform check. The desktop cannot start it.";

function causeOf(code: string | null, message: string | null): string {
  if (code === null) return "The publish stopped and the engine gave no reason.";
  return PUBLISH_CAUSE[code] ?? message ?? `The publish stopped (${code}).`;
}

function nextOf(code: string | null): string {
  return (code === null ? undefined : PUBLISH_NEXT[code]) ?? "Review the post on the daemon host before approving it again.";
}

/** One engine publish job as the desktop's progress view. Throws on a state the desktop does not show. */
export function publishProgressOf(engine: EnginePublishJob): PublishProgress {
  if (!isOneOf(PUBLISH_STATES, engine.state)) {
    throw mappingError(`publish state "${engine.state}" is not shown by the desktop yet`);
  }
  return {
    state: engine.state,
    permalink: engine.remote.permalink,
    resumeAtUtc: engine.state === "waiting" ? engine.next_attempt_at : null,
    errorCode: engine.last_error_code,
    errorMessage: engine.last_error_message,
  };
}

/**
 * The publish job for one calendar entry at its current revision, or null when there is none. A job
 * for an older revision is not shown: the entry changed after that job was approved.
 */
export function publishProgressFor(jobs: readonly EnginePublishJob[], entry: EngineEntry): PublishProgress | null {
  const job = jobs.find((j) => j.entry_id === entry.entry_id && j.entry_revision === entry.revision);
  return job ? publishProgressOf(job) : null;
}

/**
 * A failure row for a publish job that needs the operator: failed, outcome_unknown, or cancelled with
 * a reason. Queued, running, waiting and published jobs need no failure row (null).
 */
export function publishFailureOf(engine: EnginePublishJob): FailureItem | null {
  const progress = publishProgressOf(engine);
  const base: Omit<FailureItem, "kind" | "summary" | "nextAction"> = {
    id: `publish-${engine.job_id}`,
    alertKind: null,
    accountId: accountId(engine.scope, engine.platform),
    jobId: jobKey(engine.scope, engine.entry_id),
    atUtc: engine.updated_at,
    attempts: engine.attempts,
    status: "open",
    reconcile: null,
    resolution: null,
    resolutionNote: null,
  };
  switch (progress.state) {
    case "failed":
      return {
        ...base,
        kind: "dead_letter",
        summary: causeOf(progress.errorCode, progress.errorMessage),
        nextAction: nextOf(progress.errorCode),
      };
    case "outcome_unknown":
      return { ...base, kind: "outcome_unknown", summary: OUTCOME_UNKNOWN_SUMMARY, nextAction: OUTCOME_UNKNOWN_NEXT };
    case "cancelled":
      if (progress.errorCode === null) return null;
      return {
        ...base,
        kind: "dead_letter",
        summary: causeOf(progress.errorCode, progress.errorMessage),
        nextAction: nextOf(progress.errorCode),
      };
    case "queued":
    case "running":
    case "waiting":
    case "published":
      return null;
  }
}

// ---- Publish jobs, read only (the Publish jobs screen) ----------------------------------------

const PUBLISH_DECISIONS: readonly PublishResolutionDecision[] = ["posted", "not_posted"];

/** What a person recorded for an outcome_unknown job. Null on every other job. An unknown decision is a mapping error. */
function publishResolutionOf(engine: EnginePublishResolution | null | undefined): PublishJobResolution | null {
  if (engine === null || engine === undefined) return null;
  if (!isOneOf(PUBLISH_DECISIONS, engine.decision)) {
    throw mappingError(`publish resolution "${engine.decision}" is not shown by the desktop yet`);
  }
  return {
    decision: engine.decision,
    resolvedBy: engine.resolved_by,
    resolvedAtUtc: engine.resolved_at,
    note: engine.note,
    remoteIdEnteredByHand: engine.remote_id_entered_by_hand,
  };
}

/** One engine publish job as the desktop's read-only row. publishProgressOf refuses a state the desktop does not show. */
export function publishJobRecordOf(engine: EnginePublishJob): PublishJobRecord {
  const progress = publishProgressOf(engine);
  return {
    scopeId: accountKey(engine.scope),
    jobId: engine.job_id,
    // The calendar entry's id is its job key, the same id PublishJob.id carries, so it parses back with parseJobKey.
    entryId: jobKey(engine.scope, engine.entry_id),
    entryRevision: countOf(engine.entry_revision, "entry revision"),
    platform: platformText(engine.platform, "publish job"),
    state: progress.state,
    phase: engine.phase,
    remotePostId: engine.remote.post_id,
    permalink: engine.remote.permalink,
    attempts: countOf(engine.attempts, "publish attempts"),
    nextAttemptAtUtc: engine.next_attempt_at,
    errorCode: engine.last_error_code,
    errorMessage: engine.last_error_message,
    resolution: publishResolutionOf(engine.resolution),
    createdAtUtc: engine.created_at,
    updatedAtUtc: engine.updated_at,
  };
}

// ---- Suggestions and spend ----------------------------------------------------------------------
// A scope id is the account key (venture, channel, account), each part URI-encoded. Amounts pass only
// when they are safe integers. Anything else is an engine_mapping error, never a rounded value.

/** The scope a suggestion or spend id names. Null when the id is not one this data source issued. */
export function parseScopeKey(id: string): EngineScope | null {
  const parts = id.split("/");
  if (parts.length !== 3) return null;
  try {
    const [venture, channel, account] = parts.map((part) => decodeURIComponent(part));
    if (!venture || !channel || !account) return null;
    return { venture, channel, account };
  } catch {
    return null;
  }
}

const OPTIMISE_STATES: readonly OptimiseState[] = ["proposed", "accepted", "dismissed"];
const INTEGER_TEXT = /^-?\d+$/;

/** The engine sends value and threshold as exact integer text. Anything else is refused, not rounded. */
function wholeText(text: string, field: string, engine: EngineOptimiseProposal): string {
  if (!INTEGER_TEXT.test(text)) {
    throw mappingError(`proposal ${engine.proposal_id} has a ${field} that is not a whole number`);
  }
  return text;
}

/** One optimise proposal as the desktop's OptimiseProposal. Only the remake rule and the three states are shown. */
export function proposalOf(engine: EngineOptimiseProposal): OptimiseProposal {
  if (engine.kind !== "remake") throw mappingError(`optimise rule "${engine.kind}" is not shown by the desktop yet`);
  if (!isOneOf(OPTIMISE_STATES, engine.state)) {
    throw mappingError(`optimise state "${engine.state}" is not shown by the desktop yet`);
  }
  return {
    id: engine.proposal_id,
    scopeId: accountKey(engine.scope),
    kind: "remake",
    state: engine.state,
    revision: engine.revision,
    sourceJobId: engine.source_job_id,
    platform: platformText(engine.platform, "optimise"),
    metric: engine.metric,
    window: engine.window,
    observedAtUtc: engine.observed_at,
    value: wholeText(engine.value, "value", engine),
    threshold: wholeText(engine.threshold, "threshold", engine),
    sampleSize: engine.sample_size,
    decileSize: engine.decile_size,
    dueAtUtc: engine.due_at,
    lineageJobIds: engine.lineage_job_ids,
    lineagePullIds: engine.lineage_pull_ids,
    decisionNote: engine.decision_note,
    decidedAtUtc: engine.decided_at,
    calendarEntryId: calendarJobKey(engine.scope, engine.calendar_entry_id),
    createdAtUtc: engine.created_at,
    updatedAtUtc: engine.updated_at,
  };
}

/** A money amount from the engine. Only a safe integer passes; any other value is a mapping error. */
function micro(value: number, field: string): number {
  if (!Number.isSafeInteger(value)) throw mappingError(`${field} is not a safe whole number of micro-units`);
  return value;
}

export function allowanceOf(engine: EngineSpendAllowance): SpendAllowance {
  return {
    month: engine.month,
    currency: engine.currency,
    limitMicro: micro(engine.limit_micro, "limit"),
    reservedMicro: micro(engine.reserved_micro, "reserved"),
    committedMicro: micro(engine.committed_micro, "committed"),
    remainingMicro: micro(engine.remaining_micro, "remaining"),
    revision: engine.revision,
    updatedAtUtc: engine.updated_at,
  };
}

export function spendReportOf(engine: EngineSpendReport): SpendReport {
  return {
    month: engine.month,
    currency: engine.currency,
    limitMicro: micro(engine.limit_micro, "limit"),
    reservedMicro: micro(engine.reserved_micro, "reserved"),
    committedMicro: micro(engine.committed_micro, "committed"),
    remainingMicro: micro(engine.remaining_micro, "remaining"),
    reconciles: engine.reconciles,
    routes: engine.routes.map((route) => ({
      routeId: route.route_id,
      countReserved: route.count_reserved,
      countCommitted: route.count_committed,
      countReleased: route.count_released,
      countExpired: route.count_expired,
      openMicro: micro(route.open_micro, "open estimate"),
      forecastMicro: micro(route.forecast_micro, "forecast"),
      measuredMicro: micro(route.measured_micro, "measured"),
      varianceMicro: micro(route.variance_micro, "variance"),
      overReservations: route.over_reservations,
    })),
  };
}

export function priceOf(engine: EngineSpendPrice): SpendPrice {
  return {
    routeId: engine.route_id,
    version: engine.version,
    provider: engine.provider,
    modelSlug: engine.model_slug,
    unit: engine.unit,
    amountMicro: micro(engine.amount_micro, "price"),
    currency: engine.currency,
    sourceUrl: engine.source_url,
    sourcePathLine: engine.source_path_line,
    fetchedAtUtc: engine.fetched_at,
    verifiedBy: engine.verified_by,
    qualified: engine.qualified,
    reason: engine.reason,
  };
}

// ---- Campaigns, tracked links and attribution -------------------------------------------------
// Each row carries its engine scope, which the desktop keeps as the scope id (accountKey). Campaign, link and
// hook test ids are the engine's own ids, shown as stored. A row from another scope is never returned by
// the scoped reads, so no id here is joined across channels.

/** A count from the engine: a safe, non-negative whole number. Anything else is a mapping error. */
function countOf(value: number, field: string): number {
  if (!Number.isSafeInteger(value) || value < 0) throw mappingError(`${field} is not a whole count`);
  return value;
}

/** One engine campaign as the desktop's Campaign. */
export function campaignOf(engine: EngineCampaign): Campaign {
  return {
    id: engine.campaign_id,
    scopeId: accountKey(engine.scope),
    name: engine.name,
    utmCampaign: engine.utm_campaign,
    revision: countOf(engine.revision, "campaign revision"),
    createdAtUtc: engine.created_at,
    updatedAtUtc: engine.updated_at,
  };
}

/** One engine tracked link as the desktop's TrackedLink. The UTM parts and final URL are shown as stored. */
export function linkOf(engine: EngineTrackedLink): TrackedLink {
  return {
    id: engine.link_id,
    scopeId: accountKey(engine.scope),
    campaignId: engine.campaign_id,
    variantId: engine.variant_id,
    platform: platformText(engine.platform, "link"),
    destination: engine.destination,
    calendarEntryId: calendarJobKey(engine.scope, engine.calendar_entry_id),
    utmSource: engine.utm_source,
    utmMedium: engine.utm_medium,
    utmCampaign: engine.utm_campaign,
    utmContent: engine.utm_content,
    finalUrl: engine.final_url,
    createdAtUtc: engine.created_at,
  };
}

/**
 * The visit counts of one campaign. The engine's total must equal the sum of its rows; a mismatch is
 * refused, not shown.
 */
export function attributionOf(engine: EngineLinkAttribution): CampaignAttribution {
  const links = engine.links.map((row) => ({
    linkId: row.link_id,
    variantId: row.variant_id,
    platform: platformText(row.platform, "link attribution"),
    calendarEntryId: calendarJobKey(engine.scope, row.calendar_entry_id),
    visits: countOf(row.visits, "visit count"),
  }));
  const total = countOf(engine.total_visits, "total visits");
  const sum = links.reduce((acc, row) => acc + row.visits, 0);
  if (sum !== total) throw mappingError(`attribution for ${engine.campaign_id} does not add up to its total`);
  return { campaignId: engine.campaign_id, totalVisits: total, links };
}

/**
 * The links of one campaign, each with the campaign's name and its count from the attribution reply of the same
 * campaign. A link the attribution reply does not list has no count (null), never a zero.
 */
export function channelLinksOf(
  campaignName: string,
  links: readonly EngineTrackedLink[],
  attribution: EngineLinkAttribution,
): ChannelLink[] {
  const counts = new Map(attributionOf(attribution).links.map((row) => [row.linkId, row.visits] as const));
  return links.map((link) => ({
    link: linkOf(link),
    campaignName,
    visits: counts.get(link.link_id) ?? null,
  }));
}

// ---- Hook tests -------------------------------------------------------------------------------

const HOOK_STATES: readonly HookTestState[] = ["open", "decided"];
const HOOK_ROLES: readonly HookTestRole[] = ["promoted", "archived"];

function hookState(state: string): HookTestState {
  if (isOneOf(HOOK_STATES, state)) return state;
  throw mappingError(`hook test state "${state}" is not shown by the desktop yet`);
}

function hookRole(role: string | null): HookTestRole | null {
  if (role === null) return null;
  if (isOneOf(HOOK_ROLES, role)) return role;
  throw mappingError(`hook test role "${role}" is not shown by the desktop yet`);
}

/** A hook test value is exact integer text. Anything else is refused, not rounded. */
function hookValue(value: string | null, setId: string, index: number): string | null {
  if (value === null) return null;
  if (!INTEGER_TEXT.test(value)) {
    throw mappingError(`hook test ${setId} opening ${index} has a value that is not a whole number`);
  }
  return value;
}

function hookOpeningOf(engine: EngineHookTestOpening, setId: string): HookTestOpening {
  return {
    index: countOf(engine.opening_index, "opening index"),
    text: engine.text,
    jobId: engine.job_id,
    boundAtUtc: engine.bound_at,
    role: hookRole(engine.role),
    value: hookValue(engine.value, setId, engine.opening_index),
    observedAtUtc: engine.observed_at,
    pullId: engine.pull_id,
    publishedAtUtc: engine.published_at,
  };
}

/**
 * One engine hook test set as the desktop's HookTestSet. A decided set must name its winner and its
 * decision instant; a set missing either is a mapping error.
 */
export function hookTestOf(engine: EngineHookTestSet): HookTestSet {
  const state = hookState(engine.state);
  const winner = engine.winner_opening === null ? null : countOf(engine.winner_opening, "winner opening");
  if (state === "decided" && (winner === null || engine.decided_at === null)) {
    throw mappingError(`hook test ${engine.set_id} is decided without a winner or a decision time`);
  }
  return {
    id: engine.set_id,
    scopeId: accountKey(engine.scope),
    briefRef: engine.brief_ref,
    body: engine.body,
    metric: engine.metric,
    window: engine.window,
    state,
    revision: countOf(engine.revision, "hook test revision"),
    winnerIndex: winner,
    decidedAtUtc: engine.decided_at,
    createdAtUtc: engine.created_at,
    updatedAtUtc: engine.updated_at,
    openings: engine.openings.map((opening) => hookOpeningOf(opening, engine.set_id)),
  };
}

// ---- Research digests and content library -----------------------------------------------------
// Text the engine stores (topic, finding, source, platform, format) is shown as stored. Counts and
// durations are whole numbers; anything else is an engine_mapping error, never a rounded value.

const CONTENT_STATES: readonly ContentState[] = ["active", "withdrawn", "expired"];
const CONTENT_MEDIA_KINDS: readonly ContentMediaKind[] = ["image", "video"];
const CONTENT_VARIANT_KINDS: readonly ContentVariantKind[] = ["caption", "cta"];
const CONTENT_VERDICTS: readonly ContentVerdict[] = ["insufficient_evidence", "evidence_sufficient"];

/** One digest entry as the desktop's DigestEntry. */
function digestEntryOf(engine: EngineDigestEntry): DigestEntry {
  return {
    id: engine.entry_id,
    topic: engine.topic,
    finding: engine.finding,
    source: engine.source,
    fetchedOn: engine.fetched_on,
    platform: engine.platform,
    format: engine.format,
  };
}

/** One stored digest as the desktop's ResearchDigest. The id is the engine's digest id, within its scope. */
export function digestOf(engine: EngineDigestRecord): ResearchDigest {
  return {
    id: engine.digest_id,
    scopeId: accountKey(engine.scope),
    digestDate: engine.digest_date,
    entries: engine.entries.map((entry) => digestEntryOf(entry)),
    createdAtUtc: engine.created_at,
  };
}

/** One applied digest entry as the desktop's DigestApplication. The entry's scope comes from the digest.applied reply. */
export function digestApplicationOf(engine: EngineDigestApplication, scope: EngineScope): DigestApplication {
  return {
    digestId: engine.digest_id,
    digestEntryId: engine.digest_entry_id,
    briefId: engine.brief_id,
    calendarEntryId: jobKey(scope, engine.entry_id),
    appliedAtUtc: engine.applied_at,
  };
}

function contentStateOf(state: string): ContentState {
  if (isOneOf(CONTENT_STATES, state)) return state;
  throw mappingError(`content state "${state}" is not shown by the desktop yet`);
}

function contentMediaKindOf(kind: string): ContentMediaKind {
  if (isOneOf(CONTENT_MEDIA_KINDS, kind)) return kind;
  throw mappingError(`content media kind "${kind}" is not shown by the desktop yet`);
}

function contentVariantKindOf(kind: string): ContentVariantKind {
  if (isOneOf(CONTENT_VARIANT_KINDS, kind)) return kind;
  throw mappingError(`content variant kind "${kind}" is not shown by the desktop yet`);
}

function contentVerdictOf(verdict: string): ContentVerdict {
  if (isOneOf(CONTENT_VERDICTS, verdict)) return verdict;
  throw mappingError(`content verdict "${verdict}" is not shown by the desktop yet`);
}

function contentMediaOf(item: EngineContentMediaItem): ContentMediaItem {
  return {
    hash: item.hash,
    kind: contentMediaKindOf(item.kind),
    durationMs: item.duration_ms === null ? null : countOf(item.duration_ms, "media duration"),
  };
}

/** One stored content record as the desktop's ContentRecord. The state is the engine's derived state. */
export function contentRecordOf(engine: EngineContentRecord): ContentRecord {
  return {
    id: engine.content_id,
    scopeId: accountKey(engine.scope),
    title: engine.title,
    media: engine.media.map((item) => contentMediaOf(item)),
    baseCaption: engine.base_caption,
    evergreen: engine.evergreen,
    cooldownDays: countOf(engine.cooldown_days, "cooldown days"),
    expiresAtUtc: engine.expires_at,
    state: contentStateOf(engine.state),
    withdrawalReason: engine.withdrawal_reason,
    revision: countOf(engine.revision, "content revision"),
    createdAtUtc: engine.created_at,
    updatedAtUtc: engine.updated_at,
  };
}

/** One content variant as the desktop's ContentVariant. */
export function contentVariantOf(engine: EngineContentVariant): ContentVariant {
  return {
    id: engine.variant_id,
    scopeId: accountKey(engine.scope),
    contentId: engine.content_id,
    kind: contentVariantKindOf(engine.kind),
    text: engine.text,
    createdAtUtc: engine.created_at,
  };
}

function contentVariantCountOf(engine: EngineContentVariantCount): ContentVariantCount {
  return {
    variantId: engine.variant_id,
    text: engine.text,
    planned: countOf(engine.planned, "planned count"),
    published: countOf(engine.published, "published count"),
    cancelled: countOf(engine.cancelled, "cancelled count"),
    meetsMinimum: engine.meets_minimum,
  };
}

/** The record and variants of one content.get reply, as the desktop's ContentDetail. */
export function contentDetailOf(engine: EngineContentGet): ContentDetail {
  return {
    record: contentRecordOf(engine.record),
    variants: engine.variants.map((variant) => contentVariantOf(variant)),
  };
}

/** The variant comparison of one content.experiment_summary reply. The verdict is checked, never reworded. */
export function experimentSummaryOf(engine: EngineContentExperimentSummary): ContentExperimentSummary {
  return {
    scopeId: accountKey(engine.scope),
    contentId: engine.content_id,
    minOccurrences: countOf(engine.min_occurrences, "minimum occurrences"),
    verdict: contentVerdictOf(engine.verdict),
    reason: engine.reason,
    caption: engine.caption.map((row) => contentVariantCountOf(row)),
    cta: engine.cta.map((row) => contentVariantCountOf(row)),
  };
}

// ---- Text gate, briefs, deliveries and dispatch attempts (read only) ---------------------------
// Thresholds are integers in thousandths and pass through as whole numbers; the screens write them as
// decimals. Every enum is checked against its list: an unknown value is an engine_mapping error, never a
// guess. A document field is read only when it has the expected type; a field it lacks is null or empty.

const GATE_DISPOSITIONS: readonly GateDisposition[] = ["pass", "hold", "block"];
const GATE_VERDICTS: readonly GateVerdict[] = ["pass", "fail", "inconclusive", "unverified", "hold", "not_applicable"];
const BRIEF_STATES: readonly BriefState[] = ["issued", "partial", "delivered"];
const DISPATCH_OUTCOMES: readonly DispatchOutcome[] = [
  "running",
  "delivered",
  "partial",
  "awaiting_approval",
  "engine_failed",
  "timed_out",
  "delivery_invalid",
  "engine_missing",
  "interrupted",
];

/** A whole number the engine sends. Negative values are kept, because some fields may be negative. */
function safeIntOf(value: number, field: string): number {
  if (!Number.isSafeInteger(value)) throw mappingError(`${field} is not a whole number`);
  return value;
}

function gateDispositionOf(value: string): GateDisposition {
  if (isOneOf(GATE_DISPOSITIONS, value)) return value;
  throw mappingError(`gate disposition "${value}" is not shown by the desktop yet`);
}

function gateVerdictOf(value: string): GateVerdict {
  if (isOneOf(GATE_VERDICTS, value)) return value;
  throw mappingError(`gate verdict "${value}" is not shown by the desktop yet`);
}

function briefStateOf(value: string): BriefState {
  if (isOneOf(BRIEF_STATES, value)) return value;
  throw mappingError(`brief state "${value}" is not shown by the desktop yet`);
}

function dispatchOutcomeOf(value: string): DispatchOutcome {
  if (isOneOf(DISPATCH_OUTCOMES, value)) return value;
  throw mappingError(`dispatch outcome "${value}" is not shown by the desktop yet`);
}

/** The latest policy revision as the desktop's GatePolicy. */
export function gatePolicyOf(engine: EngineGatePolicy): GatePolicy {
  const body = engine.policy;
  return {
    scopeId: accountKey(engine.scope),
    revision: countOf(engine.revision, "gate policy revision"),
    createdAtUtc: engine.created_at,
    bannedPhrases: body.banned_phrases,
    stockOpeners: body.stock_openers,
    minWords: countOf(body.min_words, "minimum words"),
    dashMaxPer100WordsMilli: countOf(body.dash_max_per_100_words_milli, "dash maximum"),
    semicolonMaxPer100WordsMilli: countOf(body.semicolon_max_per_100_words_milli, "semicolon maximum"),
    varianceMinMilli: safeIntOf(body.variance_min_milli, "variance minimum"),
    varianceMinSentences: countOf(body.variance_min_sentences, "variance minimum sentences"),
    tricolonMaxPer100SentencesMilli: countOf(body.tricolon_max_per_100_sentences_milli, "tricolon maximum"),
    tricolonMinSentences: countOf(body.tricolon_min_sentences, "tricolon minimum sentences"),
    required: {
      bannedPhrase: body.required.banned_phrase,
      dashDensity: body.required.dash_density,
      semicolonDensity: body.required.semicolon_density,
      sentenceLengthVariance: body.required.sentence_length_variance,
      tricolonFrequency: body.required.tricolon_frequency,
      stockOpener: body.required.stock_opener,
    },
  };
}

/** One gate.check reply as the desktop's GateCheck. The desktop never asks the engine to record, so nothing is stored. */
export function gateCheckOf(engine: EngineGateCheck): GateCheck {
  return {
    subjectId: engine.subject_id,
    disposition: gateDispositionOf(engine.disposition),
    policyRevision: countOf(engine.policy_revision, "gate policy revision"),
    textHash: engine.text_hash,
    rules: engine.rules.map((rule) => ({
      ruleId: rule.rule_id,
      required: rule.required,
      verdict: gateVerdictOf(rule.verdict),
      blocks: rule.blocks,
      reason: rule.reason,
    })),
    blockingRules: engine.blocking_rules,
    proofIdsUsed: engine.proof_ids_used,
  };
}

/** One stored verdict record. The scope is the one the read was made in, because the record carries none. */
export function gateRecordOf(engine: EngineGateRecord, scopeId: string): GateRecord {
  return {
    scopeId,
    recordSeq: countOf(engine.record_seq, "record sequence"),
    subjectId: engine.subject_id,
    textHash: engine.text_hash,
    policyRevision: countOf(engine.policy_revision, "gate policy revision"),
    disposition: gateDispositionOf(engine.disposition),
    checkedAtUtc: engine.checked_at,
  };
}

/** One field of a JSON object. Undefined when the value is not an object or the field is absent. */
function fieldOf(value: JsonValue | undefined, key: string): JsonValue | undefined {
  if (value === undefined || value === null || typeof value !== "object" || Array.isArray(value)) return undefined;
  return value[key];
}

function textOf(value: JsonValue | undefined): string | null {
  return typeof value === "string" ? value : null;
}

function wholeOf(value: JsonValue | undefined): number | null {
  return typeof value === "number" && Number.isSafeInteger(value) ? value : null;
}

/** The strings of a list. A list item that is not a string is kept as its JSON text, never dropped. */
function textsOf(value: JsonValue | undefined): string[] {
  return Array.isArray(value) ? value.map((item) => (typeof item === "string" ? item : JSON.stringify(item))) : [];
}

/** The fields of a brief document the desktop shows. Anything the document lacks stays null or empty. */
export function briefSummaryOf(document: JsonValue): BriefSummary {
  const review = fieldOf(document, "review");
  const stages = fieldOf(document, "stages");
  return {
    briefType: textOf(fieldOf(document, "briefType")),
    platforms: textsOf(fieldOf(document, "platforms")),
    deliverables: textsOf(fieldOf(document, "deliverables")),
    hook: textOf(fieldOf(document, "hook")),
    reviewMode: textOf(fieldOf(review, "mode")),
    policyRevision: wholeOf(fieldOf(review, "policyRevision")),
    stageCount: Array.isArray(stages) ? stages.length : 0,
  };
}

/** One outgoing brief as the list shows it. The document is left out of the list. */
export function briefOf(engine: EngineBriefOut): OutgoingBrief {
  return {
    scopeId: accountKey(engine.scope),
    requestId: engine.request_id,
    briefId: engine.brief_id,
    revision: countOf(engine.revision, "brief revision"),
    state: briefStateOf(engine.state),
    createdAtUtc: engine.created_at,
    updatedAtUtc: engine.updated_at,
  };
}

/** One outgoing brief with its summary and the document as stored. */
export function briefDetailOf(engine: EngineBriefOut): OutgoingBriefDetail {
  return { ...briefOf(engine), summary: briefSummaryOf(engine.document), document: engine.document };
}

/** One accepted delivery envelope as the desktop's DeliveryEnvelope. */
export function deliveryOf(engine: EngineDeliveryRecord): DeliveryEnvelope {
  return {
    scopeId: accountKey(engine.scope),
    envelopeId: engine.envelope_id,
    requestId: engine.request_id,
    deliverable: engine.deliverable,
    briefId: engine.brief_id,
    briefRevision: countOf(engine.brief_revision, "brief revision"),
    acceptedAtUtc: engine.accepted_at,
    document: engine.document,
  };
}

/** One dispatch attempt. The message and stderr tail are read from the engine's detail object when present. */
export function dispatchAttemptOf(engine: EngineDispatchAttempt, scopeId: string): DispatchAttempt {
  return {
    scopeId,
    attemptId: countOf(engine.attempt_id, "attempt id"),
    requestId: engine.request_id,
    engine: engine.engine,
    startedAtUtc: engine.started_at,
    finishedAtUtc: engine.finished_at,
    exitCode: engine.exit_code === null ? null : safeIntOf(engine.exit_code, "exit code"),
    outcome: dispatchOutcomeOf(engine.outcome),
    stderrTail: textOf(fieldOf(engine.detail, "stderr_tail")),
    message: textOf(fieldOf(engine.detail, "message")),
    producedDeliverables: textsOf(fieldOf(engine.detail, "deliverables")),
    skipped: skippedOf(fieldOf(engine.detail, "skipped")),
  };
}

/** The engine's `skipped` list as recorded on an attempt. An entry that is not an object is kept as an unreadable row. */
function skippedOf(value: JsonValue | undefined): DispatchSkipped[] {
  if (!Array.isArray(value)) return [];
  const entries: DispatchSkipped[] = [];
  for (const entry of value) {
    if (entry === null || typeof entry !== "object" || Array.isArray(entry)) {
      entries.push({ deliverable: null, code: null, message: "The engine sent a skipped entry the desktop cannot read." });
      continue;
    }
    entries.push({
      deliverable: textOf(entry["deliverable"] ?? entry["preset"]),
      code: textOf(entry["code"]),
      message: textOf(entry["message"]),
    });
  }
  return entries;
}

// ---- Channel memory and the proof bank (read only) --------------------------------------------
// memory.get returns one revision with its document; memory.history returns the revision rows without
// documents. proof.list returns each proof's value as exact decimal text, which is kept verbatim. A status the
// desktop does not show is an engine_mapping error.

const PROOF_STATUSES: readonly ProofStatus[] = ["collected", "approved", "withdrawn", "superseded"];

/** One stored memory revision without its document. The scope is the one the read was made in. */
export function memoryRevisionOf(engine: EngineMemoryRevisionRow, scopeId: string): ChannelMemoryRevision {
  return {
    scopeId,
    revision: countOf(engine.revision, "memory revision"),
    supersedes: engine.supersedes === null ? null : countOf(engine.supersedes, "superseded memory revision"),
    author: engine.author,
    createdAtUtc: engine.created_at,
    contentHash: engine.content_hash,
  };
}

/** The latest memory revision with its document. A document that is not a JSON object is a mapping error. */
export function memoryDetailOf(engine: EngineMemoryGet): ChannelMemoryDetail {
  const { document } = engine;
  if (document === null || typeof document !== "object" || Array.isArray(document)) {
    throw mappingError(`memory revision ${engine.revision} has no document object`);
  }
  return { ...memoryRevisionOf(engine, accountKey(engine.scope)), document };
}

function proofStatusOf(status: string): ProofStatus {
  if (isOneOf(PROOF_STATUSES, status)) return status;
  throw mappingError(`proof status "${status}" is not shown by the desktop yet`);
}

/** One proof row. The value is kept as the exact decimal text the engine sent; it is never parsed. */
export function proofOf(engine: EngineProofRow): ProofRecord {
  if (typeof engine.value !== "string") throw mappingError(`proof ${engine.proof_id} has a value that is not text`);
  return {
    scopeId: accountKey({ venture: engine.venture, channel: engine.channel, account: engine.account }),
    answerId: engine.answer_id,
    proofId: engine.proof_id,
    status: proofStatusOf(engine.status),
    subject: engine.subject,
    value: engine.value,
    units: engine.units,
    origin: engine.origin,
    observedFrom: engine.observed_from,
    observedTo: engine.observed_to,
    supersededBy: engine.superseded_by,
    withdrawalNote: engine.withdrawal_note,
  };
}

// ---- Typed briefs and platform variants (read only) --------------------------------------------
// A brief type or link placement the desktop does not show is an engine_mapping error, never a guess. A caption
// is kept exactly as the engine stored it: the desktop does not split hashtags out or count the caption length.

const BRIEF_TYPES: readonly BriefType[] = [
  "talking_head",
  "faceless_cartoon_short",
  "explainer",
  "carousel",
  "ugc_character_review",
  "product_pitch",
  "product_demo",
];
const LINK_PLACEMENTS: readonly LinkPlacement[] = ["caption", "bio", "first_comment", "none"];

function briefTypeOf(value: string): BriefType {
  if (isOneOf(BRIEF_TYPES, value)) return value;
  throw mappingError(`brief type "${value}" is not shown by the desktop yet`);
}

function linkPlacementOf(value: string): LinkPlacement {
  if (isOneOf(LINK_PLACEMENTS, value)) return value;
  throw mappingError(`link placement "${value}" is not shown by the desktop yet`);
}

function sceneOf(engine: EngineSceneSpec): SceneSpec {
  return {
    teaches: engine.teaches,
    frame: engine.frame,
    motion: engine.motion,
    words: engine.words,
    sound: engine.sound,
  };
}

function explainerOf(engine: EngineExplainerBeats): ExplainerBeats {
  return {
    question: sceneOf(engine.question),
    model: sceneOf(engine.model),
    proof: sceneOf(engine.proof),
    turn: sceneOf(engine.turn),
    payoff: sceneOf(engine.payoff),
  };
}

/** One stored typed brief as the desktop's TypedBrief. An explainer with no beats is a mapping error. */
export function typedBriefOf(engine: EngineTypedBrief): TypedBrief {
  const briefType = briefTypeOf(engine.brief_type);
  if (briefType === "explainer" && engine.explainer === null) {
    throw mappingError(`typed brief ${engine.brief_id} is an explainer with no beats`);
  }
  return {
    scopeId: accountKey(engine.scope),
    briefId: engine.brief_id,
    briefType,
    hook: engine.hook,
    body: engine.body,
    explainer: engine.explainer === null ? null : explainerOf(engine.explainer),
    demoSteps: engine.demo_steps.map((step) => ({ say: step.say })),
    revision: countOf(engine.revision, "typed brief revision"),
    createdAtUtc: engine.created_at,
    updatedAtUtc: engine.updated_at,
  };
}

/** One platform variant as the desktop's BriefVariant. The link is kept as given; no slot is re-derived. */
export function briefVariantOf(engine: EngineBriefVariant): BriefVariant {
  return {
    scopeId: accountKey(engine.scope),
    variantId: engine.variant_id,
    briefId: engine.brief_id,
    briefRevision: countOf(engine.brief_revision, "brief revision"),
    platform: platformText(engine.platform, "brief variant"),
    ruleRevision: engine.rule_revision,
    linkPlacement: linkPlacementOf(engine.link_placement),
    caption: engine.caption,
    link: engine.link,
    createdAtUtc: engine.created_at,
  };
}

// ---- Analytics and manual metric imports (read only) ------------------------------------------
// A stored value is an exact decimal string and is kept as the engine wrote it. The state and the value must agree:
// a value state carries text, and the unavailable state carries none. Anything else is an engine_mapping error.

const ANALYTICS_SOURCES: readonly AnalyticsSource[] = ["api", "manual_export"];
const DECIMAL_TEXT = /^-?\d+(\.\d+)?$/;

interface StoredAnalyticsValue {
  state: AnalyticsMetricState;
  value: string | null;
}

function analyticsValueOf(state: string, value: string | null | undefined, metric: string): StoredAnalyticsValue {
  if (state === "unavailable") {
    if (value === null || value === undefined) return { state: "unavailable", value: null };
    throw mappingError(`metric ${metric} is unavailable but carries a value`);
  }
  if (state !== "value") throw mappingError(`analytics state "${state}" is not shown by the desktop yet`);
  if (typeof value === "string" && DECIMAL_TEXT.test(value)) return { state: "value", value };
  throw mappingError(`metric ${metric} has a value that is not an exact decimal`);
}

function analyticsSourceOf(value: string): AnalyticsSource {
  if (isOneOf(ANALYTICS_SOURCES, value)) return value;
  throw mappingError(`analytics source "${value}" is not shown by the desktop yet`);
}

/**
 * The published posts of one scope, from analytics.summary. The summary names no source: a metric with an
 * import id is a manual row, and one without is an API row.
 */
export function analyticsPostsOf(engine: EngineAnalyticsSummary): AnalyticsPost[] {
  const scopeId = accountKey(engine.scope);
  return engine.jobs.map(
    (job): AnalyticsPost => ({
      scopeId,
      jobId: job.job_id,
      platform: platformText(job.platform, "analytics post"),
      remotePostId: job.remote_post_id,
      publishedAtUtc: job.published_at,
      metrics: job.metrics.map((metric): AnalyticsSummaryMetric => {
        const stored = analyticsValueOf(metric.state, metric.value, metric.metric);
        const importId = metric.import_id ?? null;
        const source: AnalyticsSource = importId === null ? "api" : "manual_export";
        return {
          metric: metric.metric,
          state: stored.state,
          value: stored.value,
          observedAtUtc: metric.observed_at,
          window: metric.window,
          source,
          pullId: metric.pull_id,
          importId,
          postAgeSeconds: safeIntOf(metric.post_age_secs, "post age"),
        };
      }),
    }),
  );
}

/** One stored observation, from analytics.list. The scope is the one the read was made in. */
export function analyticsObservationOf(engine: EngineAnalyticsObservation, scopeId: string): AnalyticsObservation {
  const stored = analyticsValueOf(engine.state, engine.value, engine.metric);
  return {
    scopeId,
    jobId: engine.job_id,
    metric: engine.metric,
    state: stored.state,
    value: stored.value,
    observedAtUtc: engine.observed_at,
    window: engine.window,
    source: analyticsSourceOf(engine.source),
    pullId: engine.pull_id,
    importId: engine.import_id ?? null,
  };
}

/** One committed manual import. Only the manual_export source is shown; any other source is a mapping error. */
export function metricImportOf(engine: EngineMetricImportRecord): MetricImport {
  if (engine.source !== "manual_export") {
    throw mappingError(`metric import ${engine.import_id} has source "${engine.source}", not shown by the desktop yet`);
  }
  return {
    scopeId: accountKey(engine.scope),
    importId: engine.import_id,
    platform: platformText(engine.platform, "metric import"),
    sourceNote: engine.source_note,
    source: "manual_export",
    createdAtUtc: engine.created_at,
    rowsWritten: countOf(engine.rows_written, "import rows"),
    observationsWritten: countOf(engine.observations_written, "import observations"),
  };
}

// ---- Store health (read only) -----------------------------------------------------------------
// The states and ages are the engine's, shown as given: nothing is recomputed here. A state the desktop does not
// show is an engine_mapping error. A schema the engine did not record is null, never zero.

const HEARTBEAT_STATES: readonly HeartbeatState[] = ["alive", "stale", "missing", "unreadable"];

function heartbeatStateOf(state: string): HeartbeatState {
  if (isOneOf(HEARTBEAT_STATES, state)) return state;
  throw mappingError(`heartbeat state "${state}" is not shown by the desktop yet`);
}

/** The heartbeat status as the desktop's HeartbeatStatus: one record, no per-source list. */
export function heartbeatStatusOf(engine: EngineHeartbeatStatus): HeartbeatStatus {
  const note = engine.record_state;
  return {
    nowUtc: engine.now,
    staleAfterSeconds: safeIntOf(engine.stale_after_secs, "stale threshold"),
    allAlive: engine.all_alive,
    state: heartbeatStateOf(engine.state),
    pid: engine.pid === null ? null : safeIntOf(engine.pid, "heartbeat pid"),
    updatedMs: engine.updated_ms === null ? null : safeIntOf(engine.updated_ms, "heartbeat update time"),
    ageSeconds: engine.age_secs === null ? null : safeIntOf(engine.age_secs, "heartbeat age"),
    recordState: note === null || note === undefined ? null : typeof note === "string" ? note : JSON.stringify(note),
    error: engine.error,
  };
}

/** One restore receipt as the desktop's RestoreReceipt. */
function restoreReceiptOf(engine: EngineRestoreReceipt): RestoreReceipt {
  const before = engine.schema_before ?? null;
  return {
    receiptId: engine.receipt_id,
    restoredAtUtc: engine.restored_at,
    manifestDigest: engine.manifest_digest,
    schemaBefore: before === null ? null : safeIntOf(before, "schema before"),
    schemaAfter: safeIntOf(engine.schema_after, "schema after"),
    previousStore: engine.previous_store ?? null,
  };
}

/** The restore receipts as the desktop's RestoreReceiptList, newest first as the engine sends them. */
export function restoreReceiptListOf(engine: EngineRestoreReceiptList): RestoreReceiptList {
  return {
    receipts: engine.receipts.map((receipt) => restoreReceiptOf(receipt)),
    unreadable: engine.unreadable,
  };
}

// ---- Webhook inbox and publish pacing (read only) -----------------------------------------------
// A row the desktop cannot read is kept as an UnreadableRow with its reason and its raw JSON. No row is dropped,
// and an unknown kind, state, reason or publish state is never guessed at. Event text is untrusted: it is copied
// as given and shown as plain text by the screens.

const INBOX_KINDS: readonly InboxKind[] = ["comment", "message", "mention", "other"];
const INBOX_STATES: readonly InboxState[] = ["open", "handled", "ignored"];
const PACING_REASONS: readonly PacingReason[] = [
  "not_due",
  "outcome_unknown_on_account",
  "account_not_ready",
  "account_paused",
  "rate_limited_until",
  "daily_cap",
  "account_gap",
];

/** A value as pretty printed JSON text. Used for raw rows and raw event items. */
export function rawTextOf(value: unknown): string {
  const text: string | undefined = JSON.stringify(value, null, 2);
  return text ?? String(value);
}

/** Maps one row. A row whose mapping throws is kept unread, with the reason and its raw JSON. It is never dropped. */
export function rowOf<T>(raw: unknown, map: () => T): T | UnreadableRow {
  try {
    return map();
  } catch (error) {
    return { readable: false, reason: error instanceof Error ? error.message : String(error), rawJson: rawTextOf(raw) };
  }
}

/** One inbox event. An unknown kind or state is a mapping error, so the row is kept unread (see rowOf). */
export function inboxEventOf(engine: EngineInboxEvent): InboxEvent {
  if (!isOneOf(INBOX_KINDS, engine.kind)) {
    throw mappingError(`inbox kind "${engine.kind}" is not shown by the desktop yet`);
  }
  if (!isOneOf(INBOX_STATES, engine.state)) {
    throw mappingError(`inbox state "${engine.state}" is not shown by the desktop yet`);
  }
  return {
    readable: true,
    eventId: engine.event_id,
    scopeId: engine.scope === null ? null : accountKey(engine.scope),
    unmatched: engine.unmatched_account,
    unmatchedReason: engine.unmatched_reason,
    platform: engine.platform,
    kind: engine.kind,
    recipientId: engine.recipient_id,
    platformEventId: engine.platform_event_id,
    senderId: engine.sender_id,
    text: engine.text,
    eventTimeAsGiven: engine.event_time,
    oversize: engine.oversize,
    receivedAtUtc: engine.received_at,
    state: engine.state,
    stateBy: engine.state_by,
    stateAtUtc: engine.state_at,
    stateNote: engine.state_note,
  };
}

/** The raw item JSON of one event, shown as text. */
export function inboxRawOf(engine: EngineInboxGet): InboxRaw {
  return { eventId: engine.event.event_id, rawJson: rawTextOf(engine.raw) };
}

function inboxStatsGroupOf(engine: EngineInboxStatsGroup): InboxStatsGroup {
  return {
    scopeId: engine.scope === null ? null : accountKey(engine.scope),
    kind: engine.kind,
    state: engine.state,
    count: countOf(engine.count, "inbox count"),
  };
}

/** The inbox counts as the engine stores them. Each group is kept, even one whose count cannot be read. */
export function inboxStatsOf(engine: EngineInboxStats): InboxStats {
  return {
    totalEvents: countOf(engine.total_events, "inbox events"),
    unmatchedEvents: countOf(engine.unmatched_events, "unmatched events"),
    deliveriesParsed: countOf(engine.deliveries_parsed, "deliveries parsed"),
    deliveriesUnparseable: countOf(engine.deliveries_unparseable, "deliveries unparseable"),
    refusedTotal: countOf(engine.refused_total, "refused deliveries"),
    groups: engine.groups.map((group) => rowOf(group, () => inboxStatsGroupOf(group))),
  };
}

function pacingReasonOf(value: string): PacingReason {
  if (isOneOf(PACING_REASONS, value)) return value;
  throw mappingError(`publish pacing reason "${value}" is not shown by the desktop yet`);
}

/** A job publish.next names. Its state must be one the desktop shows. */
function pacingJobOf(engine: EnginePublishNextJob): PacingJob {
  if (!isOneOf(PUBLISH_STATES, engine.state)) {
    throw mappingError(`publish state "${engine.state}" is not shown by the desktop yet`);
  }
  return {
    readable: true,
    jobId: engine.job_id,
    scopeId: accountKey(engine.scope),
    platform: engine.platform,
    remoteAccount: engine.remote_account,
    entryId: engine.entry_id,
    state: engine.state,
    phase: engine.phase,
    scheduledAtUtc: engine.scheduled_at,
  };
}

function pacingBlockedOf(engine: EnginePublishNextBlocked): PacingBlockedJob {
  return {
    ...pacingJobOf(engine),
    reason: pacingReasonOf(engine.reason),
    reasons: engine.reasons.map((reason) => pacingReasonOf(reason)),
    untilUtc: engine.until ?? null,
    detail: engine.detail,
    behindJobId: engine.behind ?? null,
  };
}

/** publish.next as the desktop's view. Every row is kept: a row it cannot read is an UnreadableRow. */
export function publishNextOf(engine: EnginePublishNext): PublishNextView {
  return {
    nowUtc: engine.now,
    ready: engine.ready.map((job) => rowOf(job, () => pacingJobOf(job))),
    readyTotal: countOf(engine.ready_total, "ready total"),
    blocked: engine.blocked.map((job) => rowOf(job, () => pacingBlockedOf(job))),
    inProgress: engine.in_progress.map((job) => rowOf(job, () => pacingJobOf(job))),
  };
}

/** One account's pacing as stored. The limits are the stored ones, or the host app defaults when opted_in is false. */
export function publishQuotaOf(engine: EnginePublishQuota): PublishQuota {
  return {
    scopeId: accountKey(engine.scope),
    platform: engine.platform,
    remoteAccount: engine.remote_account,
    optedIn: engine.opted_in,
    minGapSecs: countOf(engine.min_gap_secs, "minimum gap"),
    maxPerDay: countOf(engine.max_per_day, "maximum per day"),
    defaultMinGapSecs: countOf(engine.default_min_gap_secs, "default gap"),
    defaultMaxPerDay: countOf(engine.default_max_per_day, "default per day"),
    pausedUntilUtc: engine.paused_until ?? null,
    lastCreateAtUtc: engine.last_create_at ?? null,
    createsToday: countOf(engine.creates_today, "creates today"),
    dayResetsAtUtc: engine.day_resets_at,
    rateLimitedAtUtc: engine.rate_limited_at ?? null,
    rateLimitedUntilUtc: engine.rate_limited_until ?? null,
  };
}
