import type {
  Account,
  AccountDisconnect,
  Campaign,
  CampaignAttribution,
  ChannelLink,
  DataError as BaseDataError,
  DataErrorCode as BaseDataErrorCode,
  DateRange,
  Digest,
  FailureItem,
  FailureResolution,
  HookTestSet,
  ModerationCapability,
  PublishJob,
  PublishJobRecord,
  ReconcileResult,
  ManualDraft,
  OperatorScope,
  OptimiseProposal,
  RejectDecision,
  SpendAllowance,
  SpendPrice,
  SpendReport,
  ThreadItem,
  TrackedLink,
  ContentDetail,
  ContentExperimentSummary,
  ContentRecord,
  DigestApplication,
  ResearchDigest,
  DeliveryEnvelope,
  DispatchAttempt,
  GateCheck,
  GatePolicy,
  GateRecord,
  OutgoingBrief,
  OutgoingBriefDetail,
  ChannelMemoryDetail,
  ChannelMemoryRevision,
  ProofRecord,
  ProofStatus,
  TypedBrief,
  BriefVariant,
  AnalyticsObservation,
  AnalyticsPost,
  MetricImport,
  HeartbeatStatus,
  RestoreReceiptList,
  InboxListing,
  InboxRaw,
  InboxStats,
  PublishNextView,
  PublishQuota,
} from "./types";

/**
 * Codes the engine returns that types.ts does not list. They are declared here, not in types.ts,
 * so this change stays inside the engine data source. Move them into types.ts with the next change
 * to that file.
 *   foreign_scope       the scope matches no stored channel
 *   engine_unavailable  the engine has no method or field for this yet; the message names it
 *   engine_error        any other engine error; `engineCode` carries the engine's own code
 */
export type EngineOnlyErrorCode = "foreign_scope" | "engine_unavailable" | "engine_error";
export type DataErrorCode = BaseDataErrorCode | EngineOnlyErrorCode;

export interface DataError extends Omit<BaseDataError, "code"> {
  code: DataErrorCode;
  /** Set on engine_error: the engine's own error code. */
  engineCode?: string;
}

/**
 * A typed result. Every value in types.ts still fits: the mock's Result<T> is assignable to this.
 */
export type Result<T> = { ok: true; value: T } | { ok: false; error: DataError };

/**
 * An account the engine holds that the desktop cannot map yet: its platform is not in SOCIAL_PLATFORMS, or a
 * state is not shown. It is listed apart from the mapped accounts, with its raw id and the reason. Never dropped.
 */
export interface UnmappedAccount {
  /** The desktop account id the account would have had. */
  id: string;
  /** The engine's raw platform id, shown as stored. */
  platform: string;
  reason: string;
}

/**
 * The seam between the screens and the daemon. The screens import only this interface. Today the
 * only implementation is MockDataSource. The daemon client implements the same methods later, so
 * no screen changes when it lands.
 *
 * Rule carried from the daemon: approve and reject fail on any mismatch between the revision or
 * account the operator saw and the job the daemon holds. Nothing is applied on mismatch.
 */
export interface DataSource {
  /** "mock" shows the Mock data banner on every screen. */
  readonly kind: "mock" | "daemon";
  /** The daemon's clock. Countdowns and "today" are computed from this, never from the browser. */
  nowUtc(): string;

  // Calendar, inbox and accounts
  listAccounts(): Promise<Account[]>;
  /** The accounts the last listAccounts could not map. Optional: the mock holds none. Read after listAccounts. */
  unmappedAccounts?(): readonly UnmappedAccount[];
  listJobs(range: DateRange): Promise<PublishJob[]>;
  getJob(jobId: string): Promise<PublishJob | null>;
  approve(jobId: string, revision: number, accountId: string): Promise<Result<PublishJob>>;
  reject(jobId: string, decision: RejectDecision): Promise<Result<PublishJob>>;
  pauseAccount(accountId: string, reason: string): Promise<Result<Account>>;
  resumeAccount(accountId: string): Promise<Result<Account>>;
  /**
   * Ends the connection to the platform and cancels the account's queued and waiting posts. The engine stores no
   * token, so the revocation is its state change; the person also revokes access in the platform's own settings.
   * Does not delete data (a deletion request does).
   */
  disconnectAccount(accountId: string): Promise<Result<AccountDisconnect>>;

  // Failures and digest
  listFailures(): Promise<FailureItem[]>;
  /** Asks the platform whether an outcome_unknown post exists. A read only; never publishes. */
  reconcile(jobId: string): Promise<Result<ReconcileResult>>;
  /**
   * Records how a failure ended. The daemon enforces the rules: a published or cancelled outcome
   * needs a matching reconcile result, acceptance as lost needs a note, and an alert takes only
   * "acknowledged".
   */
  resolveFailure(failureId: string, resolution: FailureResolution, note: string): Promise<Result<FailureItem>>;
  listDigest(): Promise<Digest>;

  // Publish jobs (read only). The desktop lists what the daemon stored; it never queues, steps, runs, reconciles,
  // cancels or resolves a job. Scope ids come from listScopes.
  /** Every publish job of one channel, newest first. An empty list is a state, not an error. */
  listPublishJobs(scopeId: string): Promise<PublishJobRecord[]>;

  // Threads
  listThreads(): Promise<ThreadItem[]>;
  resolveThread(threadId: string): Promise<Result<ThreadItem>>;
  /** Snoozes a thread until a UTC instant that must be in the future. */
  snoozeThread(threadId: string, untilUtc: string): Promise<Result<ThreadItem>>;
  listModerationCapabilities(): Promise<ModerationCapability[]>;

  // Manual drafts. the host app never posts a draft: the operator posts it by hand and records the link.
  listDrafts(): Promise<ManualDraft[]>;
  /**
   * Approves a drafted draft at exactly `revision`. The caller supplies the disclosure phrases; the
   * engine has no built-in list. The engine's gate decides, and a refusal changes nothing.
   */
  approveDraft(draftId: string, revision: number, disclosurePhrases: readonly string[]): Promise<Result<ManualDraft>>;
  /** Records the operator's post. The link must be https on an allowed host for the platform. */
  markDraftPosted(draftId: string, revision: number, postedUrl: string): Promise<Result<ManualDraft>>;
  /** Ends a drafted or approved draft with a reason. The reason is required. */
  skipDraft(draftId: string, revision: number, reason: string): Promise<Result<ManualDraft>>;

  // Suggestions and spend. Suggestions and the allowance and report are read per channel scope; the
  // scope ids come from listScopes. The price table is not scoped. "now" is always nowUtc().
  listScopes(): Promise<OperatorScope[]>;
  listProposals(scopeId: string): Promise<OptimiseProposal[]>;
  /** Null when the suggestion is not in this scope. */
  getProposal(scopeId: string, proposalId: string): Promise<OptimiseProposal | null>;
  /**
   * Accepts a suggestion at exactly `expectedRevision`. The engine creates one draft calendar entry and
   * records its id. The entry still needs a real caption and approval; nothing is approved here.
   */
  acceptProposal(scopeId: string, proposalId: string, expectedRevision: number): Promise<Result<OptimiseProposal>>;
  /** Dismisses a suggestion at exactly `expectedRevision`. Creates no calendar entry. */
  dismissProposal(scopeId: string, proposalId: string, expectedRevision: number): Promise<Result<OptimiseProposal>>;
  /** Null when no allowance is set for the month. That is a state, not an error. */
  getAllowance(scopeId: string, month: string): Promise<SpendAllowance | null>;
  /** Null when no allowance is set for the month. That is a state, not an error. */
  spendReport(scopeId: string, month: string): Promise<SpendReport | null>;
  listPrices(): Promise<SpendPrice[]>;

  // Campaigns, tracked links and attribution (read only). Scoped like suggestions: campaign and link ids
  // are the engine's own. The desktop creates no campaign, cuts no link and records no visit.
  listCampaigns(scopeId: string): Promise<Campaign[]>;
  /** Every tracked link of one campaign in the scope. An empty list is a state, not an error. */
  listCampaignLinks(scopeId: string, campaignId: string): Promise<TrackedLink[]>;
  /** Null when the campaign is not in this scope. */
  campaignAttribution(scopeId: string, campaignId: string): Promise<CampaignAttribution | null>;
  /**
   * Every tracked link in one scope, across its campaigns, each with its campaign's name and visit count. The
   * campaign order is the engine's, then variant, platform and link id. An empty list is a state, not an error.
   */
  listChannelLinks(scopeId: string): Promise<ChannelLink[]>;

  // Hook tests (read only). The desktop never reads, creates or binds a set: reading an open set is a
  // server-side step.
  listHookTests(scopeId: string): Promise<HookTestSet[]>;
  /** Null when no set with this id is in the scope. */
  getHookTest(scopeId: string, setId: string): Promise<HookTestSet | null>;

  // Research digests and the content library (read only). The desktop never puts, plans, applies,
  // withdraws or records either one. Scope ids come from listScopes.
  /** Every digest in one scope, ordered by digest date then id. */
  listDigests(scopeId: string): Promise<ResearchDigest[]>;
  /** Null when no digest with this id is in the scope. */
  getDigest(scopeId: string, digestId: string): Promise<ResearchDigest | null>;
  /** Which digest entries became draft calendar entries. Null when the digest is not in the scope. */
  digestApplications(scopeId: string, digestId: string): Promise<DigestApplication[] | null>;
  /** Every content record in one scope, ordered by content id. */
  listContent(scopeId: string): Promise<ContentRecord[]>;
  /** A record with its variants. Null when no record with this id is in the scope. */
  getContent(scopeId: string, contentId: string): Promise<ContentDetail | null>;
  /** The engine's variant comparison for one record. Null when the record is not in the scope. */
  contentExperimentSummary(scopeId: string, contentId: string): Promise<ContentExperimentSummary | null>;

  // Channel memory and the proof bank (read only). The desktop never puts a memory revision, imports proofs or
  // applies proof history. Scope ids come from listScopes.
  /** The latest memory revision with its document. Null when none has been written: a state, not an error. */
  channelMemory(scopeId: string): Promise<ChannelMemoryDetail | null>;
  /** Every stored memory revision, newest first, without documents. An empty list when none is stored. */
  channelMemoryHistory(scopeId: string): Promise<ChannelMemoryRevision[]>;
  /** The proofs of one scope, sorted by answer id. A null status lists every status. */
  listProofs(scopeId: string, status: ProofStatus | null): Promise<ProofRecord[]>;

  // Text gate, outgoing briefs, deliveries and dispatch attempts (read only). The desktop never puts a
  // policy, never records a check, and never issues, accepts or runs a brief.
  /** The latest gate policy of the scope. Null when none is set, which means nothing can pass. That is a state, not an error. */
  gatePolicy(scopeId: string): Promise<GatePolicy | null>;
  /**
   * A dry-run check of one script with no claim annotations. Stores nothing. A refusal (for example hidden
   * characters, or no policy) is a Result, not a throw.
   */
  checkText(scopeId: string, subjectId: string, text: string): Promise<Result<GateCheck>>;
  /** The stored verdict records of one subject, newest first. A subject with none is an empty list. */
  gateRecords(scopeId: string, subjectId: string): Promise<GateRecord[]>;
  /** Every outgoing brief in one scope, ordered by request id. */
  listBriefs(scopeId: string): Promise<OutgoingBrief[]>;
  /** Null when no brief with this request id is in the scope. */
  getBrief(scopeId: string, requestId: string): Promise<OutgoingBriefDetail | null>;
  /** Every accepted delivery in one scope. */
  listDeliveries(scopeId: string): Promise<DeliveryEnvelope[]>;
  /** Null when no envelope with this id is in the scope. */
  getDelivery(scopeId: string, envelopeId: string): Promise<DeliveryEnvelope | null>;
  /** The dispatch attempts of one brief, oldest first. An empty list is a state, not an error. */
  listDispatchAttempts(scopeId: string, requestId: string): Promise<DispatchAttempt[]>;

  // Typed briefs and platform variants (read only). The desktop never puts a brief, derives a variant or writes a
  // rule. Briefs and variants are read per channel scope; the rule table is global and takes no scope.
  /** Every typed brief in one scope, ordered by brief id. */
  listTypedBriefs(scopeId: string): Promise<TypedBrief[]>;
  /** Null when no brief with this id is in the scope. */
  getTypedBrief(scopeId: string, briefId: string): Promise<TypedBrief | null>;
  /** The variants of one brief, ordered by platform then variant id. An empty list is a state, not an error. */
  listBriefVariants(scopeId: string, briefId: string): Promise<BriefVariant[]>;

  // Analytics and manual metric imports (read only). Numbers are pulled on the server (the daemon) or imported from a
  // file the owner exported; the desktop pulls and imports nothing and computes no rate, rank, average or comparison.
  // Every value is an exact decimal string, shown as stored. Scope ids come from listScopes.
  /** Every published post in one scope with its latest stored metrics. An empty list is a state, not an error. */
  listAnalyticsPosts(scopeId: string): Promise<AnalyticsPost[]>;
  /** Every stored observation of one post in the scope, oldest first. An empty list is a state, not an error. */
  listAnalyticsObservations(scopeId: string, jobId: string): Promise<AnalyticsObservation[]>;
  /** Every committed manual import in one scope. An empty list is a state, not an error. */
  listMetricImports(scopeId: string): Promise<MetricImport[]>;

  // Store health (read only). Whole store, no scope: the desktop never beats a heartbeat, backs up or restores.
  /**
   * The daemon heartbeat record's state at `nowUtc`, against `staleAfterSecs`. The ages and states come from the engine
   * as given. An empty list is a state, and it is not a sign of health.
   */
  heartbeatStatus(nowUtc: string, staleAfterSecs: number): Promise<HeartbeatStatus>;
  /** Every restore receipt, newest first, with the receipt files that could not be read. */
  listRestoreReceipts(): Promise<RestoreReceiptList>;

  // Webhook inbox (read only). Each event is untrusted text written by a stranger: the desktop shows it as received
  // and never replies, marks or handles it. A null scope id is the unmatched view. Scope ids come from listScopes.
  /** The newest page of events of one channel account, or of the unmatched events when scopeId is null. */
  listInbox(scopeId: string | null): Promise<InboxListing>;
  /** The raw item JSON of one event. Null when no event with this id is in the scope. */
  getInboxEvent(scopeId: string | null, eventId: string): Promise<InboxRaw | null>;
  /** Event and delivery counts as the daemon stores them. Whole store, not per channel. */
  inboxStats(): Promise<InboxStats>;

  // Publish pacing (read only). The desktop never queues, runs, pauses or changes a limit. "now" is always nowUtc().
  /** What may run now on every account of one channel, and what may not, with reasons. Scope ids come from listScopes. */
  listPublishNext(scopeId: string): Promise<PublishNextView>;
  /**
   * One account's pacing: the limits in force, the pause, the last create and the platform wait. Throws on a failed
   * read, so the screen shows the error for that account only.
   */
  getPublishQuota(scopeId: string, platform: string, remoteAccount: string): Promise<PublishQuota>;

  /** Present only on the mock. Lets the preview simulate an edit made on another device. */
  readonly mock?: MockControls;
}

export interface MockControls {
  simulateCaptionChange(jobId: string, caption: string): Promise<Result<PublishJob>>;
}
