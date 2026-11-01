// The DataSource backed by the supervised engine, through the shell (rk_shell_engine_call_tagged).
// Fields and methods the engine does not provide are named in docs/design/p9-desktop-screens-v1.md
// ("Engine gaps"). Reads throw EngineUnavailableError or EngineReadError, which the screens show as
// their load error. Writes return a Result, which the screens show as a notice.
import { engineCall, type EngineCallError, type EngineCallResult } from "../../lib/host.js";
import type { DataError, DataSource, Result, UnmappedAccount as UnmappedAccountView } from "../source.js";
import type {
  Account,
  AccountDisconnect,
  Campaign,
  CampaignAttribution,
  ChannelLink,
  DateRange,
  Digest,
  FailureItem,
  FailureResolution,
  ModerationCapability,
  PublishJob,
  PublishJobRecord,
  ManualDraft,
  OperatorScope,
  OptimiseProposal,
  ReconcileResult,
  RejectDecision,
  SpendAllowance,
  SpendPrice,
  SpendReport,
  ThreadItem,
  TrackedLink,
  HookTestSet,
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
} from "../types.js";
import { EngineReadError, EngineUnavailableError } from "./errors.js";
import {
  accountId,
  accountKey,
  accountOf,
  allowanceOf,
  attributionOf,
  campaignOf,
  channelLinksOf,
  draftOf,
  encodeRejection,
  engineInstant,
  entryToJob,
  hookTestOf,
  linkOf,
  parseAccountId,
  parseDraftKey,
  parseJobKey,
  parseScopeKey,
  priceOf,
  proposalOf,
  publishFailureOf,
  publishJobRecordOf,
  publishProgressFor,
  spendReportOf,
  type UnmappedAccount,
  contentDetailOf,
  contentRecordOf,
  digestApplicationOf,
  digestOf,
  experimentSummaryOf,
  briefDetailOf,
  briefOf,
  deliveryOf,
  dispatchAttemptOf,
  gateCheckOf,
  gatePolicyOf,
  gateRecordOf,
  memoryDetailOf,
  memoryRevisionOf,
  proofOf,
  typedBriefOf,
  briefVariantOf,
  analyticsObservationOf,
  analyticsPostsOf,
  metricImportOf,
  heartbeatStatusOf,
  restoreReceiptListOf,
  rowOf,
  inboxEventOf,
  inboxRawOf,
  inboxStatsOf,
  publishNextOf,
  publishQuotaOf,
} from "./mapping.js";
import type {
  EngineAccount,
  EngineAccountDisconnect,
  EngineAccountList,
  EngineCalendarList,
  EngineCampaign,
  EngineCampaignList,
  EngineHookTestList,
  EngineHookTestSet,
  EngineLinkAttribution,
  EngineLinkList,
  EngineChannelList,
  EngineDraft,
  EngineDraftList,
  EngineDraftMarkPosted,
  EngineEntry,
  EngineOptimiseList,
  EngineOptimiseProposal,
  EngineScope,
  EnginePublishJob,
  EnginePublishList,
  EngineSpendAllowance,
  EngineSpendPriceList,
  EngineSpendReport,
  EngineDigestApplied,
  EngineDigestList,
  EngineDigestRecord,
  EngineContentExperimentSummary,
  EngineContentGet,
  EngineContentList,
  EngineGatePolicy,
  EngineGateCheck,
  EngineGateRecords,
  EngineBriefOut,
  EngineBriefOutList,
  EngineDeliveryRecord,
  EngineDeliveryList,
  EngineDispatchList,
  EngineMemoryGet,
  EngineMemoryHistory,
  EngineProofList,
  EngineTypedBrief,
  EngineTypedBriefList,
  EngineVariantList,
  UiMethod,
  EngineAnalyticsObservations,
  EngineAnalyticsSummary,
  EngineMetricImportList,
  EngineHeartbeatStatus,
  EngineRestoreReceiptList,
  EngineInboxGet,
  EngineInboxList,
  EngineInboxStats,
  EnginePublishNext,
  EnginePublishQuota,
} from "./protocol.js";

/** The approver the engine records. The shell has no operator identity yet (see "Engine gaps"). */
const APPROVER = "desktop";

/** The newest page of inbox events the screen loads. A larger store shows a plain note that older events exist. */
const INBOX_PAGE = 200;

/** Recording a publish outcome or running the platform check happens on the daemon host. */
const DAEMON_ONLY_CHECK =
  "The platform check runs on the daemon host. The desktop cannot start it, so nothing was checked.";
const DAEMON_ONLY_RESOLVE =
  "Recording the outcome of a publish runs on the daemon host. The desktop cannot record it, so nothing was changed.";

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function unavailable<T>(what: string): Result<T> {
  return {
    ok: false,
    error: { code: "engine_unavailable", message: `${what} is unavailable from the engine yet.` },
  };
}

function failed<T>(error: DataError): Result<T> {
  return { ok: false, error };
}

/**
 * Engine error code to the operator's error code. Codes with a counterpart keep it; every other
 * engine code becomes engine_error, which carries the engine's own code.
 */
function dataErrorFrom(error: EngineCallError): DataError {
  switch (error.code) {
    case "revision_conflict":
      return { code: "revision_conflict", message: error.message };
    case "foreign_scope":
      return { code: "foreign_scope", message: error.message };
    case "not_found":
      return { code: "not_found", message: error.message };
    case "bad_input":
      return { code: "invalid_input", message: error.message };
    default:
      return { code: "engine_error", message: error.message, engineCode: error.code };
  }
}

export class EngineDataSource implements DataSource {
  readonly kind = "daemon" as const;

  /** Accounts from the last listAccounts that the desktop could not map. */
  private unmapped: UnmappedAccount[] = [];

  /**
   * The browser clock. The engine has no clock method yet, so countdowns use this value and not the
   * daemon's clock. See "Engine gaps" in the design doc.
   */
  nowUtc(): string {
    return engineInstant(new Date().toISOString());
  }

  // ---- Reads ----------------------------------------------------------------------------------

  /**
   * Every stored account, from account.list, mapped to the operator's Account. Fields the engine
   * does not hold take the named fallbacks in mapping.ts ("Accounts"). Each account maps on its own:
   * one whose platform or state the desktop does not show yet is left out of the result and kept,
   * with its typed error, in unmappedAccounts(). The other accounts still load. A failed call throws.
   */
  async listAccounts(): Promise<Account[]> {
    const nowUtc = this.nowUtc();
    const list = await read<EngineAccountList>("account.list", {});
    const accounts: Account[] = [];
    const unmapped: UnmappedAccount[] = [];
    for (const engine of list.accounts) {
      try {
        accounts.push(accountOf(engine, nowUtc));
      } catch (error) {
        unmapped.push({
          id: accountId(engine.scope, engine.platform),
          platform: engine.platform,
          error: error instanceof EngineReadError ? error : new EngineReadError("engine_mapping", messageOf(error)),
        });
      }
    }
    this.unmapped = unmapped;
    return accounts;
  }

  /** The accounts the last listAccounts could not map, each with the reason the desktop gives. */
  unmappedAccounts(): readonly UnmappedAccountView[] {
    return this.unmapped.map((row) => ({ id: row.id, platform: row.platform, reason: row.error.message }));
  }

  /**
   * Every calendar entry in the range. Each entry carries its publish progress (permalink, waiting
   * time, failure) from the engine's publish job for that entry revision. Publish reads run in every
   * role, so the desktop sees the daemon's progress.
   */
  async listJobs(range: DateRange): Promise<PublishJob[]> {
    const from = engineInstant(range.fromUtc);
    const to = engineInstant(range.toUtc);
    const scopes = await this.scopes();
    const [lists, publishByScope] = await Promise.all([
      Promise.all(scopes.map((scope) => read<EngineCalendarList>("calendar.list", { scope, from, to }))),
      Promise.all(scopes.map((scope) => this.publishJobsFor(scope))),
    ]);
    const jobs = lists.flatMap((list, index) => {
      const publishJobs = publishByScope[index];
      return list.entries.map((entry) => {
        const job = entryToJob(entry);
        job.publish = publishProgressFor(publishJobs, entry);
        return job;
      });
    });
    return jobs.sort((a, b) => a.scheduledAtUtc.localeCompare(b.scheduledAtUtc) || a.id.localeCompare(b.id));
  }

  /** Null when the id is unknown to the engine for its scope (not_found). */
  async getJob(jobId: string): Promise<PublishJob | null> {
    const key = parseJobKey(jobId);
    if (!key) return null;
    const reply = await engineCall<EngineEntry>("calendar.get", { scope: key.scope, entry_id: key.entryId });
    if (!reply.ok) {
      if (reply.error.code === "not_found") return null;
      throw new EngineReadError(reply.error.code, `calendar.get: ${reply.error.message}`);
    }
    const job = entryToJob(reply.value);
    const publishJobs = await this.publishJobsFor(key.scope);
    job.publish = publishProgressFor(publishJobs, reply.value);
    return job;
  }

  /**
   * Failure rows from publish jobs in state failed, outcome_unknown, or cancelled with a reason, from
   * publish.list per scope. A failed read throws (the screen's load error), so an empty list always
   * means no failures. The failures do not depend on the digest, which still has no engine source.
   */
  async listFailures(): Promise<FailureItem[]> {
    const scopes = await this.scopes();
    const failures: FailureItem[] = [];
    for (const scope of scopes) {
      const publishJobs = await this.publishJobsFor(scope);
      for (const job of publishJobs) {
        const failure = publishFailureOf(job);
        if (failure !== null) failures.push(failure);
      }
    }
    return failures;
  }

  /**
   * Every publish job in one scope, from publish.list, newest first. A read of the stored job only: the desktop
   * never steps, runs, reconciles, cancels or resolves a job. A failed read throws; an empty list is a state.
   */
  async listPublishJobs(scopeId: string): Promise<PublishJobRecord[]> {
    const jobs = await this.publishJobsFor(scopeOf(scopeId));
    return jobs
      .map((job) => publishJobRecordOf(job))
      .sort((a, b) => b.createdAtUtc.localeCompare(a.createdAtUtc) || a.jobId.localeCompare(b.jobId));
  }

  async listDigest(): Promise<Digest> {
    throw new EngineUnavailableError("The digest is not in the engine yet.");
  }

  async listThreads(): Promise<ThreadItem[]> {
    throw new EngineUnavailableError("Threads are not in the engine yet.");
  }

  async listModerationCapabilities(): Promise<ModerationCapability[]> {
    throw new EngineUnavailableError("Moderation capabilities are not in the engine yet.");
  }

  // ---- Writes ---------------------------------------------------------------------------------

  /**
   * Approves exactly `revision`. The account is checked here first, as the daemon does; the engine
   * then checks the revision itself. The engine approves without checking media readiness, so the
   * screen's own gate is the only media check (see "Engine gaps").
   */
  async approve(jobId: string, revision: number, expectedAccountId: string): Promise<Result<PublishJob>> {
    const key = parseJobKey(jobId);
    if (!key) return failed<PublishJob>({ code: "not_found", message: `No job ${jobId} in the engine.` });
    const current = await engineCall<EngineEntry>("calendar.get", { scope: key.scope, entry_id: key.entryId });
    if (!current.ok) return this.failure<PublishJob>(current.error, key.scope, key.entryId);
    if (accountId(key.scope, current.value.platform) !== expectedAccountId) {
      return failed<PublishJob>({ code: "account_mismatch", message: "This job belongs to another account. Nothing was approved." });
    }
    const reply = await engineCall<EngineEntry>("calendar.approve", {
      scope: key.scope,
      entry_id: key.entryId,
      expected_revision: revision,
      approved_by: APPROVER,
      policy_revision: current.value.policy_revision ?? 0,
    });
    return this.entryResult(reply, key.scope, key.entryId);
  }

  async reject(jobId: string, decision: RejectDecision): Promise<Result<PublishJob>> {
    const key = parseJobKey(jobId);
    if (!key) return failed<PublishJob>({ code: "not_found", message: `No job ${jobId} in the engine.` });
    const current = await engineCall<EngineEntry>("calendar.get", { scope: key.scope, entry_id: key.entryId });
    if (!current.ok) return this.failure<PublishJob>(current.error, key.scope, key.entryId);
    if (accountId(key.scope, current.value.platform) !== decision.accountId) {
      return failed<PublishJob>({ code: "account_mismatch", message: "This job belongs to another account. Nothing was rejected." });
    }
    const reply = await engineCall<EngineEntry>("calendar.reject", {
      scope: key.scope,
      entry_id: key.entryId,
      expected_revision: decision.revision,
      reason: encodeRejection(decision.code, decision.note),
    });
    return this.entryResult(reply, key.scope, key.entryId);
  }

  /**
   * Pauses one account. The engine's pause needs the revision the operator saw; the desktop reads the
   * current revision first, so the write is a compare-and-swap against that read. A revision that
   * moved in between is revision_conflict and nothing is paused.
   */
  async pauseAccount(id: string, reason: string): Promise<Result<Account>> {
    const key = parseAccountId(id);
    if (!key) return failed<Account>({ code: "not_found", message: `No account ${id} in the engine.` });
    const current = await engineCall<EngineAccount>("account.get", { scope: key.scope, platform: key.platform });
    if (!current.ok) return failed<Account>(dataErrorFrom(current.error));
    const reply = await engineCall<EngineAccount>("account.pause", {
      scope: key.scope,
      platform: key.platform,
      reason,
      expected_revision: current.value.revision,
    });
    return this.accountResult(reply);
  }

  /** Resumes one account, with the same revision read and compare-and-swap as pauseAccount. */
  async resumeAccount(id: string): Promise<Result<Account>> {
    const key = parseAccountId(id);
    if (!key) return failed<Account>({ code: "not_found", message: `No account ${id} in the engine.` });
    const current = await engineCall<EngineAccount>("account.get", { scope: key.scope, platform: key.platform });
    if (!current.ok) return failed<Account>(dataErrorFrom(current.error));
    const reply = await engineCall<EngineAccount>("account.resume", {
      scope: key.scope,
      platform: key.platform,
      expected_revision: current.value.revision,
    });
    return this.accountResult(reply);
  }

  /**
   * Disconnects one account with account.disconnect: the connection ends and the account's queued and waiting
   * publish jobs are cancelled in one engine transaction. Same revision read and compare-and-swap as pauseAccount.
   */
  async disconnectAccount(id: string): Promise<Result<AccountDisconnect>> {
    const key = parseAccountId(id);
    if (!key) return failed<AccountDisconnect>({ code: "not_found", message: `No account ${id} in the engine.` });
    const current = await engineCall<EngineAccount>("account.get", { scope: key.scope, platform: key.platform });
    if (!current.ok) return failed<AccountDisconnect>(dataErrorFrom(current.error));
    const reply = await engineCall<EngineAccountDisconnect>("account.disconnect", {
      scope: key.scope,
      platform: key.platform,
      expected_revision: current.value.revision,
    });
    if (!reply.ok) return failed<AccountDisconnect>(dataErrorFrom(reply.error));
    try {
      return {
        ok: true,
        value: {
          account: accountOf(reply.value.account, this.nowUtc()),
          cancelledJobs: reply.value.cancelled_jobs.length,
          keptJobs: reply.value.kept_jobs.length,
        },
      };
    } catch (error) {
      return failed<AccountDisconnect>({ code: "engine_error", message: messageOf(error), engineCode: "engine_mapping" });
    }
  }

  /**
   * The platform check is publish.reconcile, which is refused in the desktop role. It never reaches
   * the engine from here, so the desktop makes no platform call.
   */
  async reconcile(_jobId: string): Promise<Result<ReconcileResult>> {
    return failed<ReconcileResult>({ code: "engine_unavailable", message: DAEMON_ONLY_CHECK });
  }

  /**
   * Publish failures are resolved on the daemon (publish.reconcile and publish.cancel are role-gated).
   * Other failure kinds have no engine method yet.
   */
  async resolveFailure(failureId: string, _resolution: FailureResolution, _note: string): Promise<Result<FailureItem>> {
    if (failureId.startsWith("publish-")) {
      return failed<FailureItem>({ code: "engine_unavailable", message: DAEMON_ONLY_RESOLVE });
    }
    return unavailable<FailureItem>("Resolving a failure");
  }

  async resolveThread(_threadId: string): Promise<Result<ThreadItem>> {
    return unavailable<ThreadItem>("Resolving a thread");
  }

  async snoozeThread(_threadId: string, _untilUtc: string): Promise<Result<ThreadItem>> {
    return unavailable<ThreadItem>("Snoozing a thread");
  }

  // ---- Manual drafts --------------------------------------------------------------------------
  // the host app never posts a draft. These calls store and check it; the operator posts by hand and
  // records the link. The engine makes no network call for any of them.

  /** Every draft in every stored channel, from draft.list per scope, newest update first. */
  async listDrafts(): Promise<ManualDraft[]> {
    const scopes = await this.scopes();
    const lists = await Promise.all(scopes.map((scope) => read<EngineDraftList>("draft.list", { scope })));
    return lists
      .flatMap((list) => list.drafts.map((draft) => draftOf(draft)))
      .sort((a, b) => b.updatedAtUtc.localeCompare(a.updatedAtUtc) || a.id.localeCompare(b.id));
  }

  /** Approves exactly `revision` when the engine's gate passes. The disclosure phrases come from the caller. */
  async approveDraft(
    draftId: string,
    revision: number,
    disclosurePhrases: readonly string[],
  ): Promise<Result<ManualDraft>> {
    const key = parseDraftKey(draftId);
    if (!key) return failed<ManualDraft>({ code: "not_found", message: `No draft ${draftId} in the engine.` });
    const reply = await engineCall<EngineDraft>("draft.approve", {
      scope: key.scope,
      draft_id: key.draftId,
      expected_revision: revision,
      disclosure_phrases: [...disclosurePhrases],
    });
    return this.draftResult(reply);
  }

  /** Records the operator's post. The engine checks the link: https, on an allowed host for the platform. */
  async markDraftPosted(draftId: string, revision: number, postedUrl: string): Promise<Result<ManualDraft>> {
    const key = parseDraftKey(draftId);
    if (!key) return failed<ManualDraft>({ code: "not_found", message: `No draft ${draftId} in the engine.` });
    const reply = await engineCall<EngineDraftMarkPosted>("draft.mark_posted", {
      scope: key.scope,
      draft_id: key.draftId,
      expected_revision: revision,
      posted_url: postedUrl,
    });
    if (!reply.ok) return failed<ManualDraft>(dataErrorFrom(reply.error));
    return this.draftResult({ ok: true, value: reply.value.draft });
  }

  /** Ends a drafted or approved draft with a reason. The engine refuses a blank reason. */
  async skipDraft(draftId: string, revision: number, reason: string): Promise<Result<ManualDraft>> {
    const key = parseDraftKey(draftId);
    if (!key) return failed<ManualDraft>({ code: "not_found", message: `No draft ${draftId} in the engine.` });
    const reply = await engineCall<EngineDraft>("draft.skip", {
      scope: key.scope,
      draft_id: key.draftId,
      expected_revision: revision,
      reason,
    });
    return this.draftResult(reply);
  }

  // ---- Suggestions and spend ------------------------------------------------------------------
  // Suggestions are decided on the desktop: accept creates one draft calendar entry, dismiss creates
  // nothing. Spend is read-only. The engine's "no allowance" answers are states, so they return null.

  /** Every stored channel, from channel.list, labelled as venture / channel / account. */
  async listScopes(): Promise<OperatorScope[]> {
    const list = await read<EngineChannelList>("channel.list", {});
    return list.channels.map((row) => ({
      id: accountKey(row.scope),
      label: `${row.scope.venture} / ${row.scope.channel} / ${row.scope.account}`,
    }));
  }

  /** Every suggestion in one scope, from optimise.list, in the engine's creation order. */
  async listProposals(scopeId: string): Promise<OptimiseProposal[]> {
    const list = await read<EngineOptimiseList>("optimise.list", { scope: scopeOf(scopeId) });
    return list.proposals.map((proposal) => proposalOf(proposal));
  }

  /** One suggestion with its lineage, from optimise.get. Null when the engine does not hold it in this scope. */
  async getProposal(scopeId: string, proposalId: string): Promise<OptimiseProposal | null> {
    const reply = await engineCall<EngineOptimiseProposal>("optimise.get", {
      scope: scopeOf(scopeId),
      proposal_id: proposalId,
    });
    if (!reply.ok) {
      if (reply.error.code === "not_found") return null;
      throw new EngineReadError(reply.error.code, `optimise.get: ${reply.error.message}`);
    }
    return proposalOf(reply.value);
  }

  /** Accepts at exactly `expectedRevision`. The engine creates the draft entry; nothing is approved here. */
  async acceptProposal(scopeId: string, proposalId: string, expectedRevision: number): Promise<Result<OptimiseProposal>> {
    return this.decideProposal("optimise.accept", scopeId, proposalId, expectedRevision);
  }

  /** Dismisses at exactly `expectedRevision`. The engine writes only the suggestion row. */
  async dismissProposal(scopeId: string, proposalId: string, expectedRevision: number): Promise<Result<OptimiseProposal>> {
    return this.decideProposal("optimise.dismiss", scopeId, proposalId, expectedRevision);
  }

  /** The month's allowance, from allowance.get. The engine's not_found is "no allowance set", so it returns null. */
  async getAllowance(scopeId: string, month: string): Promise<SpendAllowance | null> {
    const reply = await engineCall<EngineSpendAllowance>("allowance.get", { scope: scopeOf(scopeId), month });
    if (!reply.ok) {
      if (reply.error.code === "not_found") return null;
      throw new EngineReadError(reply.error.code, `allowance.get: ${reply.error.message}`);
    }
    return allowanceOf(reply.value);
  }

  /** The month report, from spend.report. The engine's allowance_missing is "no allowance set", so it returns null. */
  async spendReport(scopeId: string, month: string): Promise<SpendReport | null> {
    const reply = await engineCall<EngineSpendReport>("spend.report", { scope: scopeOf(scopeId), month });
    if (!reply.ok) {
      if (reply.error.code === "allowance_missing") return null;
      throw new EngineReadError(reply.error.code, `spend.report: ${reply.error.message}`);
    }
    return spendReportOf(reply.value);
  }

  /** The latest version of every price route, from price.list, with qualification at the browser clock. */
  async listPrices(): Promise<SpendPrice[]> {
    const list = await read<EngineSpendPriceList>("price.list", { now: this.nowUtc() });
    return list.prices.map((price) => priceOf(price));
  }

  // ---- Campaigns, tracked links and hook tests (read only) ----------------------------------
  // The desktop never calls campaign.put, link.create, link.record_visit, hook_test.create, bind or read.

  /** Every campaign in one scope, from campaign.list. */
  async listCampaigns(scopeId: string): Promise<Campaign[]> {
    const list = await read<EngineCampaignList>("campaign.list", { scope: scopeOf(scopeId) });
    return list.campaigns.map((campaign: EngineCampaign) => campaignOf(campaign));
  }

  /** The tracked links of one campaign, from link.list filtered by campaign id. */
  async listCampaignLinks(scopeId: string, campaignId: string): Promise<TrackedLink[]> {
    const list = await read<EngineLinkList>("link.list", { scope: scopeOf(scopeId), campaign_id: campaignId });
    return list.links.map((link) => linkOf(link));
  }

  /** The visit counts of one campaign, from link.attribution. Null when the engine holds no such campaign in the scope. */
  async campaignAttribution(scopeId: string, campaignId: string): Promise<CampaignAttribution | null> {
    const reply = await engineCall<EngineLinkAttribution>("link.attribution", {
      scope: scopeOf(scopeId),
      campaign_id: campaignId,
    });
    if (!reply.ok) {
      if (reply.error.code === "not_found") return null;
      throw new EngineReadError(reply.error.code, `link.attribution: ${reply.error.message}`);
    }
    return attributionOf(reply.value);
  }

  /**
   * Every tracked link in one scope. campaign.list gives the campaigns; for each one, link.list and link.attribution
   * are read together. Both are read-only, and any failed read throws, so the screen shows its load error.
   */
  async listChannelLinks(scopeId: string): Promise<ChannelLink[]> {
    const scope = scopeOf(scopeId);
    const campaigns = await read<EngineCampaignList>("campaign.list", { scope });
    const perCampaign = await Promise.all(
      campaigns.campaigns.map(async (campaign) => {
        const [links, attribution] = await Promise.all([
          read<EngineLinkList>("link.list", { scope, campaign_id: campaign.campaign_id }),
          read<EngineLinkAttribution>("link.attribution", { scope, campaign_id: campaign.campaign_id }),
        ]);
        return channelLinksOf(campaign.name, links.links, attribution);
      }),
    );
    return perCampaign.flat();
  }

  /** Every hook test set in one scope, from hook_test.list. Reading the sets here decides nothing. */
  async listHookTests(scopeId: string): Promise<HookTestSet[]> {
    const list = await read<EngineHookTestList>("hook_test.list", { scope: scopeOf(scopeId) });
    return list.sets.map((set) => hookTestOf(set));
  }

  /** One hook test set with its openings, from hook_test.get. Null when no set with this id is in the scope. */
  async getHookTest(scopeId: string, setId: string): Promise<HookTestSet | null> {
    const reply = await engineCall<EngineHookTestSet>("hook_test.get", { scope: scopeOf(scopeId), set_id: setId });
    if (!reply.ok) {
      if (reply.error.code === "not_found") return null;
      throw new EngineReadError(reply.error.code, `hook_test.get: ${reply.error.message}`);
    }
    return hookTestOf(reply.value);
  }

  // ---- Research digests and content library (read only) --------------------------------------
  // The desktop never calls digest.put, digest.plan, digest.apply, content.put, content.withdraw,
  // content.add_variant, content.plan_occurrence, content.cancel_occurrence or content.mark_published.

  /** Every digest in one scope, from digest.list. */
  async listDigests(scopeId: string): Promise<ResearchDigest[]> {
    const list = await read<EngineDigestList>("digest.list", { scope: scopeOf(scopeId) });
    return list.digests.map((digest: EngineDigestRecord) => digestOf(digest));
  }

  /** One digest with its entries, from digest.get. Null when no digest with this id is in the scope. */
  async getDigest(scopeId: string, digestId: string): Promise<ResearchDigest | null> {
    const reply = await engineCall<EngineDigestRecord>("digest.get", {
      scope: scopeOf(scopeId),
      digest_id: digestId,
    });
    if (!reply.ok) {
      if (reply.error.code === "not_found") return null;
      throw new EngineReadError(reply.error.code, `digest.get: ${reply.error.message}`);
    }
    return digestOf(reply.value);
  }

  /** Which digest entries became draft calendar entries, from digest.applied. Null when the digest is not in the scope. */
  async digestApplications(scopeId: string, digestId: string): Promise<DigestApplication[] | null> {
    const reply = await engineCall<EngineDigestApplied>("digest.applied", {
      scope: scopeOf(scopeId),
      digest_id: digestId,
    });
    if (!reply.ok) {
      if (reply.error.code === "not_found") return null;
      throw new EngineReadError(reply.error.code, `digest.applied: ${reply.error.message}`);
    }
    return reply.value.applications.map((application) => digestApplicationOf(application, reply.value.scope));
  }

  /** Every content record in one scope, from content.list, read at the browser clock so expiry is derived. */
  async listContent(scopeId: string): Promise<ContentRecord[]> {
    const list = await read<EngineContentList>("content.list", { scope: scopeOf(scopeId), now: this.nowUtc() });
    return list.records.map((record) => contentRecordOf(record));
  }

  /** One record with its variants, from content.get. Null when no record with this id is in the scope. */
  async getContent(scopeId: string, contentId: string): Promise<ContentDetail | null> {
    const reply = await engineCall<EngineContentGet>("content.get", {
      scope: scopeOf(scopeId),
      content_id: contentId,
      now: this.nowUtc(),
    });
    if (!reply.ok) {
      if (reply.error.code === "not_found") return null;
      throw new EngineReadError(reply.error.code, `content.get: ${reply.error.message}`);
    }
    return contentDetailOf(reply.value);
  }

  /** The variant comparison for one record, from content.experiment_summary. Null when the record is not in the scope. */
  async contentExperimentSummary(scopeId: string, contentId: string): Promise<ContentExperimentSummary | null> {
    const reply = await engineCall<EngineContentExperimentSummary>("content.experiment_summary", {
      scope: scopeOf(scopeId),
      content_id: contentId,
    });
    if (!reply.ok) {
      if (reply.error.code === "not_found") return null;
      throw new EngineReadError(reply.error.code, `content.experiment_summary: ${reply.error.message}`);
    }
    return experimentSummaryOf(reply.value);
  }

  // ---- Text gate, briefs, deliveries and dispatch attempts (read only) -------------------------
  // The desktop never calls gate_policy.put, sends record: true, or calls brief_out.put, delivery.accept or
  // dispatch.run. The check below asks for no record and carries no claim annotations.

  /** The latest gate policy, from gate_policy.get. Null when the engine answers policy_missing: nothing can pass. */
  async gatePolicy(scopeId: string): Promise<GatePolicy | null> {
    const reply = await engineCall<EngineGatePolicy>("gate_policy.get", { scope: scopeOf(scopeId) });
    if (!reply.ok) {
      if (reply.error.code === "policy_missing") return null;
      throw new EngineReadError(reply.error.code, `gate_policy.get: ${reply.error.message}`);
    }
    return gatePolicyOf(reply.value);
  }

  /**
   * A dry-run check of one script, from gate.check. Nothing is stored. A refusal (hidden characters, no
   * policy) comes back as a Result with the engine's code, so the screen shows it as a notice.
   */
  async checkText(scopeId: string, subjectId: string, text: string): Promise<Result<GateCheck>> {
    const scope = parseScopeKey(scopeId);
    if (!scope) return failed<GateCheck>({ code: "not_found", message: `No channel ${scopeId} in the engine.` });
    const reply = await engineCall<EngineGateCheck>("gate.check", {
      scope,
      subject_id: subjectId,
      text,
      claims: [],
      record: false,
    });
    if (!reply.ok) return failed<GateCheck>(dataErrorFrom(reply.error));
    try {
      return { ok: true, value: gateCheckOf(reply.value) };
    } catch (error) {
      return failed<GateCheck>({ code: "engine_error", message: messageOf(error), engineCode: "engine_mapping" });
    }
  }

  /** The stored verdict records of one subject, newest first, from gate.records. */
  async gateRecords(scopeId: string, subjectId: string): Promise<GateRecord[]> {
    const list = await read<EngineGateRecords>("gate.records", { scope: scopeOf(scopeId), subject_id: subjectId });
    return list.records.map((record) => gateRecordOf(record, scopeId));
  }

  /** Every outgoing brief in one scope, from brief_out.list. */
  async listBriefs(scopeId: string): Promise<OutgoingBrief[]> {
    const list = await read<EngineBriefOutList>("brief_out.list", { scope: scopeOf(scopeId) });
    return list.briefs.map((brief) => briefOf(brief));
  }

  /** One brief with its summary and document, from brief_out.get. Null when no brief has this request id in the scope. */
  async getBrief(scopeId: string, requestId: string): Promise<OutgoingBriefDetail | null> {
    const reply = await engineCall<EngineBriefOut>("brief_out.get", {
      scope: scopeOf(scopeId),
      request_id: requestId,
    });
    if (!reply.ok) {
      if (reply.error.code === "not_found") return null;
      throw new EngineReadError(reply.error.code, `brief_out.get: ${reply.error.message}`);
    }
    return briefDetailOf(reply.value);
  }

  /** Every accepted delivery in one scope, from delivery.list. */
  async listDeliveries(scopeId: string): Promise<DeliveryEnvelope[]> {
    const list = await read<EngineDeliveryList>("delivery.list", { scope: scopeOf(scopeId) });
    return list.deliveries.map((delivery) => deliveryOf(delivery));
  }

  /** One delivery envelope, from delivery.get. Null when no envelope has this id in the scope. */
  async getDelivery(scopeId: string, envelopeId: string): Promise<DeliveryEnvelope | null> {
    const reply = await engineCall<EngineDeliveryRecord>("delivery.get", {
      scope: scopeOf(scopeId),
      envelope_id: envelopeId,
    });
    if (!reply.ok) {
      if (reply.error.code === "not_found") return null;
      throw new EngineReadError(reply.error.code, `delivery.get: ${reply.error.message}`);
    }
    return deliveryOf(reply.value);
  }

  /** The dispatch attempts of one brief, from dispatch.list filtered by request id, oldest first. */
  async listDispatchAttempts(scopeId: string, requestId: string): Promise<DispatchAttempt[]> {
    const list = await read<EngineDispatchList>("dispatch.list", { scope: scopeOf(scopeId), request_id: requestId });
    return list.attempts.map((attempt) => dispatchAttemptOf(attempt, scopeId));
  }

  // ---- Channel memory and the proof bank (read only) -----------------------------------------
  // The desktop never calls memory.put, channel.import or proof.apply_history.

  /** The latest memory revision with its document, from memory.get. Null when the engine answers not_found: nothing written yet. */
  async channelMemory(scopeId: string): Promise<ChannelMemoryDetail | null> {
    const reply = await engineCall<EngineMemoryGet>("memory.get", { scope: scopeOf(scopeId) });
    if (!reply.ok) {
      if (reply.error.code === "not_found") return null;
      throw new EngineReadError(reply.error.code, `memory.get: ${reply.error.message}`);
    }
    return memoryDetailOf(reply.value);
  }

  /** Every stored memory revision, newest first, from memory.history. An empty list when nothing is stored. */
  async channelMemoryHistory(scopeId: string): Promise<ChannelMemoryRevision[]> {
    const list = await read<EngineMemoryHistory>("memory.history", { scope: scopeOf(scopeId) });
    return list.revisions.map((row) => memoryRevisionOf(row, scopeId));
  }

  /** The proofs of one scope, from proof.list. A null status sends no status filter, so every status is listed. */
  async listProofs(scopeId: string, status: ProofStatus | null): Promise<ProofRecord[]> {
    const scope = scopeOf(scopeId);
    const params = status === null ? { scope } : { scope, status };
    const list = await read<EngineProofList>("proof.list", params);
    return list.proofs.map((row) => proofOf(row));
  }

  // ---- Typed briefs and platform variants (read only) -----------------------------------------
  // The desktop never calls typed_brief.put, variant.derive or variant.derive; rules come from the platform-rules catalog.

  /** Every typed brief in one scope, from typed_brief.list, ordered by brief id. */
  async listTypedBriefs(scopeId: string): Promise<TypedBrief[]> {
    const list = await read<EngineTypedBriefList>("typed_brief.list", { scope: scopeOf(scopeId) });
    return list.briefs.map((brief) => typedBriefOf(brief));
  }

  /** One brief, from typed_brief.get. Null when the engine answers not_found: no brief with this id in the scope. */
  async getTypedBrief(scopeId: string, briefId: string): Promise<TypedBrief | null> {
    const reply = await engineCall<EngineTypedBrief>("typed_brief.get", {
      scope: scopeOf(scopeId),
      brief_id: briefId,
    });
    if (!reply.ok) {
      if (reply.error.code === "not_found") return null;
      throw new EngineReadError(reply.error.code, `typed_brief.get: ${reply.error.message}`);
    }
    return typedBriefOf(reply.value);
  }

  /** The variants of one brief, from variant.list, ordered by platform then variant id. */
  async listBriefVariants(scopeId: string, briefId: string): Promise<BriefVariant[]> {
    const list = await read<EngineVariantList>("variant.list", { scope: scopeOf(scopeId), brief_id: briefId });
    return list.variants.map((variant) => briefVariantOf(variant));
  }

  // ---- Analytics and manual imports (read only) -----------------------------------------------
  // The desktop never calls analytics.pull, metric_import.preview or metric_import.commit. Numbers are pulled on
  // the daemon; this reads what is stored.

  /** Every published post in one scope with its latest metrics, from analytics.summary. */
  async listAnalyticsPosts(scopeId: string): Promise<AnalyticsPost[]> {
    const summary = await read<EngineAnalyticsSummary>("analytics.summary", { scope: scopeOf(scopeId) });
    return analyticsPostsOf(summary);
  }

  /** The stored observations of one post, from analytics.list filtered to its job. The engine orders them oldest first. */
  async listAnalyticsObservations(scopeId: string, jobId: string): Promise<AnalyticsObservation[]> {
    const list = await read<EngineAnalyticsObservations>("analytics.list", {
      scope: scopeOf(scopeId),
      job_id: jobId,
    });
    return list.observations.map((observation) => analyticsObservationOf(observation, scopeId));
  }

  /** Every committed manual import in one scope, from metric_import.list. */
  async listMetricImports(scopeId: string): Promise<MetricImport[]> {
    const list = await read<EngineMetricImportList>("metric_import.list", { scope: scopeOf(scopeId) });
    return list.imports.map((record) => metricImportOf(record));
  }

  // ---- Store health (read only) ---------------------------------------------------------------
  // The desktop never calls heartbeat.beat, store.backup or store.restore. The engine reads the clock only from `now`.

  /** The heartbeat record's state at `nowUtc`, from heartbeat.status. Ages and states are the engine's. */
  async heartbeatStatus(nowUtc: string, staleAfterSecs: number): Promise<HeartbeatStatus> {
    const engine = await read<EngineHeartbeatStatus>("heartbeat.status", {
      now: nowUtc,
      stale_after_secs: staleAfterSecs,
    });
    return heartbeatStatusOf(engine);
  }

  /** Every restore receipt, newest first, from store.restore_receipts. */
  async listRestoreReceipts(): Promise<RestoreReceiptList> {
    const engine = await read<EngineRestoreReceiptList>("store.restore_receipts", {});
    return restoreReceiptListOf(engine);
  }

  // ---- Webhook inbox (read only) ---------------------------------------------------------------
  // The desktop reads the events the daemon stored. It never receives an event, sets its state or tests a signature.

  /** The newest page of events of one channel account, or of the unmatched events when scopeId is null. */
  async listInbox(scopeId: string | null): Promise<InboxListing> {
    const params =
      scopeId === null ? { unmatched: true, limit: INBOX_PAGE } : { scope: scopeOf(scopeId), limit: INBOX_PAGE };
    const list = await read<EngineInboxList>("inbox.list", params);
    return {
      rows: list.events.map((event) => rowOf(event, () => inboxEventOf(event))),
      moreAvailable: list.next_before !== null,
    };
  }

  /** The raw item of one event. Null when the engine holds no event with this id in the scope (not_found). */
  async getInboxEvent(scopeId: string | null, eventId: string): Promise<InboxRaw | null> {
    const params = scopeId === null ? { event_id: eventId } : { event_id: eventId, scope: scopeOf(scopeId) };
    const reply = await engineCall<EngineInboxGet>("inbox.get", params);
    if (!reply.ok) {
      if (reply.error.code === "not_found") return null;
      throw new EngineReadError(reply.error.code, `inbox.get: ${reply.error.message}`);
    }
    return inboxRawOf(reply.value);
  }

  /** Event and delivery counts from inbox.stats. The engine takes no input and keeps no per-channel refusal count. */
  async inboxStats(): Promise<InboxStats> {
    return inboxStatsOf(await read<EngineInboxStats>("inbox.stats", {}));
  }

  // ---- Publish pacing (read only) --------------------------------------------------------------
  // publish.next and publish.quota_get decide and report; the desktop never queues, runs or changes a limit.

  /** publish.next for every account of one channel, decided at the browser clock. Every row is kept. */
  async listPublishNext(scopeId: string): Promise<PublishNextView> {
    const scope = scopeOf(scopeId);
    const reply = await read<EnginePublishNext>("publish.next", {
      scope: { venture: scope.venture, channel: scope.channel },
      now: this.nowUtc(),
    });
    return publishNextOf(reply);
  }

  /** publish.quota_get for one account, read at the browser clock. A failed read throws; the screen shows it for that account. */
  async getPublishQuota(scopeId: string, platform: string, remoteAccount: string): Promise<PublishQuota> {
    const record = await read<EnginePublishQuota>("publish.quota_get", {
      scope: scopeOf(scopeId),
      platform,
      remote_account: remoteAccount,
      now: this.nowUtc(),
    });
    return publishQuotaOf(record);
  }

  // ---- Internal-------------------------------------------------------------------------------

  /** An accept or dismiss: the engine's reply as a Result. Revision and state errors come back as typed codes. */
  private async decideProposal(
    method: "optimise.accept" | "optimise.dismiss",
    scopeId: string,
    proposalId: string,
    expectedRevision: number,
  ): Promise<Result<OptimiseProposal>> {
    const scope = parseScopeKey(scopeId);
    if (!scope) return failed<OptimiseProposal>({ code: "not_found", message: `No channel ${scopeId} in the engine.` });
    const reply = await engineCall<EngineOptimiseProposal>(method, {
      scope,
      proposal_id: proposalId,
      expected_revision: expectedRevision,
      now: this.nowUtc(),
    });
    if (!reply.ok) return failed<OptimiseProposal>(dataErrorFrom(reply.error));
    try {
      return { ok: true, value: proposalOf(reply.value) };
    } catch (error) {
      return failed<OptimiseProposal>({ code: "engine_error", message: messageOf(error), engineCode: "engine_mapping" });
    }
  }

  /** Every stored channel's scope, from channel.list. Scope is the only key the engine has. */
  private async scopes(): Promise<EngineScope[]> {
    const list = await read<EngineChannelList>("channel.list", {});
    return list.channels.map((row) => row.scope);
  }

  /**
   * The publish jobs in one scope, from publish.list. Reads run in every engine role. Any engine error
   * throws, so the screen shows its load error rather than an empty list.
   */
  private async publishJobsFor(scope: EngineScope): Promise<EnginePublishJob[]> {
    const list = await read<EnginePublishList>("publish.list", { scope });
    return list.jobs;
  }

  /** An account write's reply: the stored account as an Account, or the mapped engine error. */
  private accountResult(reply: EngineCallResult<EngineAccount>): Result<Account> {
    if (!reply.ok) return failed<Account>(dataErrorFrom(reply.error));
    try {
      return { ok: true, value: accountOf(reply.value, this.nowUtc()) };
    } catch (error) {
      return failed<Account>({ code: "engine_error", message: messageOf(error), engineCode: "engine_mapping" });
    }
  }

  /** A draft write's reply: the stored draft as a ManualDraft, or the mapped engine error. */
  private draftResult(reply: EngineCallResult<EngineDraft>): Result<ManualDraft> {
    if (!reply.ok) return failed<ManualDraft>(dataErrorFrom(reply.error));
    try {
      return { ok: true, value: draftOf(reply.value) };
    } catch (error) {
      return failed<ManualDraft>({ code: "engine_error", message: messageOf(error), engineCode: "engine_mapping" });
    }
  }

  /** A calendar write's reply: the stored entry as a job, or the mapped engine error. */
  private async entryResult(
    reply: EngineCallResult<EngineEntry>,
    scope: EngineScope,
    entryId: string,
  ): Promise<Result<PublishJob>> {
    if (!reply.ok) return this.failure<PublishJob>(reply.error, scope, entryId);
    try {
      return { ok: true, value: entryToJob(reply.value) };
    } catch (error) {
      return failed<PublishJob>({ code: "engine_error", message: messageOf(error), engineCode: "engine_mapping" });
    }
  }

  /** The mapped error for a failed call. A revision conflict reads the current revision from the engine. */
  private async failure<T>(error: EngineCallError, scope: EngineScope, entryId: string): Promise<Result<T>> {
    const mapped = dataErrorFrom(error);
    if (mapped.code !== "revision_conflict") return failed<T>(mapped);
    const now = await engineCall<EngineEntry>("calendar.get", { scope, entry_id: entryId });
    return failed<T>({ ...mapped, currentRevision: now.ok ? now.value.revision : undefined });
  }
}

/** Reads throw on any engine error, so the screen shows its load error state. */
async function read<T>(method: UiMethod, params: Record<string, unknown>): Promise<T> {
  const reply = await engineCall<T>(method, params);
  if (!reply.ok) throw new EngineReadError(reply.error.code, `${method}: ${reply.error.message}`);
  return reply.value;
}

/** A scope id as the engine's scope. A malformed id is a read error, not a silent empty result. */
function scopeOf(id: string): EngineScope {
  const scope = parseScopeKey(id);
  if (!scope) throw new EngineReadError("not_found", `No channel ${id} in the engine.`);
  return scope;
}
