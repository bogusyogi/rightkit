// RIGHTKIT-PENDING #383: ported version saved in /Volumes/D/scratch/postright/port-wip/full-port.patch
import type { DataSource, MockControls, Result } from "../source";
import type {
  Account,
  AccountDisconnect,
  Campaign,
  CampaignAttribution,
  ChannelLink,
  DataErrorCode,
  DateRange,
  Digest,
  FailureItem,
  FailureResolution,
  HookTestSet,
  ManualDraft,
  ModerationCapability,
  OperatorScope,
  OptimiseProposal,
  PublishJob,
  PublishJobRecord,
  ReconcileResult,
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
  GateRuleFinding,
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
  AnalyticsSummaryMetric,
  MetricImport,
  HeartbeatStatus,
  RestoreReceiptList,
  InboxListing,
  InboxRaw,
  InboxRow,
  InboxStats,
  PublishNextView,
  PublishQuota,
} from "../types";
import { awaitingDecision, mediaReady } from "../derive";
import { disclosureFound } from "../draftRules";
import { FIXTURE_ACCOUNTS, FIXTURE_JOBS, MOCK_NOW_UTC, MOCK_POLICY_REVISION } from "./fixtures";
import { FIXTURE_CAMPAIGNS, FIXTURE_LINKS, FIXTURE_LINK_VISITS } from "./fixtures-links";
import { FIXTURE_DRAFTS } from "./fixtures-drafts";
import { FIXTURE_HOOK_TESTS } from "./fixtures-hooks";
import { FIXTURE_DIGESTS, FIXTURE_DIGEST_APPLICATIONS } from "./fixtures-digests";
import { FIXTURE_GATE_POLICIES, FIXTURE_GATE_RECORDS } from "./fixtures-gate";
import { FIXTURE_BRIEFS, FIXTURE_DELIVERIES, FIXTURE_DISPATCH_ATTEMPTS } from "./fixtures-briefs";
import { FIXTURE_MEMORY, FIXTURE_PROOFS } from "./fixtures-memory";
import { FIXTURE_BRIEF_VARIANTS, FIXTURE_TYPED_BRIEFS } from "./fixtures-variants";
import {
  FIXTURE_ANALYTICS_OBSERVATIONS,
  FIXTURE_ANALYTICS_POSTS,
  FIXTURE_METRIC_IMPORTS,
} from "./fixtures-analytics";
import { FIXTURE_HEARTBEAT, FIXTURE_RESTORE_RECEIPTS } from "./fixtures-store";
import { FIXTURE_PUBLISH_JOBS } from "./fixtures-publish";
import { FIXTURE_INBOX, FIXTURE_INBOX_STATS, FIXTURE_UNMATCHED_UNREADABLE } from "./fixtures-inbox";
import { pacingViewFor, quotaFor } from "./fixtures-pacing";
import { FIXTURE_CONTENT, FIXTURE_CONTENT_SUMMARIES, FIXTURE_CONTENT_VARIANTS } from "./fixtures-content";
import {
  FIXTURE_ALLOWANCES,
  FIXTURE_PRICES,
  FIXTURE_PROPOSALS,
  FIXTURE_REPORTS,
  FIXTURE_SCOPES,
} from "./fixtures-optimise";
import {
  EXTRA_JOBS,
  FIXTURE_FAILURES,
  FIXTURE_MODERATION,
  FIXTURE_THREADS,
  RECONCILE_RESULTS,
} from "./fixtures-ops";

export interface MockOptions {
  /** Simulated network latency for every call. Default 220 ms, so loading states are visible. */
  latencyMs?: number;
  /** When true, every read rejects. Lets the error state be exercised. */
  failReads?: boolean;
}

/** FNV-1a, 32 bit. A display fingerprint for the mock, not a cryptographic digest. */
export function fingerprint(text: string): string {
  let hash = 0x811c9dc5;
  for (let i = 0; i < text.length; i += 1) {
    hash ^= text.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash.toString(16).padStart(8, "0");
}

const clone = <T>(value: T): T => structuredClone(value);

/** The links of one campaign in one scope, in the engine's order: variant, platform, then link id. */
function sortedLinks(scopeId: string, campaignId: string): TrackedLink[] {
  return FIXTURE_LINKS.filter((l) => l.scopeId === scopeId && l.campaignId === campaignId).sort(
    (a, b) =>
      a.variantId.localeCompare(b.variantId) || a.platform.localeCompare(b.platform) || a.id.localeCompare(b.id),
  );
}
const DAY_MS = 86_400_000;

const ALERT_LABEL: Record<string, string> = {
  disk_full: "disk space",
  credentials_expired: "expired credentials",
  quota_cooldown: "quota cooldown",
};

/**
 * In-memory daemon stand-in. It enforces the daemon's rules on every write: a mismatched account
 * or revision returns a typed error and changes nothing. Data resets on each page load.
 */
export class MockDataSource implements DataSource, MockControls {
  readonly kind = "mock" as const;
  readonly mock: MockControls;

  private readonly latencyMs: number;
  private readonly failReads: boolean;
  private readonly accounts: Account[];
  private readonly jobs: Map<string, PublishJob>;
  private readonly failures: Map<string, FailureItem>;
  private readonly threads: Map<string, ThreadItem>;
  private readonly drafts: Map<string, ManualDraft>;
  private readonly proposals: Map<string, OptimiseProposal>;

  constructor(options: MockOptions = {}) {
    this.latencyMs = options.latencyMs ?? 220;
    this.failReads = options.failReads ?? false;
    this.accounts = clone(FIXTURE_ACCOUNTS);
    this.jobs = new Map(
      clone([...FIXTURE_JOBS, ...EXTRA_JOBS]).map((job) => {
        job.captionDigest = fingerprint(job.caption);
        return [job.id, job] as const;
      }),
    );
    this.failures = new Map(clone(FIXTURE_FAILURES).map((f) => [f.id, f] as const));
    this.threads = new Map(clone(FIXTURE_THREADS).map((t) => [t.id, t] as const));
    this.drafts = new Map(
      clone(FIXTURE_DRAFTS).map((draft) => {
        draft.bodyChars = Array.from(draft.body).length;
        return [draft.id, draft] as const;
      }),
    );
    this.proposals = new Map(clone(FIXTURE_PROPOSALS).map((p) => [p.id, p] as const));
    this.mock = this;
  }

  nowUtc(): string {
    return MOCK_NOW_UTC;
  }

  listAccounts(): Promise<Account[]> {
    return this.read(() => clone(this.accounts));
  }

  listJobs(range: DateRange): Promise<PublishJob[]> {
    return this.read(() =>
      clone(
        [...this.jobs.values()]
          .filter((j) => j.scheduledAtUtc >= range.fromUtc && j.scheduledAtUtc < range.toUtc)
          .sort((a, b) => a.scheduledAtUtc.localeCompare(b.scheduledAtUtc)),
      ),
    );
  }

  getJob(jobId: string): Promise<PublishJob | null> {
    return this.read(() => {
      const job = this.jobs.get(jobId);
      return job ? clone(job) : null;
    });
  }

  async approve(jobId: string, revision: number, accountId: string): Promise<Result<PublishJob>> {
    await this.delay();
    const job = this.jobs.get(jobId);
    if (!job) return fail("not_found", "This job no longer exists. Nothing changed.");
    if (job.accountId !== accountId) {
      return fail("account_mismatch", "The job belongs to a different account. Nothing changed.");
    }
    if (job.revision !== revision) {
      return {
        ok: false,
        error: {
          code: "revision_conflict",
          message: `This job is now revision ${job.revision}, not ${revision}. Review the new version before approving.`,
          currentRevision: job.revision,
        },
      };
    }
    if (job.approval.status !== "pending" && job.approval.status !== "invalidated") {
      return fail("not_approvable", `Nothing to approve: the approval is ${job.approval.status}.`);
    }
    if (!mediaReady(job)) {
      return fail("not_approvable", "Media not ready on server. Approve again once every item is ready.");
    }
    job.approval = {
      status: "approved",
      approvedRevision: job.revision,
      policyRevision: MOCK_POLICY_REVISION,
      approvedAtUtc: MOCK_NOW_UTC,
    };
    job.policyRevision = MOCK_POLICY_REVISION;
    job.state = this.isPaused(accountId) ? "paused" : "scheduled";
    return { ok: true, value: clone(job) };
  }

  async reject(jobId: string, decision: RejectDecision): Promise<Result<PublishJob>> {
    await this.delay();
    const job = this.jobs.get(jobId);
    if (!job) return fail("not_found", "This job no longer exists. Nothing changed.");
    if (job.accountId !== decision.accountId) {
      return fail("account_mismatch", "The job belongs to a different account. Nothing changed.");
    }
    if (job.revision !== decision.revision) {
      return {
        ok: false,
        error: {
          code: "revision_conflict",
          message: `This job is now revision ${job.revision}, not ${decision.revision}. Review the new version before rejecting.`,
          currentRevision: job.revision,
        },
      };
    }
    if (decision.code === "other" && decision.note.trim() === "") {
      return fail("reason_required", "Add a note when the reason is Other.");
    }
    if (job.approval.status === "rejected") {
      return fail("not_approvable", "This job is already rejected.");
    }
    job.approval = {
      status: "rejected",
      reason: decision.code,
      note: decision.note.trim() === "" ? null : decision.note.trim(),
    };
    job.revision += 1;
    job.state = "draft";
    return { ok: true, value: clone(job) };
  }

  async pauseAccount(accountId: string, reason: string): Promise<Result<Account>> {
    await this.delay();
    const account = this.accounts.find((a) => a.id === accountId);
    if (!account) return fail("not_found", "This account is not known to the daemon.");
    account.readiness.paused = { by: "desktop", atUtc: MOCK_NOW_UTC, reason: reason.trim() || "Paused from desktop" };
    for (const job of this.jobs.values()) {
      if (job.accountId === accountId && job.state === "scheduled") job.state = "paused";
    }
    return { ok: true, value: clone(account) };
  }

  async resumeAccount(accountId: string): Promise<Result<Account>> {
    await this.delay();
    const account = this.accounts.find((a) => a.id === accountId);
    if (!account) return fail("not_found", "This account is not known to the daemon.");
    account.readiness.paused = null;
    for (const job of this.jobs.values()) {
      if (job.accountId === accountId && job.state === "paused") {
        job.state = job.approval.status === "approved" ? "scheduled" : "ready";
      }
    }
    return { ok: true, value: clone(account) };
  }

  async disconnectAccount(accountId: string): Promise<Result<AccountDisconnect>> {
    await this.delay();
    const account = this.accounts.find((a) => a.id === accountId);
    if (!account) return fail("not_found", "This account is not known to the daemon.");
    if (account.readiness.token.status === "revoked") {
      return fail("not_approvable", "This account is already disconnected.");
    }
    account.readiness.token = { status: "revoked" };
    // The engine cancels the publish jobs; the mock has no job state for that, so scheduled posts read as paused.
    let cancelledJobs = 0;
    for (const job of this.jobs.values()) {
      if (job.accountId === accountId && job.state === "scheduled") {
        job.state = "paused";
        cancelledJobs += 1;
      }
    }
    return { ok: true, value: { account: clone(account), cancelledJobs, keptJobs: 0 } };
  }

  async simulateCaptionChange(jobId: string, caption: string): Promise<Result<PublishJob>> {
    await this.delay();
    const job = this.jobs.get(jobId);
    if (!job) return fail("not_found", "This job no longer exists. Nothing changed.");
    job.caption = caption;
    job.captionDigest = fingerprint(caption);
    job.revision += 1;
    if (job.approval.status === "approved") {
      job.approval = {
        status: "invalidated",
        approvedRevision: job.approval.approvedRevision,
        policyRevision: job.approval.policyRevision,
        cause: "caption_changed",
      };
      job.state = "draft";
    }
    return { ok: true, value: clone(job) };
  }

  // ---- Failures and digest -------------------------------------------------------------

  listFailures(): Promise<FailureItem[]> {
    return this.read(() => clone([...this.failures.values()]));
  }

  async reconcile(jobId: string): Promise<Result<ReconcileResult>> {
    await this.delay();
    const failure = [...this.failures.values()].find((f) => f.jobId === jobId && f.kind === "outcome_unknown");
    const make = RECONCILE_RESULTS[jobId];
    if (!failure || !make) return fail("not_approvable", "This job has no unknown outcome to check.");
    if (failure.status === "resolved") return fail("not_approvable", "This outcome is already resolved.");
    const result = make(MOCK_NOW_UTC);
    failure.reconcile = result;
    return { ok: true, value: clone(result) };
  }

  async resolveFailure(
    failureId: string,
    resolution: FailureResolution,
    note: string,
  ): Promise<Result<FailureItem>> {
    await this.delay();
    const failure = this.failures.get(failureId);
    if (!failure) return fail("not_found", "This failure is no longer open. Nothing changed.");
    if (failure.status === "resolved") return fail("not_approvable", "This failure is already resolved.");
    const trimmed = note.trim();

    switch (failure.kind) {
      case "alert":
        if (resolution !== "acknowledged") {
          return fail("not_approvable", "An alert can only be acknowledged.");
        }
        break;
      case "dead_letter":
        if (resolution === "published_confirmed" || resolution === "acknowledged") {
          return fail("not_approvable", "A dead-letter job has no platform check to confirm against. Choose cancelled or accepted as lost.");
        }
        if (resolution === "accepted_as_lost" && trimmed === "") {
          return fail("reason_required", "Add a note to accept this as lost.");
        }
        break;
      case "outcome_unknown":
        if (resolution === "acknowledged") {
          return fail("not_approvable", "An unknown outcome is resolved, not acknowledged.");
        }
        if (resolution === "published_confirmed" && failure.reconcile?.outcome !== "found_on_platform") {
          return fail("not_approvable", "Check the platform first. Mark it published only when the check found the post.");
        }
        if (resolution === "cancelled_not_published" && failure.reconcile?.outcome !== "not_found") {
          return fail("not_approvable", "Check the platform first. Cancel only when the check did not find the post.");
        }
        if (resolution === "accepted_as_lost" && trimmed === "") {
          return fail("reason_required", "Add a note to accept this as lost.");
        }
        break;
    }

    failure.status = "resolved";
    failure.resolution = resolution;
    failure.resolutionNote = trimmed === "" ? null : trimmed;
    const job = failure.jobId ? this.jobs.get(failure.jobId) : undefined;
    if (job) {
      if (resolution === "published_confirmed") job.state = "published";
      if (resolution === "cancelled_not_published") job.state = "draft";
      if (resolution === "accepted_as_lost") job.state = "failed";
    }
    return { ok: true, value: clone(failure) };
  }

  listDigest(): Promise<Digest> {
    return this.read(() => {
      const now = Date.parse(MOCK_NOW_UTC);
      const start = now - DAY_MS;
      const within = (iso: string) => {
        const t = Date.parse(iso);
        return t >= start && t <= now;
      };
      const name = (accountId: string | null) =>
        this.accounts.find((a) => a.id === accountId)?.handle ?? "the daemon";
      const plural = (n: number, one: string, many: string) => (n === 1 ? one : many);

      const published = [...this.jobs.values()].filter((j) => j.state === "published" && within(j.scheduledAtUtc));
      const deadLetters = [...this.failures.values()].filter(
        (f) => f.kind === "dead_letter" && within(f.atUtc),
      );
      const unknownOpen = [...this.failures.values()].filter((f) => f.kind === "outcome_unknown" && f.status === "open");
      const alertsOpen = [...this.failures.values()].filter((f) => f.kind === "alert" && f.status === "open");
      const paused = this.accounts.filter((a) => a.readiness.paused !== null);
      const cooldowns = this.accounts.filter(
        (a) => a.readiness.quotaCooldownUntil !== null && Date.parse(a.readiness.quotaCooldownUntil) > now,
      );
      const pendingApprovals = [...this.jobs.values()].filter(awaitingDecision);
      const openThreads = [...this.threads.values()].filter((t) => t.status === "open");

      const sentences: string[] = [];
      sentences.push(
        published.length === 0
          ? "No post published in the last 24 hours."
          : `${published.length} ${plural(published.length, "post", "posts")} published in the last 24 hours: ${published
              .map((j) => name(j.accountId))
              .join(", ")}.`,
      );
      sentences.push(
        deadLetters.length === 0
          ? "No post stopped after repeated failures in the last 24 hours."
          : `${deadLetters.length} ${plural(deadLetters.length, "post stopped", "posts stopped")} after repeated failures: ${deadLetters
              .map((f) => name(f.accountId))
              .join(", ")}.`,
      );
      sentences.push(
        unknownOpen.length === 0
          ? "No post has an unknown outcome."
          : `${unknownOpen.length} ${plural(unknownOpen.length, "post has", "posts have")} an unknown outcome. Nothing is retried until each one is checked.`,
      );
      sentences.push(
        alertsOpen.length === 0
          ? "No open alerts."
          : `${alertsOpen.length} open ${plural(alertsOpen.length, "alert", "alerts")}: ${alertsOpen
              .map((f) => ALERT_LABEL[f.alertKind ?? ""] ?? "alert")
              .join(", ")}.`,
      );
      sentences.push(
        paused.length + cooldowns.length === 0
          ? "No account is paused or in a quota cooldown."
          : `${paused.length} ${plural(paused.length, "account is", "accounts are")} paused${
              paused.length ? `: ${paused.map((a) => a.handle).join(", ")}` : ""
            }. ${cooldowns.length} ${plural(cooldowns.length, "account is", "accounts are")} in a quota cooldown.`,
      );
      sentences.push(
        `${pendingApprovals.length} ${plural(pendingApprovals.length, "job is", "jobs are")} waiting for approval, and ${openThreads.length} ${plural(openThreads.length, "thread is", "threads are")} open.`,
      );

      return {
        windowStartUtc: new Date(start).toISOString(),
        windowEndUtc: MOCK_NOW_UTC,
        sentences,
        counts: {
          published: published.length,
          failedDeadLetter: deadLetters.length,
          outcomeUnknown: unknownOpen.length,
          openAlerts: alertsOpen.length,
          pausedAccounts: paused.length,
          quotaCooldowns: cooldowns.length,
          pendingApprovals: pendingApprovals.length,
          openThreads: openThreads.length,
        },
      };
    });
  }

  // ---- Threads ---------------------------------------------------------------------------

  listThreads(): Promise<ThreadItem[]> {
    return this.read(() => clone([...this.threads.values()]));
  }

  async resolveThread(threadId: string): Promise<Result<ThreadItem>> {
    await this.delay();
    const thread = this.threads.get(threadId);
    if (!thread) return fail("not_found", "This thread is no longer available. Nothing changed.");
    if (thread.status === "resolved") return fail("not_approvable", "This thread is already resolved.");
    thread.status = "resolved";
    thread.snoozedUntilUtc = null;
    return { ok: true, value: clone(thread) };
  }

  async snoozeThread(threadId: string, untilUtc: string): Promise<Result<ThreadItem>> {
    await this.delay();
    const thread = this.threads.get(threadId);
    if (!thread) return fail("not_found", "This thread is no longer available. Nothing changed.");
    if (thread.status === "resolved") return fail("not_approvable", "A resolved thread cannot be snoozed.");
    const until = Date.parse(untilUtc);
    if (Number.isNaN(until) || until <= Date.parse(MOCK_NOW_UTC)) {
      return fail("invalid_input", "Choose a time after now.");
    }
    thread.status = "snoozed";
    thread.snoozedUntilUtc = new Date(until).toISOString();
    return { ok: true, value: clone(thread) };
  }

  listModerationCapabilities(): Promise<ModerationCapability[]> {
    return this.read(() => clone(FIXTURE_MODERATION));
  }

  // ---- Manual drafts -------------------------------------------------------------------------
  // Mirrors the engine's rules: the approval gate, the https and host check, and revision
  // compare-and-swap. Nothing is posted. A recorded link changes only this mock's own state.

  listDrafts(): Promise<ManualDraft[]> {
    return this.read(() =>
      clone(
        [...this.drafts.values()].sort((a, b) => b.updatedAtUtc.localeCompare(a.updatedAtUtc) || a.id.localeCompare(b.id)),
      ),
    );
  }

  async approveDraft(
    draftId: string,
    revision: number,
    disclosurePhrases: readonly string[],
  ): Promise<Result<ManualDraft>> {
    await this.delay();
    const draft = this.drafts.get(draftId);
    if (!draft) return fail("not_found", "This draft is no longer available. Nothing changed.");
    if (draft.revision !== revision) return revisionConflict(draft, revision, "approving it");
    if (draft.state !== "drafted") {
      return fail("not_approvable", `Only a drafted draft can be approved. This one is ${draft.state}. Nothing changed.`);
    }
    if (draft.body.trim() === "") return fail("not_approvable", "The body is empty. Nothing was approved.");
    if ((draft.kind === "comment" || draft.kind === "reply") && !draft.target) {
      return fail("not_approvable", "A comment or reply needs a target URL before it can be approved.");
    }
    if (draft.disclosureRequired && !disclosureFound(draft.body, disclosurePhrases)) {
      const noPhrases = disclosurePhrases.every((p) => p.trim() === "");
      return fail(
        "not_approvable",
        noPhrases
          ? "Disclosure is required, and no disclosure phrases are set for this app. Nothing was approved."
          : "Disclosure is required: the first sentence must contain a disclosure phrase. Nothing was approved.",
      );
    }
    draft.state = "approved";
    draft.revision += 1;
    draft.updatedAtUtc = MOCK_NOW_UTC;
    return { ok: true, value: clone(draft) };
  }

  async markDraftPosted(draftId: string, revision: number, postedUrl: string): Promise<Result<ManualDraft>> {
    await this.delay();
    const draft = this.drafts.get(draftId);
    if (!draft) return fail("not_found", "This draft is no longer available. Nothing changed.");
    if (draft.revision !== revision) return revisionConflict(draft, revision, "recording the link");
    if (draft.state !== "approved") {
      return fail("not_approvable", `Only an approved draft can be marked posted. This one is ${draft.state}. Nothing changed.`);
    }
    const url = postedUrl.trim();
    const problem = postedLinkProblem(draft.platform, url);
    if (problem) return fail("invalid_input", problem);
    draft.state = "posted_by_operator";
    draft.postedUrl = url;
    draft.postedAtUtc = MOCK_NOW_UTC;
    draft.revision += 1;
    draft.updatedAtUtc = MOCK_NOW_UTC;
    return { ok: true, value: clone(draft) };
  }

  async skipDraft(draftId: string, revision: number, reason: string): Promise<Result<ManualDraft>> {
    await this.delay();
    const draft = this.drafts.get(draftId);
    if (!draft) return fail("not_found", "This draft is no longer available. Nothing changed.");
    if (draft.revision !== revision) return revisionConflict(draft, revision, "skipping it");
    const trimmed = reason.trim();
    if (trimmed === "") return fail("reason_required", "Add a reason to skip this draft. Nothing changed.");
    if (draft.state !== "drafted" && draft.state !== "approved") {
      return fail("not_approvable", `Only a drafted or approved draft can be skipped. This one is ${draft.state}. Nothing changed.`);
    }
    draft.state = "skipped";
    draft.skipReason = trimmed;
    draft.revision += 1;
    draft.updatedAtUtc = MOCK_NOW_UTC;
    return { ok: true, value: clone(draft) };
  }

  // ---- Suggestions and spend ------------------------------------------------------------
  // Mirrors the engine's rules: revision compare-and-swap, the proposed-only decision, and the
  // "remake_entry_" id of the accepted entry. The mock does not add a calendar job for it, because the
  // mock calendar has no account for a channel scope. Spend is read-only.

  listScopes(): Promise<OperatorScope[]> {
    return this.read(() => clone(FIXTURE_SCOPES));
  }

  listProposals(scopeId: string): Promise<OptimiseProposal[]> {
    return this.read(() =>
      clone(
        [...this.proposals.values()]
          .filter((p) => p.scopeId === scopeId)
          .sort((a, b) => a.createdAtUtc.localeCompare(b.createdAtUtc) || a.id.localeCompare(b.id)),
      ),
    );
  }

  getProposal(scopeId: string, proposalId: string): Promise<OptimiseProposal | null> {
    return this.read(() => {
      const proposal = this.proposals.get(proposalId);
      return proposal && proposal.scopeId === scopeId ? clone(proposal) : null;
    });
  }

  async acceptProposal(scopeId: string, proposalId: string, expectedRevision: number): Promise<Result<OptimiseProposal>> {
    await this.delay();
    const proposal = this.findProposal(scopeId, proposalId);
    if (!proposal) return fail("not_found", "This suggestion is not in this channel. Nothing changed.");
    if (proposal.revision !== expectedRevision) return proposalRevisionConflict(proposal, expectedRevision);
    if (proposal.state !== "proposed") {
      return fail("not_approvable", `Only a proposed suggestion can be accepted. This one is ${proposal.state}. Nothing changed.`);
    }
    proposal.state = "accepted";
    proposal.revision += 1;
    proposal.decidedAtUtc = MOCK_NOW_UTC;
    proposal.calendarEntryId = `remake_entry_${proposal.id}`;
    proposal.updatedAtUtc = MOCK_NOW_UTC;
    return { ok: true, value: clone(proposal) };
  }

  async dismissProposal(scopeId: string, proposalId: string, expectedRevision: number): Promise<Result<OptimiseProposal>> {
    await this.delay();
    const proposal = this.findProposal(scopeId, proposalId);
    if (!proposal) return fail("not_found", "This suggestion is not in this channel. Nothing changed.");
    if (proposal.revision !== expectedRevision) return proposalRevisionConflict(proposal, expectedRevision);
    if (proposal.state !== "proposed") {
      return fail("not_approvable", `Only a proposed suggestion can be dismissed. This one is ${proposal.state}. Nothing changed.`);
    }
    proposal.state = "dismissed";
    proposal.revision += 1;
    proposal.decidedAtUtc = MOCK_NOW_UTC;
    proposal.updatedAtUtc = MOCK_NOW_UTC;
    return { ok: true, value: clone(proposal) };
  }

  getAllowance(scopeId: string, month: string): Promise<SpendAllowance | null> {
    return this.read(() => {
      const row = FIXTURE_ALLOWANCES.find((a) => a.scopeId === scopeId && a.allowance.month === month);
      return row ? clone(row.allowance) : null;
    });
  }

  spendReport(scopeId: string, month: string): Promise<SpendReport | null> {
    return this.read(() => {
      const row = FIXTURE_REPORTS.find((r) => r.scopeId === scopeId && r.report.month === month);
      return row ? clone(row.report) : null;
    });
  }

  listPrices(): Promise<SpendPrice[]> {
    return this.read(() => clone(FIXTURE_PRICES));
  }

  // ---- Campaigns and hook tests (read only) ---------------------------------------------------
  // Mirrors the engine's ordering: campaigns by id, links by variant, platform and link id.

  listCampaigns(scopeId: string): Promise<Campaign[]> {
    return this.read(() =>
      clone(
        FIXTURE_CAMPAIGNS.filter((c) => c.scopeId === scopeId).sort((a, b) => a.id.localeCompare(b.id)),
      ),
    );
  }

  listCampaignLinks(scopeId: string, campaignId: string): Promise<TrackedLink[]> {
    return this.read(() => clone(sortedLinks(scopeId, campaignId)));
  }

  campaignAttribution(scopeId: string, campaignId: string): Promise<CampaignAttribution | null> {
    return this.read(() => {
      const inScope = FIXTURE_CAMPAIGNS.some((c) => c.scopeId === scopeId && c.id === campaignId);
      if (!inScope) return null;
      const links = sortedLinks(scopeId, campaignId).map((link) => ({
        linkId: link.id,
        variantId: link.variantId,
        platform: link.platform,
        calendarEntryId: link.calendarEntryId,
        visits: FIXTURE_LINK_VISITS[link.id] ?? 0,
      }));
      return {
        campaignId,
        totalVisits: links.reduce((sum, row) => sum + row.visits, 0),
        links,
      };
    });
  }

  // Every link of every campaign in the scope: campaigns by id, then the links of each in the engine's order.
  listChannelLinks(scopeId: string): Promise<ChannelLink[]> {
    return this.read((): ChannelLink[] => {
      const campaigns = FIXTURE_CAMPAIGNS.filter((c) => c.scopeId === scopeId).sort((a, b) => a.id.localeCompare(b.id));
      return clone(
        campaigns.flatMap((campaign) =>
          sortedLinks(scopeId, campaign.id).map((link) => ({
            link,
            campaignName: campaign.name,
            visits: FIXTURE_LINK_VISITS[link.id] ?? 0,
          })),
        ),
      );
    });
  }

  listHookTests(scopeId: string): Promise<HookTestSet[]> {
    return this.read(() => clone(FIXTURE_HOOK_TESTS.filter((s) => s.scopeId === scopeId)));
  }

  getHookTest(scopeId: string, setId: string): Promise<HookTestSet | null> {
    return this.read(() => {
      const set = FIXTURE_HOOK_TESTS.find((s) => s.scopeId === scopeId && s.id === setId);
      return set ? clone(set) : null;
    });
  }

  // ---- Research digests and content library (read only) ---------------------------------------
  // Mirrors the engine's ordering: digests by date then id, content by content id. Nothing is written.

  listDigests(scopeId: string): Promise<ResearchDigest[]> {
    return this.read(() =>
      clone(
        FIXTURE_DIGESTS.filter((d) => d.scopeId === scopeId).sort(
          (a, b) => a.digestDate.localeCompare(b.digestDate) || a.id.localeCompare(b.id),
        ),
      ),
    );
  }

  getDigest(scopeId: string, digestId: string): Promise<ResearchDigest | null> {
    return this.read(() => {
      const digest = FIXTURE_DIGESTS.find((d) => d.scopeId === scopeId && d.id === digestId);
      return digest ? clone(digest) : null;
    });
  }

  digestApplications(scopeId: string, digestId: string): Promise<DigestApplication[] | null> {
    return this.read(() => {
      const inScope = FIXTURE_DIGESTS.some((d) => d.scopeId === scopeId && d.id === digestId);
      if (!inScope) return null;
      return clone(
        FIXTURE_DIGEST_APPLICATIONS.filter((a) => a.digestId === digestId).sort(
          (a, b) => a.appliedAtUtc.localeCompare(b.appliedAtUtc) || a.digestEntryId.localeCompare(b.digestEntryId),
        ),
      );
    });
  }

  listContent(scopeId: string): Promise<ContentRecord[]> {
    return this.read(() =>
      clone(FIXTURE_CONTENT.filter((c) => c.scopeId === scopeId).sort((a, b) => a.id.localeCompare(b.id))),
    );
  }

  getContent(scopeId: string, contentId: string): Promise<ContentDetail | null> {
    return this.read(() => {
      const record = FIXTURE_CONTENT.find((c) => c.scopeId === scopeId && c.id === contentId);
      if (!record) return null;
      const variants = FIXTURE_CONTENT_VARIANTS.filter((v) => v.scopeId === scopeId && v.contentId === contentId).sort(
        (a, b) => a.kind.localeCompare(b.kind) || a.createdAtUtc.localeCompare(b.createdAtUtc) || a.id.localeCompare(b.id),
      );
      return clone({ record, variants });
    });
  }

  contentExperimentSummary(scopeId: string, contentId: string): Promise<ContentExperimentSummary | null> {
    return this.read(() => {
      const summary = FIXTURE_CONTENT_SUMMARIES.find((s) => s.scopeId === scopeId && s.contentId === contentId);
      return summary ? clone(summary) : null;
    });
  }

  // ---- Text gate, briefs, deliveries and dispatch attempts (read only) ----------------------------
  // Mirrors the engine: a check stores nothing, and a scope with no policy has no gate policy here.

  gatePolicy(scopeId: string): Promise<GatePolicy | null> {
    return this.read(() => {
      const policy = FIXTURE_GATE_POLICIES.find((p) => p.scopeId === scopeId);
      return policy ? clone(policy) : null;
    });
  }

  checkText(scopeId: string, subjectId: string, text: string): Promise<Result<GateCheck>> {
    return this.read((): Result<GateCheck> => {
      const policy = FIXTURE_GATE_POLICIES.find((p) => p.scopeId === scopeId);
      if (!policy) {
        return {
          ok: false,
          error: {
            code: "engine_error",
            message: "No gate policy is set for this channel, so nothing can pass.",
            engineCode: "policy_missing",
          },
        };
      }
      return { ok: true, value: mockGateCheck(policy, subjectId, text) };
    });
  }

  gateRecords(scopeId: string, subjectId: string): Promise<GateRecord[]> {
    return this.read(() =>
      clone(
        FIXTURE_GATE_RECORDS.filter((r) => r.scopeId === scopeId && r.subjectId === subjectId).sort(
          (a, b) => b.recordSeq - a.recordSeq,
        ),
      ),
    );
  }

  listBriefs(scopeId: string): Promise<OutgoingBrief[]> {
    return this.read(() =>
      clone(
        FIXTURE_BRIEFS.filter((b) => b.scopeId === scopeId).sort((a, b) => a.requestId.localeCompare(b.requestId)),
      ),
    );
  }

  getBrief(scopeId: string, requestId: string): Promise<OutgoingBriefDetail | null> {
    return this.read(() => {
      const brief = FIXTURE_BRIEFS.find((b) => b.scopeId === scopeId && b.requestId === requestId);
      return brief ? clone(brief) : null;
    });
  }

  listDeliveries(scopeId: string): Promise<DeliveryEnvelope[]> {
    return this.read(() =>
      clone(
        FIXTURE_DELIVERIES.filter((d) => d.scopeId === scopeId).sort(
          (a, b) => a.acceptedAtUtc.localeCompare(b.acceptedAtUtc) || a.envelopeId.localeCompare(b.envelopeId),
        ),
      ),
    );
  }

  getDelivery(scopeId: string, envelopeId: string): Promise<DeliveryEnvelope | null> {
    return this.read(() => {
      const delivery = FIXTURE_DELIVERIES.find((d) => d.scopeId === scopeId && d.envelopeId === envelopeId);
      return delivery ? clone(delivery) : null;
    });
  }

  listDispatchAttempts(scopeId: string, requestId: string): Promise<DispatchAttempt[]> {
    return this.read(() =>
      clone(
        FIXTURE_DISPATCH_ATTEMPTS.filter((a) => a.scopeId === scopeId && a.requestId === requestId).sort(
          (a, b) => a.attemptId - b.attemptId,
        ),
      ),
    );
  }

  // ---- Channel memory and proof bank (read only) ----------------------------------------------
  // Mirrors the engine: the latest revision by number, history newest first without documents, and proofs
  // sorted by answer id. Nothing is written.

  channelMemory(scopeId: string): Promise<ChannelMemoryDetail | null> {
    return this.read(() => {
      const latest = FIXTURE_MEMORY.filter((m) => m.scopeId === scopeId).sort((a, b) => b.revision - a.revision)[0];
      return latest ? clone(latest) : null;
    });
  }

  channelMemoryHistory(scopeId: string): Promise<ChannelMemoryRevision[]> {
    return this.read(() =>
      FIXTURE_MEMORY.filter((m) => m.scopeId === scopeId)
        .sort((a, b) => b.revision - a.revision)
        .map((detail) => revisionOnly(detail)),
    );
  }

  listProofs(scopeId: string, status: ProofStatus | null): Promise<ProofRecord[]> {
    return this.read(() =>
      clone(
        FIXTURE_PROOFS.filter((p) => p.scopeId === scopeId && (status === null || p.status === status)).sort(
          (a, b) => a.answerId.localeCompare(b.answerId),
        ),
      ),
    );
  }

  // ---- Typed briefs and platform variants (read only) ----------------------------------------------
  // Mirrors the engine's ordering: briefs by brief id, variants by platform then variant id, rules by platform.
  // Nothing is written.

  listTypedBriefs(scopeId: string): Promise<TypedBrief[]> {
    return this.read(() =>
      clone(
        FIXTURE_TYPED_BRIEFS.filter((b) => b.scopeId === scopeId).sort((a, b) => a.briefId.localeCompare(b.briefId)),
      ),
    );
  }

  getTypedBrief(scopeId: string, briefId: string): Promise<TypedBrief | null> {
    return this.read(() => {
      const brief = FIXTURE_TYPED_BRIEFS.find((b) => b.scopeId === scopeId && b.briefId === briefId);
      return brief ? clone(brief) : null;
    });
  }

  listBriefVariants(scopeId: string, briefId: string): Promise<BriefVariant[]> {
    return this.read(() =>
      clone(
        FIXTURE_BRIEF_VARIANTS.filter((v) => v.scopeId === scopeId && v.briefId === briefId).sort(
          (a, b) => a.platform.localeCompare(b.platform) || a.variantId.localeCompare(b.variantId),
        ),
      ),
    );
  }

  // ---- Analytics and manual imports (read only) -----------------------------------------------
  // Mirrors the engine: the latest value of each metric of each post, the history oldest first, and the imports in
  // creation order. Nothing is pulled or imported here.

  listAnalyticsPosts(scopeId: string): Promise<AnalyticsPost[]> {
    return this.read(() => clone(latestAnalytics(scopeId)));
  }

  listAnalyticsObservations(scopeId: string, jobId: string): Promise<AnalyticsObservation[]> {
    return this.read(() =>
      clone(
        FIXTURE_ANALYTICS_OBSERVATIONS.filter((row) => row.scopeId === scopeId && row.jobId === jobId).sort(
          (a, b) => a.observedAtUtc.localeCompare(b.observedAtUtc),
        ),
      ),
    );
  }

  listMetricImports(scopeId: string): Promise<MetricImport[]> {
    return this.read(() =>
      clone(
        FIXTURE_METRIC_IMPORTS.filter((row) => row.scopeId === scopeId).sort(
          (a, b) => a.createdAtUtc.localeCompare(b.createdAtUtc) || a.importId.localeCompare(b.importId),
        ),
      ),
    );
  }

  // ---- Publish jobs (read only) ---------------------------------------------------------------
  // Mirrors the engine: every job of the scope, newest first. Nothing is queued, stepped or resolved here.

  listPublishJobs(scopeId: string): Promise<PublishJobRecord[]> {
    return this.read(() =>
      clone(
        FIXTURE_PUBLISH_JOBS.filter((job) => job.scopeId === scopeId).sort(
          (a, b) => b.createdAtUtc.localeCompare(a.createdAtUtc) || a.jobId.localeCompare(b.jobId),
        ),
      ),
    );
  }

  // ---- Webhook inbox (read only) ---------------------------------------------------------------
  // Mirrors the engine: the newest events of one account, or the unmatched ones, newest first. Nothing is received
  // or marked here. The unmatched list ends with the rows the desktop cannot read, as the engine would keep them.

  listInbox(scopeId: string | null): Promise<InboxListing> {
    return this.read((): InboxListing => {
      const events: InboxRow[] = FIXTURE_INBOX.filter((row) => row.event.scopeId === scopeId)
        .map((row) => row.event)
        .sort((a, b) => b.receivedAtUtc.localeCompare(a.receivedAtUtc) || b.eventId.localeCompare(a.eventId));
      const rows: InboxRow[] = scopeId === null ? [...events, ...FIXTURE_UNMATCHED_UNREADABLE] : events;
      return clone({ rows, moreAvailable: false });
    });
  }

  getInboxEvent(scopeId: string | null, eventId: string): Promise<InboxRaw | null> {
    return this.read((): InboxRaw | null => {
      const row = FIXTURE_INBOX.find((r) => r.event.eventId === eventId && r.event.scopeId === scopeId);
      return row ? { eventId, rawJson: row.rawJson } : null;
    });
  }

  inboxStats(): Promise<InboxStats> {
    return this.read(() => clone(FIXTURE_INBOX_STATS));
  }

  // ---- Publish pacing (read only) -------------------------------------------------------------
  // Mirrors the engine's answer for the channel and for each account. Nothing is queued, run or changed here.

  listPublishNext(scopeId: string): Promise<PublishNextView> {
    return this.read(() => clone(pacingViewFor(scopeId)));
  }

  getPublishQuota(scopeId: string, platform: string, remoteAccount: string): Promise<PublishQuota> {
    return this.read(() => clone(quotaFor(scopeId, platform, remoteAccount)));
  }

  // ---- Store health (read only)---------------------------------------------------------------
  // Mirrors the engine: the age is whole seconds from the `now` given, and a source older than the limit is stale.
  // Nothing is beaten, backed up or restored here.

  heartbeatStatus(nowUtc: string, staleAfterSecs: number): Promise<HeartbeatStatus> {
    return this.read((): HeartbeatStatus => {
      const now = Date.parse(nowUtc);
      const ageSeconds = Math.floor((now - FIXTURE_HEARTBEAT.updatedMs) / 1000);
      const state = ageSeconds <= staleAfterSecs ? "alive" : "stale";
      return {
        nowUtc,
        staleAfterSeconds: staleAfterSecs,
        allAlive: state === "alive",
        state,
        pid: FIXTURE_HEARTBEAT.pid,
        updatedMs: FIXTURE_HEARTBEAT.updatedMs,
        ageSeconds,
        recordState: FIXTURE_HEARTBEAT.recordState,
        error: null,
      };
    });
  }

  listRestoreReceipts(): Promise<RestoreReceiptList> {
    return this.read((): RestoreReceiptList => ({ receipts: clone(FIXTURE_RESTORE_RECEIPTS), unreadable: [] }));
  }

  private findProposal(scopeId: string, proposalId: string): OptimiseProposal | undefined {
    const proposal = this.proposals.get(proposalId);
    return proposal && proposal.scopeId === scopeId ? proposal : undefined;
  }

  private isPaused(accountId: string): boolean {
    return this.accounts.some((a) => a.id === accountId && a.readiness.paused !== null);
  }

  private async read<T>(produce: () => T): Promise<T> {
    await this.delay();
    if (this.failReads) throw new Error("Mock read failure: failReads is set");
    return produce();
  }

  private delay(): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, this.latencyMs));
  }
}

/** A regular expression's literal text, so a banned phrase is matched as typed. */
function escapeRegExp(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * The mock's text check. It fails a script that contains a banned phrase of the policy as a whole word,
 * case-insensitive, and passes every other script. It simulates no other rule: each other rule passes and
 * says so. The real engine also checks numbers, which this mock does not do.
 */
function mockGateCheck(policy: GatePolicy, subjectId: string, text: string): GateCheck {
  const hit = policy.bannedPhrases
    .map((phrase) => ({ phrase, match: new RegExp(`\\b${escapeRegExp(phrase)}\\b`, "i").exec(text) }))
    .find((row) => row.match !== null);
  const bannedFailed = hit !== undefined;
  const bannedReason =
    hit !== undefined && hit.match !== null
      ? `Mock check: the banned phrase "${hit.phrase}" occurs at character ${hit.match.index}.`
      : "Mock check: no banned phrase occurs.";

  const simulated = (ruleId: string, required: boolean): GateRuleFinding => ({
    ruleId,
    required,
    verdict: "pass",
    blocks: false,
    reason: "Mock check: this rule is not simulated, so it passes.",
  });
  const rules: GateRuleFinding[] = [
    {
      ruleId: "det.text.banned_phrase",
      required: policy.required.bannedPhrase,
      verdict: bannedFailed ? "fail" : "pass",
      blocks: bannedFailed,
      reason: bannedReason,
    },
    simulated("det.text.dash_density", policy.required.dashDensity),
    simulated("det.text.semicolon_density", policy.required.semicolonDensity),
    simulated("det.text.sentence_length_variance", policy.required.sentenceLengthVariance),
    simulated("det.text.tricolon_frequency", policy.required.tricolonFrequency),
    simulated("det.text.stock_opener", policy.required.stockOpener),
    simulated("det.text.claim_proof", true),
    simulated("postright.text.claim_citation", true),
    simulated("postright.text.unannotated_number", true),
  ];
  const blockingRules = rules.filter((rule) => rule.blocks).map((rule) => rule.ruleId);
  return {
    subjectId,
    disposition: blockingRules.length > 0 ? "block" : "pass",
    policyRevision: policy.revision,
    textHash: `mock:${fingerprint(text)}`,
    rules,
    blockingRules,
    proofIdsUsed: [],
  };
}

/** Hosts a posted link may use, by platform. A platform not listed accepts any https host. Mirrors the engine. */
const POSTED_HOSTS: Record<string, readonly string[]> = {
  reddit: ["reddit.com", "www.reddit.com", "old.reddit.com"],
  x: ["x.com", "twitter.com"],
  threads: ["threads.net", "www.threads.net", "threads.com", "www.threads.com"],
};

/** The reason a posted link is refused, or null when it passes the https and host checks. */
function postedLinkProblem(platform: string, url: string): string | null {
  if (/\s/.test(url) || !/^https:\/\//i.test(url)) return "The link must be an https URL. Nothing changed.";
  const authority = url.slice(8).split(/[/?#]/)[0].toLowerCase();
  if (authority === "" || /[@:]/.test(authority)) {
    return "The link must name a plain host, with no user, port or credentials. Nothing changed.";
  }
  const allowed = POSTED_HOSTS[platform];
  if (allowed && !allowed.includes(authority)) {
    return `The link's host ${authority} is not an allowed ${platform} host. Nothing changed.`;
  }
  return null;
}

function revisionConflict<T>(draft: ManualDraft, revision: number, action: string): Result<T> {
  return {
    ok: false,
    error: {
      code: "revision_conflict",
      message: `This draft is now revision ${draft.revision}, not ${revision}. Review it before ${action}.`,
      currentRevision: draft.revision,
    },
  };
}

function proposalRevisionConflict<T>(proposal: OptimiseProposal, expected: number): Result<T> {
  return {
    ok: false,
    error: {
      code: "revision_conflict",
      message: `This suggestion is now revision ${proposal.revision}, not ${expected}. Nothing changed.`,
      currentRevision: proposal.revision,
    },
  };
}

/** A memory revision without its document, as memory.history lists it. */
function revisionOnly(detail: ChannelMemoryDetail): ChannelMemoryRevision {
  return {
    scopeId: detail.scopeId,
    revision: detail.revision,
    supersedes: detail.supersedes,
    author: detail.author,
    createdAtUtc: detail.createdAtUtc,
    contentHash: detail.contentHash,
  };
}

/**
 * The latest stored value of each metric of each post, as analytics.summary reports it. For one metric and window
 * the row observed last wins; at the same instant the later row in the fixture list wins. A post's age is its
 * observation instant minus its publish instant, in seconds.
 */
function latestAnalytics(scopeId: string): AnalyticsPost[] {
  return FIXTURE_ANALYTICS_POSTS.filter((post) => post.scopeId === scopeId)
    .sort((a, b) => a.publishedAtUtc.localeCompare(b.publishedAtUtc) || a.jobId.localeCompare(b.jobId))
    .map((post) => {
      const latest = new Map<string, AnalyticsObservation>();
      for (const row of FIXTURE_ANALYTICS_OBSERVATIONS.filter((o) => o.jobId === post.jobId)) {
        const key = `${row.metric}|${row.window ?? "no window"}`;
        const held = latest.get(key);
        if (held === undefined || row.observedAtUtc >= held.observedAtUtc) latest.set(key, row);
      }
      const metrics: AnalyticsSummaryMetric[] = [...latest.values()]
        .sort((a, b) => a.metric.localeCompare(b.metric))
        .map((row) => ({
          metric: row.metric,
          state: row.state,
          value: row.value,
          observedAtUtc: row.observedAtUtc,
          window: row.window,
          source: row.source,
          pullId: row.pullId,
          importId: row.importId,
          postAgeSeconds: (Date.parse(row.observedAtUtc) - Date.parse(post.publishedAtUtc)) / 1000,
        }));
      return { ...post, metrics };
    });
}

function fail<T>(code: DataErrorCode, message: string): Result<T> {
  return { ok: false, error: { code, message } };
}
