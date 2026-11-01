// Operator data model. These types are the contract the screens render. The daemon owns every
// value here; the desktop only displays it and sends intents back through DataSource.

/**
 * The account platforms the desktop shows: exactly the engine's PLATFORMS (crates/protocol/src/accounts.rs:22-33).
 * The list and its order live in SOCIAL_PLATFORMS (platforms.ts). Draft-only ids are not in this union.
 */
export type SocialPlatform =
  | "instagram"
  | "facebook"
  | "threads"
  | "youtube"
  | "tiktok"
  | "x"
  | "linkedin"
  | "pinterest"
  | "bluesky"
  | "mastodon";

export type TokenHealth =
  | { status: "valid"; expiresOn: string }
  | { status: "expiring"; expiresOn: string }
  | { status: "refresh_failed" }
  | { status: "revoked" };

/** Three states that are never merged into one "live" flag (plan section 4, P1). */
export type Entitlement = "sandbox_mechanics" | "review_submitted" | "audience_visible";

export interface PauseRecord {
  by: "desktop" | "phone";
  atUtc: string;
  reason: string;
}

export interface AccountReadiness {
  token: TokenHealth;
  /** ISO date by which the operator must re-authorise. Null when the provider refreshes tokens. */
  reauthNeededBy: string | null;
  entitlement: Entitlement;
  /** UTC instant the quota cooldown ends, or null when none is active. */
  quotaCooldownUntil: string | null;
  quotaCooldownSource: "numeric_retry_after" | "dated_retry_after" | null;
  paused: PauseRecord | null;
}

export interface Venture {
  id: string;
  name: string;
}

export interface Account {
  id: string;
  venture: Venture;
  platform: SocialPlatform;
  handle: string;
  displayName: string;
  /** Last four characters of the platform account id, shown so same-handle look-alikes differ. */
  idSuffix: string;
  /** IANA zone the account publishes in. */
  timeZone: string;
  /** Stable colour band index (0 to 5). Always shown next to a text label. */
  band: number;
  readiness: AccountReadiness;
}

export type MediaKind = "video" | "image";

export interface MediaItem {
  /** Full digest, for example "blake3:9c1e4a7f...". The UI shows a short form. */
  hash: string;
  kind: MediaKind;
  durationSeconds: number | null;
  /** A whole number of milliseconds, or null when the engine holds none. Absent on mock records. */
  durationMs?: number | null;
  dimensions: string | null;
  status: "ready" | "uploading" | "missing";
  uploadPercent: number | null;
}

/** "remake" is the format of a calendar entry the optimise module creates when a suggestion is accepted. */
export type JobFormat = "reel" | "carousel" | "photo" | "short" | "video" | "text_post" | "remake";

export type JobState =
  | "draft"
  | "media_pending"
  | "ready"
  | "scheduled"
  | "publishing"
  | "published"
  | "outcome_unknown"
  | "failed"
  | "late"
  | "paused";

export type RejectReason = "wrong_content" | "wrong_account" | "timing" | "media_issue" | "other";

export type ApprovalInvalidation = "caption_changed" | "media_changed" | "policy_changed";

export type Approval =
  | { status: "none" }
  | { status: "pending" }
  | { status: "approved"; approvedRevision: number; policyRevision: number; approvedAtUtc: string }
  | { status: "rejected"; reason: RejectReason; note: string | null }
  | {
      status: "invalidated";
      approvedRevision: number;
      policyRevision: number;
      cause: ApprovalInvalidation;
    };

export type DstStatus =
  | { kind: "none" }
  | { kind: "ambiguous_fires_once"; note: string }
  | { kind: "nonexistent_moved"; note: string };

export interface Lateness {
  /** UTC instant by which media had to be ready. */
  mediaNotReadyBy: string;
  policy: "publish_next_slot" | "held_for_review";
}

export interface ScheduleConflict {
  withJobId: string;
  minimumSpacingMinutes: number;
}

export interface PublishJob {
  id: string;
  accountId: string;
  briefId: string;
  format: JobFormat;
  /** Job compare-and-swap revision. Every content, schedule or decision change bumps it. */
  revision: number;
  caption: string;
  /** Short digest of the exact caption text, so the preview can be checked against the daemon. */
  captionDigest: string;
  firstComment: string | null;
  settings: string[];
  mediaItems: MediaItem[];
  scheduledAtUtc: string;
  /** Account zone at schedule time. Copied onto the job so the calendar reads one field. */
  timeZone: string;
  dst: DstStatus;
  state: JobState;
  approval: Approval;
  /** Current platform-rules and policy revision the daemon applies to this job. Null when the engine does not state one. */
  policyRevision: number | null;
  lateness: Lateness | null;
  conflict: ScheduleConflict | null;
  /**
   * The engine's publish job for this entry at its current revision. Absent when the engine does
   * not report one (the desktop role cannot read publish jobs; see "Publish jobs" in the design doc).
   * Null when there is no job for this revision.
   */
  publish?: PublishProgress | null;
}

// ---- Publish progress (engine publish core, read-only here) ----
// The daemon publishes; the desktop only displays what the engine's publish job says.

export type PublishProgressState =
  | "queued"
  | "running"
  | "waiting"
  | "published"
  | "failed"
  | "outcome_unknown"
  | "cancelled";

export interface PublishProgress {
  state: PublishProgressState;
  /** The platform's link to the post. Shown as text with a Copy link button; never opened. */
  permalink: string | null;
  /** Set only while waiting: the UTC instant the daemon next calls the platform. */
  resumeAtUtc: string | null;
  /** The engine's typed error code for the latest failure, or null. */
  errorCode: string | null;
  errorMessage: string | null;
}

// ---- Publish jobs, read only (the Publish jobs screen) ----
// The daemon's durable publish job for one approved calendar entry. The desktop lists and reads it; it never
// queues, steps, runs, reconciles, cancels or resolves a job. A job holds no token.

/** The two decisions a person can record for an outcome_unknown job. */
export type PublishResolutionDecision = "posted" | "not_posted";

/** What a person recorded for an outcome_unknown job. Null on every other job. */
export interface PublishJobResolution {
  decision: PublishResolutionDecision;
  /** The human author as stored, for example "adrian". */
  resolvedBy: string;
  resolvedAtUtc: string;
  /** What the person checked on the platform. */
  note: string;
  /** True when the remote post id was typed by a person, not returned by the platform. */
  remoteIdEnteredByHand: boolean;
}

/** One publish job of a channel, as publish.list returns it, with the scope it belongs to. */
export interface PublishJobRecord {
  scopeId: string;
  jobId: string;
  entryId: string;
  entryRevision: number;
  platform: string;
  state: PublishProgressState;
  /** The engine's phase name, as stored (for example create, publish or done). */
  phase: string;
  remotePostId: string | null;
  /** The platform's link to the post, as text. Never opened. */
  permalink: string | null;
  attempts: number;
  /** UTC instant the daemon next calls the platform. Set only while waiting. */
  nextAttemptAtUtc: string | null;
  errorCode: string | null;
  errorMessage: string | null;
  resolution: PublishJobResolution | null;
  createdAtUtc: string;
  updatedAtUtc: string;
}

// ---- Webhook inbox, read only (the Inbox screen) ----
// Every text field of an event is untrusted, written by a stranger. The desktop shows it exactly as received,
// as plain text only, and sends no reply. A row the desktop cannot read is kept, never dropped.

/** A row the desktop could not read. It is shown with its reason and its raw JSON. */
export interface UnreadableRow {
  readable: false;
  /** The desktop's plain reason, for example a value it does not show yet. */
  reason: string;
  /** The row as the engine sent it, pretty printed. May contain untrusted event text. */
  rawJson: string;
}

export type InboxKind = "comment" | "message" | "mention" | "other";
export type InboxState = "open" | "handled" | "ignored";

/** One inbox event, as inbox.list returns it. Every text field is untrusted and shown as received. */
export interface InboxEvent {
  readable: true;
  eventId: string;
  /** The channel account the event is bound to, or null when no account matched. */
  scopeId: string | null;
  /** True when no stored account matched the recipient id, or more than one did. */
  unmatched: boolean;
  /** `no_account` or `ambiguous_account` when unmatched; null otherwise. */
  unmatchedReason: string | null;
  /** The account platform as stored. Shown through the platform name helper. */
  platform: string;
  kind: InboxKind;
  /** The recipient account id the platform named. Untrusted. */
  recipientId: string | null;
  /** The platform's own id for the event. Untrusted. */
  platformEventId: string | null;
  /** The sender's platform id, as given. Untrusted. */
  senderId: string | null;
  /** The text, as received. Untrusted. */
  text: string | null;
  /** The event time the platform gave, as received and not parsed. Untrusted. */
  eventTimeAsGiven: string | null;
  /** True when a field was cut to its bound. The raw JSON keeps the whole item. */
  oversize: boolean;
  /** The instant the daemon stored the event (UTC). */
  receivedAtUtc: string;
  state: InboxState;
  /** Who set the state (a person's name), or null while open. */
  stateBy: string | null;
  stateAtUtc: string | null;
  /** The note recorded with the state. Empty when none. */
  stateNote: string;
}

export type InboxRow = InboxEvent | UnreadableRow;

/** One page of events, newest first. */
export interface InboxListing {
  rows: InboxRow[];
  /** True when older events exist beyond this page. This screen loads the newest page only. */
  moreAvailable: boolean;
}

/** The raw item JSON of one event, as received, pretty printed. Untrusted text. */
export interface InboxRaw {
  eventId: string;
  rawJson: string;
}

/** One count of the stored events: a scope (null for unmatched), a kind and a state, as stored. */
export interface InboxStatsGroup {
  scopeId: string | null;
  kind: string;
  state: string;
  count: number;
}

export type InboxStatsRow = InboxStatsGroup | UnreadableRow;

/** The inbox counts. Whole-store totals and refusals are not kept per channel by the daemon. */
export interface InboxStats {
  totalEvents: number;
  unmatchedEvents: number;
  deliveriesParsed: number;
  deliveriesUnparseable: number;
  /** Deliveries refused for a bad signature, whole store. */
  refusedTotal: number;
  groups: InboxStatsRow[];
}

// ---- Publish pacing, read only (the Pacing screen) ----
// The daemon decides what may run. The desktop shows publish.next and publish.quota_get as stored. The defaults
// are the host app own caution, not platform limits. A person sets limits on the server.

/** Why a job may not run now. The words are in the Pacing screen's labels. */
export type PacingReason =
  | "not_due"
  | "outcome_unknown_on_account"
  | "account_not_ready"
  | "account_paused"
  | "rate_limited_until"
  | "daily_cap"
  | "account_gap";

/** A publish job named by publish.next. */
export interface PacingJob {
  readable: true;
  jobId: string;
  /** The channel account of the job (its scope id). */
  scopeId: string;
  platform: string;
  /** The platform's own account id for this job. */
  remoteAccount: string;
  entryId: string;
  state: PublishProgressState;
  phase: string;
  /** When the job is due: the next attempt for a waiting job, else its enqueue instant. */
  scheduledAtUtc: string;
}

/** A job that may not run now, with the first reason, every reason, and the earliest instant it could run. */
export interface PacingBlockedJob extends PacingJob {
  reason: PacingReason;
  reasons: PacingReason[];
  /** The earliest instant every reason has passed. Null when none is given. */
  untilUtc: string | null;
  /** The daemon's plain detail for the job. */
  detail: string;
  /** The job selected first on this account, when this job waits behind it. */
  behindJobId: string | null;
}

/** What publish.next says at one instant. */
export interface PublishNextView {
  /** The daemon's instant the decision was made at (UTC). */
  nowUtc: string;
  /** Jobs that may run now, at most one per account. */
  ready: (PacingJob | UnreadableRow)[];
  /** How many jobs could run now, as the daemon counts them. */
  readyTotal: number;
  /** Jobs that may not run now. */
  blocked: (PacingBlockedJob | UnreadableRow)[];
  /** Jobs already past their create. Pacing does not hold these back. */
  inProgress: (PacingJob | UnreadableRow)[];
}

/** One account's pacing, as publish.quota_get returns it. */
export interface PublishQuota {
  /** The channel account's scope id. */
  scopeId: string;
  platform: string;
  remoteAccount: string;
  /** True when a person stored the limits. False means the host app defaults apply. */
  optedIn: boolean;
  minGapSecs: number;
  maxPerDay: number;
  /** the host app own default gap. Not a platform limit. */
  defaultMinGapSecs: number;
  /** the host app own default count per UTC day. Not a platform limit. */
  defaultMaxPerDay: number;
  pausedUntilUtc: string | null;
  lastCreateAtUtc: string | null;
  /** Creates counted in the UTC day of the read. */
  createsToday: number;
  /** The next UTC midnight: when the count restarts. */
  dayResetsAtUtc: string;
  rateLimitedAtUtc: string | null;
  rateLimitedUntilUtc: string | null;
}

export interface DateRange {
  /** Inclusive UTC instant. */
  fromUtc: string;
  /** Exclusive UTC instant. */
  toUtc: string;
}

export type DataErrorCode =
  | "revision_conflict"
  | "account_mismatch"
  | "not_approvable"
  | "reason_required"
  | "invalid_input"
  | "not_found";

export interface DataError {
  code: DataErrorCode;
  message: string;
  /** Set on revision_conflict: the revision the daemon holds now. */
  currentRevision?: number;
}

export type Result<T> = { ok: true; value: T } | { ok: false; error: DataError };

/** Every approve, reject and move carries the revision and account the operator saw. */
export interface RejectDecision {
  revision: number;
  accountId: string;
  code: RejectReason;
  note: string;
}

// ---- Failures, reconcile and digest (design doc D4) ----

export type FailureKind = "dead_letter" | "outcome_unknown" | "alert";
export type AlertKind = "disk_full" | "credentials_expired" | "quota_cooldown";
export type FailureResolution =
  | "published_confirmed"
  | "cancelled_not_published"
  | "accepted_as_lost"
  | "acknowledged";

export type ReconcileResult =
  | { outcome: "found_on_platform"; remoteId: string; permalink: string; checkedAtUtc: string }
  | { outcome: "not_found"; checkedAtUtc: string };

export interface FailureItem {
  id: string;
  kind: FailureKind;
  /** Only for kind "alert". */
  alertKind: AlertKind | null;
  /** Null for daemon-wide alerts such as disk space. */
  accountId: string | null;
  jobId: string | null;
  /** One plain sentence: what happened. */
  summary: string;
  /** One plain sentence: the single next action. */
  nextAction: string;
  atUtc: string;
  attempts: number | null;
  status: "open" | "resolved";
  reconcile: ReconcileResult | null;
  resolution: FailureResolution | null;
  resolutionNote: string | null;
}

export interface Digest {
  windowStartUtc: string;
  windowEndUtc: string;
  /** Plain sentences, built from the same counts the screen shows. */
  sentences: string[];
  counts: {
    published: number;
    failedDeadLetter: number;
    outcomeUnknown: number;
    openAlerts: number;
    pausedAccounts: number;
    quotaCooldowns: number;
    pendingApprovals: number;
    openThreads: number;
  };
}

// ---- Threads (design doc D5) ----

export type ThreadKind = "comment" | "dm";
export type ThreadStatus = "open" | "snoozed" | "resolved";

export interface ThreadMessage {
  id: string;
  /** Invented handle. Never a real account. */
  author: string;
  fromOperator: boolean;
  sentAtUtc: string;
  text: string;
}

export interface AutoReplyPlan {
  ruleName: string;
  /** The keyword found in the latest inbound message. */
  matchedKeyword: string;
  /** The reply that would be sent. Shown only; never sent by this screen. */
  draftReply: string;
  /** "matched_not_sent" is a preview. "unavailable" carries the reason. */
  state: "matched_not_sent" | "unavailable";
  unavailableReason: string | null;
}

export interface ReplyWindow {
  closesUtc: string;
}

export interface ThreadItem {
  id: string;
  accountId: string;
  postTitle: string;
  postPublishedUtc: string;
  kind: ThreadKind;
  messages: ThreadMessage[];
  /** Null when the platform gives no reply window for this kind of thread. */
  replyWindow: ReplyWindow | null;
  autoReply: AutoReplyPlan | null;
  status: ThreadStatus;
  snoozedUntilUtc: string | null;
}

export type ModerationAction = "hide_comment" | "delete_comment" | "block_author";

export interface ModerationCapability {
  platform: SocialPlatform;
  action: ModerationAction;
  supported: boolean;
  /** Shown whenever supported is false. Says why, in words. */
  reason: string;
}

// ---- Manual drafts (text the operator posts by hand) ----
// the host app never posts a draft. The operator copies the body, posts it on the platform, and records
// the link. The engine holds the draft and checks it; the desktop only displays and sends intents.

/** Draft kinds. A comment or a reply needs a target URL before it can be approved. */
export type DraftKind = "post" | "comment" | "reply";

/** Draft states. Only the operator's recorded link moves a draft to posted_by_operator. */
export type DraftState = "drafted" | "approved" | "posted_by_operator" | "skipped" | "stale";

export interface ManualDraft {
  id: string;
  /** Venture, channel and account the draft belongs to, as text. */
  channelLabel: string;
  /** Platform name as stored: a platform on the account list, or "reddit". */
  platform: string;
  kind: DraftKind;
  /** Community or thread URL. Shown as text and never opened by the screen. Null for a new post. */
  target: string | null;
  title: string | null;
  body: string;
  /** Unicode scalar values in the body. */
  bodyChars: number;
  disclosureRequired: boolean;
  state: DraftState;
  /** Compare-and-swap revision. Every action sends the revision the operator saw. */
  revision: number;
  /** The link the operator recorded. Set only when posted. */
  postedUrl: string | null;
  postedAtUtc: string | null;
  skipReason: string | null;
  sourceNote: string;
  updatedAtUtc: string;
}

// ---- Suggestions (optimise) and spend (read-only) ----
// Both are read per channel scope. A scope id is the venture, channel and account joined with "/" (each
// part URI-encoded), the same key the engine's commands take. Money is an integer count of micro-units:
// 1,000,000 micro-units is 1 currency unit. No amount is ever a float.

/** One stored channel the desktop can select for suggestions and spend. */
export interface OperatorScope {
  id: string;
  /** Venture, channel and account as text. */
  label: string;
}

export type OptimiseState = "proposed" | "accepted" | "dismissed";

/** A calendar change the optimise module proposes from stored analytics. The desktop only decides it. */
export interface OptimiseProposal {
  id: string;
  scopeId: string;
  /** The rule that produced it. "remake" is the only rule. */
  kind: "remake";
  state: OptimiseState;
  /** Compare-and-swap revision. Accept and dismiss send the revision the operator saw. */
  revision: number;
  /** The published post the suggestion is about. */
  sourceJobId: string;
  platform: string;
  metric: string;
  window: string;
  observedAtUtc: string;
  /** A whole number as exact text. Never converted to a float, so it is shown as stored. */
  value: string;
  /** The top-decile cut. A post wins when its value is at least this. Exact text, like value. */
  threshold: string;
  sampleSize: number;
  decileSize: number;
  dueAtUtc: string;
  /** The published posts compared. */
  lineageJobIds: string[];
  /** The analytics pulls whose observations were compared. */
  lineagePullIds: string[];
  decisionNote: string | null;
  decidedAtUtc: string | null;
  /** The draft calendar entry an accepted suggestion created. Null otherwise. */
  calendarEntryId: string | null;
  createdAtUtc: string;
  updatedAtUtc: string;
}

/** The month's allowance for one channel. remaining = limit - reserved - committed. */
export interface SpendAllowance {
  /** "YYYY-MM". */
  month: string;
  currency: string;
  limitMicro: number;
  reservedMicro: number;
  committedMicro: number;
  remainingMicro: number;
  revision: number;
  updatedAtUtc: string;
}

export interface SpendRouteTotal {
  routeId: string;
  countReserved: number;
  countCommitted: number;
  countReleased: number;
  countExpired: number;
  openMicro: number;
  forecastMicro: number;
  measuredMicro: number;
  varianceMicro: number;
  overReservations: number;
}

export interface SpendReport {
  month: string;
  currency: string;
  limitMicro: number;
  reservedMicro: number;
  committedMicro: number;
  remainingMicro: number;
  /** True when the allowance totals equal the reservation and commit totals. */
  reconciles: boolean;
  routes: SpendRouteTotal[];
}

/** The latest version of one price route, with its qualification at the data source's clock. */
export interface SpendPrice {
  routeId: string;
  version: number;
  provider: string;
  modelSlug: string;
  unit: string;
  amountMicro: number;
  currency: string;
  sourceUrl: string | null;
  sourcePathLine: string | null;
  fetchedAtUtc: string;
  verifiedBy: string | null;
  qualified: boolean;
  /** Set only when not qualified: the engine's reason, as text. */
  reason: string | null;
}

// ---- Campaigns, tracked links and hook tests (read-only here) ----
// Both are read per channel scope, the same scope id as suggestions and spend. Campaigns, links and
// hook test sets are made on the daemon host or with the CLI. Visit counts are plain integer counts the
// engine holds: there are no rates and no ranking, and the desktop records no visit.

/** One campaign in a channel. `utmCampaign` is the slug written into utm_campaign on links cut for it. */
export interface Campaign {
  id: string;
  scopeId: string;
  name: string;
  utmCampaign: string;
  /** Compare-and-swap revision. The desktop never writes it. */
  revision: number;
  createdAtUtc: string;
  updatedAtUtc: string;
}

/** One tracked link. Its final URL, with the UTM parts, is fixed when the link is created. */
export interface TrackedLink {
  id: string;
  scopeId: string;
  campaignId: string;
  variantId: string;
  platform: string;
  /** The https destination as given. Shown as text and never opened by the screen. */
  destination: string;
  /** The calendar entry the link was cut for, or null when it was not attached to one. */
  calendarEntryId: string | null;
  utmSource: string;
  utmMedium: string;
  utmCampaign: string;
  utmContent: string;
  finalUrl: string;
  createdAtUtc: string;
}

/** One link's visit count in an attribution summary. */
export interface LinkAttributionRow {
  linkId: string;
  variantId: string;
  platform: string;
  calendarEntryId: string | null;
  /** A whole, non-negative count. Zero is a normal state. */
  visits: number;
}

/** The visit counts of one campaign's links. `totalVisits` is the sum of the rows. */
export interface CampaignAttribution {
  campaignId: string;
  totalVisits: number;
  links: LinkAttributionRow[];
}

/** One tracked link of a channel, with the campaign it was cut for and its plain visit count. */
export interface ChannelLink {
  link: TrackedLink;
  /** The name of the campaign the link belongs to. */
  campaignName: string;
  /** Visits recorded against the link, a whole count. Null when the count was not read with the link. */
  visits: number | null;
}

/** A set is open until one read decides it. A decided set never changes. */
export type HookTestState = "open" | "decided";

/** The winner is promoted; every other opening is archived. Null on every opening of an open set. */
export type HookTestRole = "promoted" | "archived";

/** One opening of a hook test. The number fields are set only once a read has compared the opening. */
export interface HookTestOpening {
  /** 1-based position of the opening in the body. */
  index: number;
  text: string;
  /** The publish job that carries this opening, or null when not bound yet. */
  jobId: string | null;
  boundAtUtc: string | null;
  role: HookTestRole | null;
  /** The metric value as exact integer text. Null when the opening has not been read. */
  value: string | null;
  /** The instant of the observation compared. */
  observedAtUtc: string | null;
  /** The analytics pull that stored the observation compared. */
  pullId: string | null;
  /** The job's publish instant the 24-hour window counted from. */
  publishedAtUtc: string | null;
}

/** A variant set: one body with 2 to 5 openings, compared on one metric. */
export interface HookTestSet {
  id: string;
  scopeId: string;
  briefRef: string;
  body: string;
  metric: string;
  window: string;
  state: HookTestState;
  /** Compare-and-swap revision. The desktop never writes it. */
  revision: number;
  /** The 1-based index of the promoted opening once decided, otherwise null. */
  winnerIndex: number | null;
  decidedAtUtc: string | null;
  createdAtUtc: string;
  updatedAtUtc: string;
  openings: HookTestOpening[];
}

// ---- Research digests and content library (read only here) ----
// Both are read per channel scope, the same scope id as suggestions. A digest is supplied to the engine
// by its caller; nothing here fetches a source. An applied digest entry became one draft calendar entry
// with a placeholder caption, which cannot be approved until a person writes a real caption. The desktop
// never puts, plans, applies, withdraws, edits or records anything in either module.

/** One finding in a digest. `source` is an http(s) url or a path:line reference: text, never opened. */
export interface DigestEntry {
  id: string;
  topic: string;
  finding: string;
  source: string;
  /** YYYY-MM-DD: the day the finding was read. */
  fetchedOn: string;
  /** The platform the finding suggests, as stored. */
  platform: string;
  /** The format the finding suggests, as stored. */
  format: string;
}

/** One research digest in a channel. A stored digest is immutable. */
export interface ResearchDigest {
  id: string;
  scopeId: string;
  /** YYYY-MM-DD. */
  digestDate: string;
  entries: DigestEntry[];
  createdAtUtc: string;
}

/** One digest entry that became a draft calendar entry. */
export interface DigestApplication {
  digestId: string;
  digestEntryId: string;
  briefId: string;
  /** The draft calendar entry the application created. */
  calendarEntryId: string;
  appliedAtUtc: string;
}

export type ContentState = "active" | "withdrawn" | "expired";
export type ContentMediaKind = "image" | "video";
export type ContentVariantKind = "caption" | "cta";
export type ContentVerdict = "insufficient_evidence" | "evidence_sufficient";

export interface ContentMediaItem {
  hash: string;
  kind: ContentMediaKind;
  /** A whole number of milliseconds, or null when the engine holds none. */
  durationMs: number | null;
}

/** One reusable media record. `state` is derived from the stored state and the clock; expired is never stored. */
export interface ContentRecord {
  id: string;
  scopeId: string;
  title: string;
  media: ContentMediaItem[];
  baseCaption: string;
  /** Recorded for the operator. Nothing acts on it yet. */
  evergreen: boolean;
  /** Whole days between two occurrences on one platform. */
  cooldownDays: number;
  /** Last UTC instant the record may run, or null when it never expires. */
  expiresAtUtc: string | null;
  state: ContentState;
  /** Set only when withdrawn. */
  withdrawalReason: string | null;
  revision: number;
  createdAtUtc: string;
  updatedAtUtc: string;
}

/** One caption or call-to-action variant of a record. */
export interface ContentVariant {
  id: string;
  scopeId: string;
  contentId: string;
  kind: ContentVariantKind;
  text: string;
  createdAtUtc: string;
}

/** A record with its variants, as the engine returns them for one record. */
export interface ContentDetail {
  record: ContentRecord;
  variants: ContentVariant[];
}

/** Plain occurrence counts for one variant. Only published occurrences count toward the minimum. */
export interface ContentVariantCount {
  variantId: string;
  text: string;
  planned: number;
  published: number;
  cancelled: number;
  meetsMinimum: boolean;
  /** Published occurrences whose primary metric value was compared. Zero unless a metric is declared. */
  compared?: number;
  /** Exact sum of the compared values, as decimal text. "0" unless a metric is declared. */
  total?: string;
}

/** The engine's comparison of one record's variants. The engine names no winner in this slice. */
export interface ContentExperimentSummary {
  scopeId: string;
  contentId: string;
  minOccurrences: number;
  verdict: ContentVerdict;
  /** The engine's own sentence listing why the verdict holds. Shown as stored. */
  reason: string;
  /** The primary metric declared for the record, or null. */
  primaryMetric?: string | null;
  captionWinner?: string | null;
  ctaWinner?: string | null;
  caption: ContentVariantCount[];
  cta: ContentVariantCount[];
}

// ---- Text gate, outgoing briefs, deliveries and dispatch attempts (read only here) ----
// The desktop checks a script and reads what is stored. It never puts a gate policy, never records a check,
// and never issues, accepts or runs a brief. A check from this screen sends no claim annotations and asks
// the engine not to record, so it stores nothing.

/** Which style rules treat an inconclusive or unverified finding as a block. Mirrors the engine's flags. */
export interface GateRequiredRules {
  bannedPhrase: boolean;
  dashDensity: boolean;
  semicolonDensity: boolean;
  sentenceLengthVariance: boolean;
  tricolonFrequency: boolean;
  stockOpener: boolean;
}

/** The latest revision of a channel's gate policy. Thresholds are exact: 1500 is written 1.5 on screen. */
export interface GatePolicy {
  scopeId: string;
  revision: number;
  createdAtUtc: string;
  bannedPhrases: string[];
  stockOpeners: string[];
  minWords: number;
  /** Dashes per 100 words, at most, in thousandths. */
  dashMaxPer100WordsMilli: number;
  /** Semicolons per 100 words, at most, in thousandths. */
  semicolonMaxPer100WordsMilli: number;
  /** Population variance of words per sentence, at least, in thousandths. */
  varianceMinMilli: number;
  varianceMinSentences: number;
  /** Tricolon sentences per 100 sentences, at most, in thousandths. */
  tricolonMaxPer100SentencesMilli: number;
  tricolonMinSentences: number;
  required: GateRequiredRules;
}

/** The overall result of a check: pass, hold (a person decides) or block. */
export type GateDisposition = "pass" | "hold" | "block";

/** One rule's verdict. The engine's reason text is kept exactly as written. */
export type GateVerdict = "pass" | "fail" | "inconclusive" | "unverified" | "hold" | "not_applicable";

export interface GateRuleFinding {
  ruleId: string;
  required: boolean;
  verdict: GateVerdict;
  /** True when this finding alone blocks the script. */
  blocks: boolean;
  reason: string;
}

/** A dry-run check. Nothing is stored. */
export interface GateCheck {
  subjectId: string;
  disposition: GateDisposition;
  policyRevision: number;
  /** fnv1a128 over the text bytes. For identification only. */
  textHash: string;
  rules: GateRuleFinding[];
  blockingRules: string[];
  proofIdsUsed: string[];
}

/** One stored verdict record of a subject, newest first in a list. The record's own JSON is not read here. */
export interface GateRecord {
  scopeId: string;
  recordSeq: number;
  subjectId: string;
  textHash: string;
  policyRevision: number;
  disposition: GateDisposition;
  checkedAtUtc: string;
}

/** Any JSON value, as the engine stores a document. */
export type JsonValue = string | number | boolean | null | JsonValue[] | { [key: string]: JsonValue };

/**
 * `issued` until an envelope is accepted for the brief; `partial` while some envelopes are stored but the engine's
 * latest result was partial (part of the request was skipped or refused); `delivered` once everything requested
 * was produced.
 */
export type BriefState = "issued" | "partial" | "delivered";

/** One outgoing content brief in a channel. */
export interface OutgoingBrief {
  scopeId: string;
  requestId: string;
  briefId: string;
  revision: number;
  state: BriefState;
  createdAtUtc: string;
  updatedAtUtc: string;
}

/** The fields of a brief document the desktop shows. A field the document lacks is null or empty, never invented. */
export interface BriefSummary {
  briefType: string | null;
  platforms: string[];
  deliverables: string[];
  hook: string | null;
  reviewMode: string | null;
  policyRevision: number | null;
  stageCount: number;
}

export interface OutgoingBriefDetail extends OutgoingBrief {
  summary: BriefSummary;
  /** The brief exactly as the engine stored it. */
  document: JsonValue;
}

/** One accepted delivery envelope and the request it answers. A brief holds at most one envelope per deliverable. */
export interface DeliveryEnvelope {
  scopeId: string;
  envelopeId: string;
  requestId: string;
  /** The engine's key for the deliverable this envelope is for. Empty when the envelope was stored without one. */
  deliverable: string;
  briefId: string;
  briefRevision: number;
  acceptedAtUtc: string;
  document: JsonValue;
}

/** The outcomes an attempt can end with. `running` is shown while the engine child is still alive. */
export type DispatchOutcome =
  | "running"
  | "delivered"
  | "partial"
  | "awaiting_approval"
  | "engine_failed"
  | "timed_out"
  | "delivery_invalid"
  | "engine_missing"
  | "interrupted";

/** One entry of the engine's `skipped` list, as the engine wrote it. A field the entry lacks is null. */
export interface DispatchSkipped {
  deliverable: string | null;
  /** For example `spend_authorisation_required`: a paid route the engine refused. */
  code: string | null;
  message: string | null;
}

/** One run of an editor engine against an outgoing brief. */
export interface DispatchAttempt {
  scopeId: string;
  attemptId: number;
  requestId: string;
  engine: string;
  startedAtUtc: string;
  /** Null while the run is still going. */
  finishedAtUtc: string | null;
  /** Null when the child was killed or never ran. */
  exitCode: number | null;
  outcome: DispatchOutcome;
  /** The capped tail of the engine's stderr. Null when none was kept. */
  stderrTail: string | null;
  /** The message the attempt recorded, or null. */
  message: string | null;
  /** The deliverable keys that produced an envelope in this attempt. Empty when none did. */
  producedDeliverables: string[];
  /** What the engine skipped or refused, verbatim. Empty when nothing was. */
  skipped: DispatchSkipped[];
}

// ---- Channel memory and proof bank (read only here) ----
// Both are read per channel scope, the same scope id as the other channel screens. The desktop never writes a
// memory revision, imports proofs or changes a proof's status. Only an approved proof can be cited by a script,
// and approving happens outside the desktop.

/** One stored revision of a channel's memory document, without the document. */
export interface ChannelMemoryRevision {
  scopeId: string;
  revision: number;
  /** The revision this one replaces. Null for the first revision. */
  supersedes: number | null;
  author: string;
  createdAtUtc: string;
  /** Identifies the revision's content. It is not a security digest. */
  contentHash: string;
}

/** The latest revision with its document as the engine stores it. A section the document lacks is absent. */
export interface ChannelMemoryDetail extends ChannelMemoryRevision {
  document: JsonValue;
}

/** The proof statuses the engine stores. */
export type ProofStatus = "collected" | "approved" | "withdrawn" | "superseded";

/** One proof in a channel's proof bank. `value` is an exact decimal string and is shown verbatim, never parsed. */
export interface ProofRecord {
  scopeId: string;
  answerId: string;
  proofId: string;
  status: ProofStatus;
  subject: string;
  value: string;
  units: string;
  origin: string;
  observedFrom: string;
  /** Empty text when the answer named no end date. */
  observedTo: string;
  /** Set on a superseded proof: the proof id that replaces it. */
  supersededBy: string | null;
  /** Set on a withdrawn proof when the engine holds a note. */
  withdrawalNote: string | null;
}

// ---- Typed briefs and platform variants (read only here) ----
// A typed brief is one of seven types, each with its own parts. A variant is one platform rendering of one brief at
// one brief revision, made under one platform rule version. Rules are global. The desktop writes none of them.

/** The seven brief types the engine stores. */
export type BriefType =
  | "talking_head"
  | "faceless_cartoon_short"
  | "explainer"
  | "carousel"
  | "ugc_character_review"
  | "product_pitch"
  | "product_demo";

/** Where a platform rule places a link. "none" means the platform carries no link. */
export type LinkPlacement = "caption" | "bio" | "first_comment" | "none";

/** One scene of an explainer beat. Every field is text, as the engine stores it. */
export interface SceneSpec {
  teaches: string;
  frame: string;
  motion: string;
  words: string;
  sound: string;
}

/** The five beats of an explainer, in story order. */
export interface ExplainerBeats {
  question: SceneSpec;
  model: SceneSpec;
  proof: SceneSpec;
  turn: SceneSpec;
  payoff: SceneSpec;
}

/** One product-demo step: the text said while the step is recorded. */
export interface DemoStep {
  say: string;
}

/** One typed brief in a channel. Its type-specific parts are set exactly for the types that carry them. */
export interface TypedBrief {
  scopeId: string;
  briefId: string;
  briefType: BriefType;
  hook: string;
  body: string;
  /** Set exactly when the brief is an explainer. */
  explainer: ExplainerBeats | null;
  /** Non-empty exactly when the brief is a product demo. */
  demoSteps: DemoStep[];
  /** Compare-and-swap revision. The desktop never writes it. */
  revision: number;
  createdAtUtc: string;
  updatedAtUtc: string;
}

/** One platform-native variant of one brief at one brief revision, under one platform rule version. */
export interface BriefVariant {
  scopeId: string;
  variantId: string;
  briefId: string;
  briefRevision: number;
  platform: string;
  ruleRevision: string;
  /** The placement the rule applied. */
  linkPlacement: LinkPlacement;
  /** The final caption as the engine stored it. Hashtags are part of this text; the engine keeps no separate list. */
  caption: string;
  /** The https link as given, or null when none was given. Shown as text and never opened. */
  link: string | null;
  createdAtUtc: string;
}

// ---- Analytics and manual metric imports (read only here) ----
// Both are read per channel scope. Numbers are pulled on the server (the daemon) or imported from a file the owner
// exported by hand. The desktop pulls nothing, imports nothing, and computes no rate, rank, average or comparison.
// Every value is an exact decimal string, shown exactly as stored.

/** Where a stored number came from: the platform's API, or a file the owner exported by hand. */
export type AnalyticsSource = "api" | "manual_export";

/** A metric is stored with a value, or as unavailable when the platform does not provide it. Never a stored zero. */
export type AnalyticsMetricState = "value" | "unavailable";

/** The latest stored observation of one metric of one post, with the post's age at that observation. */
export interface AnalyticsSummaryMetric {
  metric: string;
  state: AnalyticsMetricState;
  /** An exact decimal string, shown verbatim. Null when unavailable. */
  value: string | null;
  observedAtUtc: string;
  /** "lifetime" for an API row. Null for a manual row, which names no window. */
  window: string | null;
  source: AnalyticsSource;
  /** The pull that stored an API row. Null for a manual row. */
  pullId: string | null;
  /** The import that stored a manual row. Null for an API row. */
  importId: string | null;
  /** The observation instant minus the publish instant, in seconds. */
  postAgeSeconds: number;
}

/** One published post with its latest stored metrics. */
export interface AnalyticsPost {
  scopeId: string;
  jobId: string;
  platform: string;
  remotePostId: string;
  publishedAtUtc: string;
  metrics: AnalyticsSummaryMetric[];
}

/** One stored observation of one metric of one post, as analytics.list returns it. */
export interface AnalyticsObservation {
  scopeId: string;
  jobId: string;
  metric: string;
  state: AnalyticsMetricState;
  /** An exact decimal string, shown verbatim. Null when unavailable. */
  value: string | null;
  observedAtUtc: string;
  /** "lifetime" for an API row. Null for a manual row. */
  window: string | null;
  source: AnalyticsSource;
  /** The pull that stored an API row. Null for a manual row. */
  pullId: string | null;
  /** The import that stored a manual row. Null for an API row. */
  importId: string | null;
  /** Lineage of the publish job, when recorded. Absent otherwise. */
  variantId?: string | null;
  renderHash?: string | null;
  timeMap?: string | null;
}

/** One committed manual import in a channel. Its values appear with the posts they belong to. */
export interface MetricImport {
  scopeId: string;
  importId: string;
  platform: string;
  sourceNote: string;
  source: "manual_export";
  createdAtUtc: string;
  rowsWritten: number;
  observationsWritten: number;
}

// ---- Store health: heartbeats and restore receipts (read only) ----
// Both are whole-store reads: no scope. The desktop never beats a heartbeat, backs up the store or restores it.

/** alive: the age is at most the threshold. stale: older than it. missing: no record. unreadable: the file does not parse. */
export type HeartbeatState = "alive" | "stale" | "missing" | "unreadable";

/** The daemon's one heartbeat record. Null fields mean there is no readable record. */
export interface HeartbeatStatus {
  /** The instant the age was measured at, UTC. */
  nowUtc: string;
  staleAfterSeconds: number;
  /** True only when the state is alive: no heartbeat is not health. */
  allAlive: boolean;
  state: HeartbeatState;
  pid: number | null;
  /** The record's update time, milliseconds since the Unix epoch. */
  updatedMs: number | null;
  /** The engine's whole-second age, shown as given. */
  ageSeconds: number | null;
  /** The record's own state text (the beat note), or null. */
  recordState: string | null;
  /** Why the file could not be read. Null otherwise. */
  error: string | null;
}

/** One restore of the channel store from a verified backup. */
export interface RestoreReceipt {
  receiptId: string;
  restoredAtUtc: string;
  /** The digest of the backup's manifest file, as the engine stored it. */
  manifestDigest: string;
  /** Null when there was no readable store to replace. */
  schemaBefore: number | null;
  schemaAfter: number;
  /** Where the replaced store was set aside. Null when there was no store to set aside. */
  previousStore: string | null;
}

export interface RestoreReceiptList {
  /** Newest first, as the engine returns them. */
  receipts: RestoreReceipt[];
  /** Names of receipt files that did not parse. */
  unreadable: string[];
}

/**
 * The outcome of disconnecting an account. The account reads back as not connected. `cancelledJobs` is the number of
 * queued or waiting publish jobs the engine cancelled; `keptJobs` is the number it left because a post may already
 * exist on the platform (the daemon reconciles those).
 */
export interface AccountDisconnect {
  account: Account;
  cancelledJobs: number;
  keptJobs: number;
}
