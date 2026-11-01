// Wire shapes of the engine commands the desktop reads and writes. These mirror the Rust structs in
// crates/protocol/src/{schema,calendar,accounts}.rs (snake_case, as serde writes them). Only the fields
// the desktop uses are listed; the engine returns more.
import type { JsonValue } from "../types";

/** Every method the shell forwards. Keep in step with UI_METHODS in src-tauri/src/lib.rs. */
export const UI_METHODS = [
  "channel.list",
  "calendar.list",
  "calendar.get",
  "calendar.approve",
  "calendar.reject",
  "calendar.pause",
  "calendar.resume",
  "brief.get",
  "account.list",
  "account.get",
  "account.pause",
  "account.resume",
  "account.disconnect",
  "draft.list",
  "draft.approve",
  "draft.mark_posted",
  "draft.skip",
  "publish.get",
  "publish.list",
  "optimise.list",
  "optimise.get",
  "optimise.accept",
  "optimise.dismiss",
  "allowance.get",
  "spend.report",
  "price.list",
  "campaign.list",
  "link.list",
  "link.attribution",
  "hook_test.list",
  "hook_test.get",
  "digest.list",
  "digest.get",
  "digest.applied",
  "content.list",
  "content.get",
  "content.experiment_summary",
  "gate_policy.get",
  "gate.check",
  "gate.records",
  "brief_out.list",
  "brief_out.get",
  "delivery.list",
  "delivery.get",
  "dispatch.list",
  "memory.get",
  "memory.history",
  "proof.list",
  "typed_brief.list",
  "typed_brief.get",
  "variant.list",
  "analytics.list",
  "analytics.summary",
  "metric_import.list",
  "heartbeat.status",
  "store.restore_receipts",
  "inbox.list",
  "inbox.get",
  "inbox.stats",
  "publish.next",
  "publish.quota_get",
  "doctor",
  "rightkit.settings.get",
  "rightkit.settings.patch",
] as const;

export type UiMethod = (typeof UI_METHODS)[number];

/** The exact key of a channel: venture, channel and account. */
export interface EngineScope {
  venture: string;
  channel: string;
  account: string;
}

export interface EngineChannelRow {
  scope: EngineScope;
  brand_code: string;
  monetised: boolean;
  asset_rights: string;
}

export interface EngineChannelList {
  channels: EngineChannelRow[];
}

export interface EngineMediaItem {
  hash: string;
  kind: string;
  duration_ms: number | null;
}

/** One stored calendar entry, as every calendar command returns it. */
export interface EngineEntry {
  scope: EngineScope;
  entry_id: string;
  brief_id: string;
  brief_revision: number;
  platform: string;
  format: string;
  caption: string;
  media: EngineMediaItem[];
  /** UTC instant, YYYY-MM-DDTHH:MM:SSZ. */
  scheduled_at: string;
  timezone: string;
  local_time: string;
  /** The stored state. */
  state: string;
  /** `late` for a draft or media_pending entry past its preparation deadline; otherwise `state`. */
  effective_state: string;
  paused_from: string | null;
  revision: number;
  approval_state: string;
  approved_entry_revision: number | null;
  approved_by: string | null;
  policy_revision: number | null;
  rejection_reason: string | null;
  updated_at: string;
}

export interface EngineCalendarList {
  scope: EngineScope;
  entries: EngineEntry[];
}

/** Derived readiness the engine returns with every account. `blockers` are machine codes. */
export interface EngineAccountReadiness {
  can_publish: boolean;
  blockers: string[];
}

/**
 * One stored account, as account.list, account.get, account.put, account.pause and account.resume
 * return it. The account key is the scope plus the platform. States the engine cannot know are
 * stored as `unknown` or `not_started`.
 */
export interface EngineAccount {
  scope: EngineScope;
  platform: string;
  handle: string;
  display_name: string;
  account_kind: string;
  /** Set only for instagram: instagram_login or facebook_login. */
  login_route: string | null;
  /** not_connected, connected, reauth_required or unknown. */
  connection_state: string;
  /** UTC instant, or null when the expiry is not known. */
  token_expires_at: string | null;
  /** Each entitlement is not_started, in_progress, passed, blocked or unknown. */
  sandbox_mechanics: string;
  review_submission: string;
  audience_visible: string;
  /** UTC instant the quota cooldown ends. Active while it is after `now`. */
  quota_cooldown_until: string | null;
  paused: boolean;
  paused_reason: string | null;
  paused_at: string | null;
  revision: number;
  updated_at: string;
  readiness: EngineAccountReadiness;
}

/**
 * account.disconnect output (crates/protocol/src/accounts.rs). `cancelled_jobs` and `kept_jobs` are publish job ids.
 */
export interface EngineAccountDisconnect {
  account: EngineAccount;
  cancelled_jobs: string[];
  kept_jobs: string[];
}

export interface EngineAccountList {
  accounts: EngineAccount[];
}

/** One stored manual draft, as every draft command returns it (crates/protocol/src/drafts.rs). */
export interface EngineDraft {
  scope: EngineScope;
  draft_id: string;
  platform: string;
  kind: string;
  target: string | null;
  title: string | null;
  body: string;
  body_chars: number;
  disclosure_required: boolean;
  state: string;
  revision: number;
  posted_url: string | null;
  /** UTC instant, YYYY-MM-DDTHH:MM:SSZ. Set only when posted. */
  posted_at: string | null;
  skip_reason: string | null;
  source_note: string;
  created_at: string;
  updated_at: string;
}

export interface EngineDraftList {
  scope: EngineScope;
  drafts: EngineDraft[];
}

/** draft.mark_posted reply: the draft plus the host check that passed. */
export interface EngineDraftMarkPosted {
  draft: EngineDraft;
  host_check: string;
  host_note: string | null;
}

/** A remote id recorded by the publish core (crates/protocol/src/publish.rs, PublishRemote). */
export interface EnginePublishRemote {
  container_id: string | null;
  post_id: string | null;
  permalink: string | null;
}

/** One durable publish job, as publish.get and publish.list return it. Holds no token. */
export interface EnginePublishJob {
  job_id: string;
  scope: EngineScope;
  remote_account: string;
  platform: string;
  entry_id: string;
  entry_revision: number;
  payload: { caption: string; format: string };
  phase: string;
  remote: EnginePublishRemote;
  attempts: number;
  polls: number;
  reconciles: number;
  inflight: string | null;
  /** UTC instant, or null when the job is not waiting. */
  next_attempt_at: string | null;
  /** queued, running, waiting, published, failed, outcome_unknown or cancelled. */
  state: string;
  last_error_code: string | null;
  last_error_message: string | null;
  created_at: string;
  updated_at: string;
  revision: number;
  /** Set only when a person closed an outcome_unknown job with publish.resolve_unknown. Null otherwise. */
  resolution?: EnginePublishResolution | null;
}

/** What a person recorded for an outcome_unknown job (PublishResolution, crates/protocol/src/publish.rs). */
export interface EnginePublishResolution {
  decision: string;
  resolved_by: string;
  resolved_at: string;
  note: string;
  remote_id_entered_by_hand: boolean;
}

export interface EnginePublishList {
  scope: EngineScope;
  jobs: EnginePublishJob[];
}

/** One stored inbox event (crates/protocol/src/inbox.rs, InboxEvent). Every text field is untrusted. */
export interface EngineInboxEvent {
  event_id: string;
  delivery: string;
  seq: number;
  platform: string;
  /** The bound channel account, or null when unmatched. */
  scope: EngineScope | null;
  unmatched_account: boolean;
  /** `no_account` or `ambiguous_account`, or null when matched. */
  unmatched_reason: string | null;
  recipient_id: string | null;
  kind: string;
  platform_event_id: string | null;
  sender_id: string | null;
  text: string | null;
  event_time: string | null;
  oversize: boolean;
  received_at: string;
  state: string;
  state_by: string | null;
  state_at: string | null;
  state_note: string;
}

export interface EngineInboxList {
  events: EngineInboxEvent[];
  /** The event id to pass as `before` for the next page, or null. */
  next_before: string | null;
}

/** inbox.get reply: the event and the raw item JSON it came from. */
export interface EngineInboxGet {
  event: EngineInboxEvent;
  raw: unknown;
}

export interface EngineInboxStatsGroup {
  scope: EngineScope | null;
  kind: string;
  state: string;
  count: number;
}

/** inbox.stats reply (InboxStatsOutput). Takes no input; the counts are whole-store. */
export interface EngineInboxStats {
  total_events: number;
  unmatched_events: number;
  groups: EngineInboxStatsGroup[];
  deliveries_parsed: number;
  deliveries_unparseable: number;
  refused_total: number;
}

/** A publish job named by publish.next (crates/protocol/src/publish.rs, PublishNextJob). */
export interface EnginePublishNextJob {
  job_id: string;
  scope: EngineScope;
  platform: string;
  remote_account: string;
  entry_id: string;
  state: string;
  phase: string;
  scheduled_at: string;
}

/** A job publish.next says may not run now. `until` and `behind` are absent when the engine gives none. */
export interface EnginePublishNextBlocked extends EnginePublishNextJob {
  reason: string;
  reasons: string[];
  until?: string | null;
  detail: string;
  behind?: string | null;
}

/** publish.next reply (PublishNextOutput). */
export interface EnginePublishNext {
  now: string;
  ready: EnginePublishNextJob[];
  ready_total: number;
  blocked: EnginePublishNextBlocked[];
  in_progress: EnginePublishNextJob[];
}

/** publish.quota_get reply (PublishQuotaRecord). Limits are the stored ones, or the host app defaults when opted_in is false. */
export interface EnginePublishQuota {
  scope: EngineScope;
  platform: string;
  remote_account: string;
  opted_in: boolean;
  min_gap_secs: number;
  max_per_day: number;
  default_min_gap_secs: number;
  default_max_per_day: number;
  paused_until?: string | null;
  last_create_at?: string | null;
  creates_today: number;
  day_resets_at: string;
  rate_limited_at?: string | null;
  rate_limited_until?: string | null;
}

/** One optimise proposal (crates/protocol/src/optimise.rs, OptimiseProposal). value and threshold are exact integer text. */
export interface EngineOptimiseProposal {
  scope: EngineScope;
  proposal_id: string;
  kind: string;
  state: string;
  revision: number;
  source_job_id: string;
  platform: string;
  metric: string;
  window: string;
  observed_at: string;
  value: string;
  threshold: string;
  sample_size: number;
  decile_size: number;
  due_at: string;
  lineage_job_ids: string[];
  lineage_pull_ids: string[];
  decision_note: string | null;
  decided_at: string | null;
  calendar_entry_id: string | null;
  created_at: string;
  updated_at: string;
}

export interface EngineOptimiseList {
  scope: EngineScope;
  proposals: EngineOptimiseProposal[];
}

/** One allowance row (crates/protocol/src/spend.rs, SpendAllowance). Amounts are integer micro-units. */
export interface EngineSpendAllowance {
  scope: EngineScope;
  month: string;
  currency: string;
  limit_micro: number;
  reserved_micro: number;
  committed_micro: number;
  remaining_micro: number;
  revision: number;
  updated_at: string;
}

export interface EngineSpendRouteTotal {
  route_id: string;
  count_reserved: number;
  count_committed: number;
  count_released: number;
  count_expired: number;
  open_micro: number;
  forecast_micro: number;
  measured_micro: number;
  variance_micro: number;
  over_reservations: number;
}

export interface EngineSpendReport {
  scope: EngineScope;
  month: string;
  currency: string;
  limit_micro: number;
  reserved_micro: number;
  committed_micro: number;
  remaining_micro: number;
  reconciles: boolean;
  routes: EngineSpendRouteTotal[];
}

/** One price version with its qualification (SpendPriceView flattens the price row). */
export interface EngineSpendPrice {
  route_id: string;
  version: number;
  provider: string;
  model_slug: string;
  unit: string;
  amount_micro: number;
  currency: string;
  source_url: string | null;
  source_path_line: string | null;
  fetched_at: string;
  verified_by: string | null;
  created_at: string;
  qualified: boolean;
  reason: string | null;
}

export interface EngineSpendPriceList {
  prices: EngineSpendPrice[];
}

/** Engine error codes the desktop maps (docs/CLI-POSTRIGHT.md, "Calendar error codes"). */
export type EngineErrorCodeName =
  | "foreign_scope"
  | "not_found"
  | "brief_not_found"
  | "revision_conflict"
  | "revision_required"
  | "invalid_transition"
  | "time_nonexistent"
  | "time_ambiguous"
  | "timezone_unsupported"
  | "time_out_of_table"
  | "bad_input"
  | "db_error";

/** One campaign (crates/protocol/src/links.rs, Campaign). */
export interface EngineCampaign {
  scope: EngineScope;
  campaign_id: string;
  name: string;
  utm_campaign: string;
  revision: number;
  created_at: string;
  updated_at: string;
}

/** campaign.list reply: every campaign in one scope, ordered by campaign id. */
export interface EngineCampaignList {
  scope: EngineScope;
  campaigns: EngineCampaign[];
}

/** One tracked link (TrackedLink). final_url is fixed at creation; the UTM parts are the ones it was cut with. */
export interface EngineTrackedLink {
  scope: EngineScope;
  link_id: string;
  campaign_id: string;
  variant_id: string;
  platform: string;
  destination: string;
  calendar_entry_id: string | null;
  utm_source: string;
  utm_medium: string;
  utm_campaign: string;
  utm_content: string;
  final_url: string;
  created_at: string;
}

/** link.list reply. Ordered by campaign id, variant id, platform and link id. */
export interface EngineLinkList {
  scope: EngineScope;
  links: EngineTrackedLink[];
}

/** One link's visit count (AttributionRow). visits is a plain whole count. */
export interface EngineAttributionRow {
  link_id: string;
  variant_id: string;
  platform: string;
  account: string;
  calendar_entry_id: string | null;
  visits: number;
}

/** link.attribution reply: counts only, with the sum in total_visits. */
export interface EngineLinkAttribution {
  scope: EngineScope;
  campaign_id: string;
  links: EngineAttributionRow[];
  total_visits: number;
}

/** One opening of a hook test set (HookTestOpening). Value fields are set only once a read has compared it. */
export interface EngineHookTestOpening {
  opening_index: number;
  text: string;
  job_id: string | null;
  bound_at: string | null;
  role: string | null;
  value: string | null;
  observed_at: string | null;
  pull_id: string | null;
  published_at: string | null;
}

/** One variant set (HookTestSet), as hook_test.get returns it and hook_test.list holds it. */
export interface EngineHookTestSet {
  scope: EngineScope;
  set_id: string;
  brief_ref: string;
  body: string;
  metric: string;
  window: string;
  state: string;
  revision: number;
  winner_opening: number | null;
  decided_at: string | null;
  created_at: string;
  updated_at: string;
  openings: EngineHookTestOpening[];
}

/** hook_test.list reply: every set in one scope, ordered by creation then set id. */
export interface EngineHookTestList {
  scope: EngineScope;
  sets: EngineHookTestSet[];
}

/** One digest entry (crates/protocol/src/digest.rs, DigestEntry). source is a url or path:line. */
export interface EngineDigestEntry {
  entry_id: string;
  topic: string;
  finding: string;
  source: string;
  fetched_on: string;
  platform: string;
  format: string;
}

/** One stored digest (DigestRecord), as digest.get returns it and digest.list holds it. */
export interface EngineDigestRecord {
  scope: EngineScope;
  digest_id: string;
  digest_date: string;
  entries: EngineDigestEntry[];
  created_at: string;
}

/** digest.list reply: every digest in one scope, ordered by date then id. */
export interface EngineDigestList {
  scope: EngineScope;
  digests: EngineDigestRecord[];
}

/** One applied digest entry (DigestApplication). entry_id is the draft calendar entry it created. */
export interface EngineDigestApplication {
  digest_id: string;
  digest_entry_id: string;
  brief_id: string;
  entry_id: string;
  applied_at: string;
}

/** digest.applied reply: which digest entries produced which calendar entries. */
export interface EngineDigestApplied {
  scope: EngineScope;
  digest_id: string;
  applications: EngineDigestApplication[];
}

/** One media item a content record reuses (ContentMediaItem). duration_ms is a whole number or null. */
export interface EngineContentMediaItem {
  hash: string;
  kind: string;
  duration_ms: number | null;
}

/** One stored content record (ContentRecord). state is the derived state at the clock the read used. */
export interface EngineContentRecord {
  scope: EngineScope;
  content_id: string;
  title: string;
  media: EngineContentMediaItem[];
  base_caption: string;
  evergreen: boolean;
  cooldown_days: number;
  expires_at: string | null;
  state: string;
  withdrawal_reason: string | null;
  revision: number;
  created_at: string;
  updated_at: string;
}

/** One caption or CTA variant (ContentVariant). */
export interface EngineContentVariant {
  scope: EngineScope;
  variant_id: string;
  content_id: string;
  kind: string;
  text: string;
  created_at: string;
}

/** content.list reply: every record in one scope, ordered by content id. */
export interface EngineContentList {
  scope: EngineScope;
  records: EngineContentRecord[];
}

/** content.get reply: one record and its variants. */
export interface EngineContentGet {
  record: EngineContentRecord;
  variants: EngineContentVariant[];
}

/** Plain counts for one variant (VariantCount). Counts are whole numbers. */
export interface EngineContentVariantCount {
  variant_id: string;
  text: string;
  planned: number;
  published: number;
  cancelled: number;
  meets_minimum: boolean;
  /** Published occurrences whose primary metric value was compared. Zero unless a metric is declared. */
  compared?: number;
  /** Exact sum of the compared values, as decimal text. "0" unless a metric is declared. */
  total?: string;
}

/** content.experiment_summary request (ContentExperimentSummaryInput). */
export interface EngineContentExperimentSummaryInput {
  scope: EngineScope;
  content_id: string;
  min_occurrences?: number;
  /** The clock for the 24-hour rule of a declared primary metric. Defaults to the current time. */
  now?: string;
}

/** content.experiment_summary reply (ContentExperimentSummaryOutput). */
export interface EngineContentExperimentSummary {
  scope: EngineScope;
  content_id: string;
  min_occurrences: number;
  verdict: string;
  reason: string;
  /** The primary metric declared for the record, or null. */
  primary_metric?: string | null;
  caption_winner?: string | null;
  cta_winner?: string | null;
  caption: EngineContentVariantCount[];
  cta: EngineContentVariantCount[];
}

/** content.declare_metric request (ContentDeclareMetricInput). */
export interface EngineContentDeclareMetricInput {
  scope: EngineScope;
  content_id: string;
  metric: string;
  now?: string;
}

/** content.declare_metric reply (ContentDeclareMetricOutput). `window` is "24h". `created` is false when the same metric was already declared. */
export interface EngineContentDeclareMetricOutput {
  scope: EngineScope;
  content_id: string;
  metric: string;
  window: string;
  declared_at: string;
  created: boolean;
}

/** content.plan_occurrence request (ContentPlanOccurrenceInput). */
export interface EngineContentPlanOccurrenceInput {
  scope: EngineScope;
  content_id: string;
  platform: string;
  scheduled_at: string;
  caption_variant_id?: string;
  cta_variant_id?: string;
  calendar_entry_id?: string;
  /** On a cooldown conflict, plan at the earliest allowed time instead of refusing. Null or false keeps the refusal. */
  reschedule_on_cooldown?: boolean | null;
  now?: string;
}

/** gate_policy.get reply: one stored policy revision (crates/protocol/src/gate.rs, GatePolicy). Thresholds are integers in thousandths. */
export interface EngineGatePolicy {
  scope: EngineScope;
  revision: number;
  policy: {
    banned_phrases: string[];
    stock_openers: string[];
    min_words: number;
    dash_max_per_100_words_milli: number;
    semicolon_max_per_100_words_milli: number;
    variance_min_milli: number;
    variance_min_sentences: number;
    tricolon_max_per_100_sentences_milli: number;
    tricolon_min_sentences: number;
    required: {
      banned_phrase: boolean;
      dash_density: boolean;
      semicolon_density: boolean;
      sentence_length_variance: boolean;
      tricolon_frequency: boolean;
      stock_opener: boolean;
    };
  };
  created_at: string;
}

/** One rule's finding in a gate.check reply (GateRuleResult). */
export interface EngineGateRule {
  rule_id: string;
  required: boolean;
  verdict: string;
  blocks: boolean;
  reason: string;
}

/** gate.check reply (GateCheckOutput). The desktop always sends record false, so recorded is false here. */
export interface EngineGateCheck {
  scope: EngineScope;
  subject_id: string;
  disposition: string;
  policy_revision: number;
  text_hash: string;
  rules: EngineGateRule[];
  blocking_rules: string[];
  proof_ids_used: string[];
  recorded: boolean;
  record_seq: number | null;
}

/** One stored verdict record (GateRecord). The record's own JSON is not listed: the desktop shows these columns only. */
export interface EngineGateRecord {
  record_seq: number;
  subject_id: string;
  text_hash: string;
  policy_revision: number;
  disposition: string;
  checked_at: string;
}

/** gate.records reply, newest first. The records carry no scope of their own, so the scope is the one read. */
export interface EngineGateRecords {
  scope: EngineScope;
  subject_id: string;
  records: EngineGateRecord[];
}

/** One outgoing brief (BriefOutRecord). document is the brief exactly as the engine accepted it. */
export interface EngineBriefOut {
  scope: EngineScope;
  request_id: string;
  brief_id: string;
  revision: number;
  state: string;
  document: JsonValue;
  /** Every envelope stored for the brief, one per deliverable key. */
  deliveries: EngineDeliveryRecord[];
  created_at: string;
  updated_at: string;
}

export interface EngineBriefOutList {
  scope: EngineScope;
  briefs: EngineBriefOut[];
}

/** One accepted delivery envelope (DeliveryRecord). */
export interface EngineDeliveryRecord {
  scope: EngineScope;
  envelope_id: string;
  request_id: string;
  brief_id: string;
  brief_revision: number;
  /** The row key the editor engine gave the envelope; empty when stored without one. */
  deliverable: string;
  document: JsonValue;
  accepted_at: string;
}

export interface EngineDeliveryList {
  scope: EngineScope;
  deliveries: EngineDeliveryRecord[];
}

/**
 * One dispatch attempt (DispatchAttempt). `detail` is the engine's own JSON object: it carries message and
 * stderr_tail, among other keys. The attempt carries no scope; the scope is the one the list was read in.
 */
export interface EngineDispatchAttempt {
  attempt_id: number;
  request_id: string;
  engine: string;
  started_at: string;
  finished_at: string | null;
  exit_code: number | null;
  outcome: string;
  detail: JsonValue;
}

export interface EngineDispatchList {
  scope: EngineScope;
  attempts: EngineDispatchAttempt[];
}

/** One stored channel memory revision without its document (memory.history, crates/protocol/src/memory.rs). */
export interface EngineMemoryRevisionRow {
  revision: number;
  supersedes: number | null;
  author: string;
  created_at: string;
  content_hash: string;
}

/** memory.get reply: one revision with its full document. Absent revision means the latest. */
export interface EngineMemoryGet extends EngineMemoryRevisionRow {
  scope: EngineScope;
  document: JsonValue;
}

/** memory.history reply, newest revision first. Empty when nothing is stored. */
export interface EngineMemoryHistory {
  scope: EngineScope;
  revisions: EngineMemoryRevisionRow[];
}

/** One stored proof (ProofRow, crates/protocol/src/schema.rs). value is the exact decimal as text, never a number. */
export interface EngineProofRow {
  venture: string;
  channel: string;
  account: string;
  answer_id: string;
  proof_id: string;
  status: string;
  subject: string;
  value: string;
  units: string;
  origin: string;
  observed_from: string;
  observed_to: string;
  source_excerpt: string;
  superseded_by: string | null;
  withdrawal_note: string | null;
}

/** proof.list reply: the proofs of exactly one scope, sorted by answer id. */
export interface EngineProofList {
  scope: EngineScope;
  proofs: EngineProofRow[];
}

// ---- Typed briefs and platform variants (crates/protocol/src/variants.rs) ----
// Snake_case, as serde writes the Rust structs. Read only here: the desktop never calls typed_brief.put or
// variant.derive.

/** variant.derive request (VariantDeriveInput). Only the fields the desktop would send are listed. */
export interface EngineVariantDeriveInput {
  scope: EngineScope;
  brief_id: string;
  platforms: string[];
  hashtags?: string[];
  link?: string;
  duration_seconds?: number;
  /** Refuse a platform rule whose fetched_at is more than this many days before now (rule_stale). Null applies no age limit. */
  max_rule_age_days?: number | null;
  now?: string;
}

/** One scene of an explainer beat (SceneSpec). Every field is text. */
export interface EngineSceneSpec {
  teaches: string;
  frame: string;
  motion: string;
  words: string;
  sound: string;
}

/** The five beats of an explainer (ExplainerBeats). Present exactly on explainer briefs. */
export interface EngineExplainerBeats {
  question: EngineSceneSpec;
  model: EngineSceneSpec;
  proof: EngineSceneSpec;
  turn: EngineSceneSpec;
  payoff: EngineSceneSpec;
}

/** One product-demo step (DemoStep). */
export interface EngineDemoStep {
  say: string;
}

/** One stored typed brief (TypedBrief), as typed_brief.get and typed_brief.list return it. */
export interface EngineTypedBrief {
  scope: EngineScope;
  brief_id: string;
  brief_type: string;
  hook: string;
  body: string;
  explainer: EngineExplainerBeats | null;
  demo_steps: EngineDemoStep[];
  revision: number;
  created_at: string;
  updated_at: string;
}

/** typed_brief.list reply: every brief in one scope, ordered by brief id. */
export interface EngineTypedBriefList {
  scope: EngineScope;
  briefs: EngineTypedBrief[];
}

/** One platform variant (BriefVariant). The caption is the final text as stored. */
export interface EngineBriefVariant {
  scope: EngineScope;
  variant_id: string;
  brief_id: string;
  brief_revision: number;
  platform: string;
  rule_revision: string;
  link_placement: string;
  caption: string;
  link: string | null;
  first_comment: string | null;
  bio_link: string | null;
  created_at: string;
}

/** variant.list reply: the variants of one brief, ordered by platform then variant id. */
export interface EngineVariantList {
  scope: EngineScope;
  brief_id: string;
  variants: EngineBriefVariant[];
}

// ---- Analytics and manual metric imports (crates/protocol/src/analytics.rs, metric_import.rs) ----
// Read only here: the desktop never calls analytics.pull, metric_import.preview or metric_import.commit.

/**
 * One summary metric (AnalyticsSummaryMetric). It carries no source field: an import id marks a manual row, and
 * its absence marks an API row. The field is left out of the JSON on API rows.
 */
export interface EngineAnalyticsSummaryMetric {
  metric: string;
  state: string;
  value: string | null;
  observed_at: string;
  window: string | null;
  pull_id: string | null;
  import_id?: string;
  post_age_secs: number;
}

export interface EngineAnalyticsSummaryJob {
  job_id: string;
  platform: string;
  remote_post_id: string;
  published_at: string;
  metrics: EngineAnalyticsSummaryMetric[];
}

/** analytics.summary reply: jobs by publish instant, metrics by name. */
export interface EngineAnalyticsSummary {
  scope: EngineScope;
  jobs: EngineAnalyticsSummaryJob[];
}

/** One stored observation (AnalyticsObservation). `source` is `api` or `manual_export`. */
export interface EngineAnalyticsObservation {
  job_id: string;
  platform: string;
  remote_post_id: string;
  published_at: string;
  metric: string;
  state: string;
  value: string | null;
  observed_at: string;
  window: string | null;
  source: string;
  pull_id: string | null;
  import_id?: string;
  /** Lineage of the publish job, when analytics.lineage_put recorded one. Absent otherwise. */
  variant_id?: string;
  render_hash?: string;
  time_map?: string;
}

/** analytics.list reply, ordered by observed instant, job and metric. The scope is the one the read was made in. */
export interface EngineAnalyticsObservations {
  scope: EngineScope;
  observations: EngineAnalyticsObservation[];
}

/** One committed manual import (MetricImportRecord). */
export interface EngineMetricImportRecord {
  scope: EngineScope;
  import_id: string;
  platform: string;
  source_note: string;
  source: string;
  created_at: string;
  rows_written: number;
  observations_written: number;
}

/** metric_import.list reply, ordered by creation instant, then import id. */
export interface EngineMetricImportList {
  scope: EngineScope;
  imports: EngineMetricImportRecord[];
}

// ---- Store health (crates/protocol/src/heartbeat.rs, crates/protocol/src/backup.rs) ----
// Read only here: the desktop never calls heartbeat.beat, store.backup or store.restore.

/** heartbeat.status reply: the one record the daemon writes. pid, updated_ms and age_secs are null when there is no readable record. */
export interface EngineHeartbeatStatus {
  now: string;
  stale_after_secs: number;
  all_alive: boolean;
  /** alive | stale | missing | unreadable */
  state: string;
  pid: number | null;
  updated_ms: number | null;
  age_secs: number | null;
  /** The record's own `state` value (the beat note); any JSON value. */
  record_state: unknown;
  error: string | null;
}

/** One restore receipt (StoreRestoreReceipt). The schema before and the set-aside path are absent when there were none. */
export interface EngineRestoreReceipt {
  format: string;
  receipt_id: string;
  restored_at: string;
  source_dir: string;
  manifest_digest: string;
  backup_digest: string;
  backup_created_at: string;
  backup_schema_version: number;
  schema_before?: number | null;
  schema_after: number;
  previous_store?: string | null;
  table_counts_after: Record<string, number>;
}

/** store.restore_receipts reply, newest first. `unreadable` names receipt files that did not parse. */
export interface EngineRestoreReceiptList {
  receipts: EngineRestoreReceipt[];
  unreadable: string[];
}
