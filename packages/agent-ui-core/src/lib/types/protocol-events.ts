import type { ToolCall } from "./benchmarks-artifacts";
import type { MiddlewareDecisionEvidence } from "./middleware-runtime";
import type {
  OrchestrationObservation,
  TaskNode,
  TranscriptMessage,
  TrustEnvelope,
  VerificationIssue,
} from "./protocol-core";
import type { AgentRosterRow, UserQuestion } from "./protocol-ops";
import type { ReferenceMeta } from "./references";

export type GoalStepV1 = {
  id: string;
  label: string;
  weight: number;
  state: string;
  verificationState: string;
};

export type GoalSnapshotV1 = {
  goalId: string;
  objective: string;
  status: string;
  startedAt: string;
  closedAt: string | null;
  dismissedAt: string | null;
  decompositionState: string;
  planRevision: number;
  completionPercent: number | null;
  now: string | null;
  next: string | null;
  steps: GoalStepV1[];
  verificationState: string;
  budgetTokens: number;
  tokensUsed: number;
  continuationCount: number;
  noProgressCount: number;
};

/** Typed, reaped terminal status of a shell child. */
export type ShellExitStatus =
  | { state: "exited"; code: number }
  | { state: "signaled"; signal?: number | null }
  | { state: "killedByTimeout"; timeoutMs: number }
  | { state: "killedByCancel" }
  | { state: "spawnFailed"; reason: string };

export type ProtocolEvent =
  | {
      $kind: "sessionStarted";
      sessionId: string;
      seq: number;
      createdAt: string;
      title?: string;
    }
  | {
      $kind: "sessionTitleUpdated";
      sessionId: string;
      seq: number;
      createdAt: string;
      title: string;
    }
  | {
      $kind: "sessionContextFolded";
      sessionId: string;
      seq: number;
      createdAt: string;
      subtaskId: string;
      startMarker: string;
      endMarker: string;
      fold: TranscriptMessage;
    }
  | {
      $kind: "selfReviewProposalReceived";
      sessionId: string;
      seq: number;
      createdAt: string;
      proposal: {
        id: string;
        kind: "memory" | "harnessRule" | "workflow" | "skillTweak" | "skill";
        summary: string;
        evidence: string;
      };
    }
  | {
      $kind: "assistantDelta";
      sessionId: string;
      seq: number;
      createdAt: string;
      text: string;
      isReasoning: boolean;
    }
  | {
      $kind: "assistantMessageCompleted";
      sessionId: string;
      seq: number;
      createdAt: string;
      message: Extract<TranscriptMessage, { $kind: "assistant" }>;
    }
  | {
      $kind: "toolCallStarted";
      sessionId: string;
      seq: number;
      createdAt: string;
      call: ToolCall;
    }
  | {
      $kind: "toolResultAdded";
      sessionId: string;
      seq: number;
      createdAt: string;
      result: Extract<TranscriptMessage, { $kind: "tool" }>;
    }
  | {
      $kind: "sessionTurnFinished";
      sessionId: string;
      seq: number;
      createdAt: string;
      turnId: string;
      status: string;
    }
  | {
      $kind: "streamResyncRequired";
      sessionId: string;
      seq: number;
      createdAt: string;
      afterSeq: number;
      oldestAvailableSeq: number;
      highWaterMark: number;
    }
  | {
      $kind: "overloaded";
      sessionId: string;
      seq: number;
      createdAt: string;
      scope: string;
      limit: number;
      current: number;
      retryAfterMs: number;
    }
  | {
      $kind: "toolApprovalRequested";
      sessionId: string;
      seq: number;
      createdAt: string;
      call: ToolCall;
      riskLabel: string;
      proposedChange?: string;
      policySource?: string;
      middlewareDecisions: MiddlewareDecisionEvidence[];
    }
  | {
      $kind: "toolApprovalResolved";
      sessionId: string;
      seq: number;
      createdAt: string;
      toolCallId: string;
      approved: boolean;
    }
  | {
      $kind: "runInterrupted";
      sessionId: string;
      seq: number;
      createdAt: string;
      reason?: string;
    }
  | {
      $kind: "runFailed";
      sessionId: string;
      seq: number;
      createdAt: string;
      error: string;
      kind: string;
      hint: string;
    }
  | {
      $kind: "usageUpdated";
      sessionId: string;
      seq: number;
      createdAt: string;
      promptTokens: number;
      completionTokens: number;
      costUsd: number;
      contextTokens: number;
      contextPercent: number;
      cacheReadTokens?: number;
      cacheCreationTokens?: number;
    }
  | {
      $kind: "sessionCompacted";
      sessionId: string;
      seq: number;
      createdAt: string;
      summary: string;
      droppedCount: number;
    }
  | {
      $kind: "backgroundShellExited";
      sessionId: string;
      seq: number;
      createdAt: string;
      shellId: string;
      command: string;
      status: ShellExitStatus;
      outputBytes: number;
    }
  | {
      $kind: "budgetExceeded";
      sessionId: string;
      seq: number;
      createdAt: string;
      scope: string;
      currentUsd: number;
      ceilingUsd: number;
      hint: string;
    }
  | {
      $kind: "verificationFinished";
      sessionId: string;
      seq: number;
      createdAt: string;
      summary: string;
      passed: boolean;
      tool: string;
      issueCount: number;
      issues: VerificationIssue[];
    }
  | {
      $kind: "taskGraphUpdated";
      sessionId: string;
      seq: number;
      createdAt: string;
      summary: string;
      tasks: TaskNode[];
    }
  | {
      $kind: "taskStatusUpdated";
      sessionId: string;
      seq: number;
      createdAt: string;
      taskId: string;
      contextId: string;
      state: string;
      task: unknown;
    }
  | {
      $kind: "taskArtifactUpdated";
      sessionId: string;
      seq: number;
      createdAt: string;
      taskId: string;
      contextId: string;
      artifactId: string;
      artifact: unknown;
    }
  | {
      $kind: "artifactLineageUpdated";
      sessionId: string;
      seq: number;
      createdAt: string;
      taskId: string;
      contextId: string;
      artifactId: string;
      lineage: unknown;
    }
  | {
      $kind: "providerRouteUpdated";
      sessionId: string;
      seq: number;
      createdAt: string;
      state: string;
      provider: string;
      model: string;
      failureKind: string;
      detail: string;
    }
  | {
      $kind: "accountRateLimited";
      sessionId: string;
      seq: number;
      createdAt: string;
      provider: string;
      /** Display label: the user's account label, else email, else profile id. */
      account: string;
      /** Stable account id (`subscription/<provider>/<label>` or `api_key/<provider>/<slug>`). */
      accountId?: string | null;
      accountKind?: "subscription" | "api_key" | null;
      /** Only when the vendor CLI reported one; never a label. */
      email?: string | null;
      organization?: string | null;
      status: string;
      rateLimitType?: string | null;
      /** Unix seconds at which the window resets. */
      resetsAt?: number | null;
      utilization?: number | null;
      overageStatus?: string | null;
      errorCode?: string | null;
    }
  | {
      $kind: "accountUsageReported";
      sessionId: string;
      seq: number;
      createdAt: string;
      provider: string;
      account: string;
      accountId?: string | null;
      accountKind?: "subscription" | "api_key" | null;
      email?: string | null;
      totalCostUsd?: number | null;
      durationMs?: number | null;
      numTurns?: number | null;
      inputTokens: number;
      outputTokens: number;
      cacheReadTokens: number;
      cacheCreationTokens: number;
      models: string[];
    }
  | {
      /** Live-only: the session's persistence latch tripped; in-flight tools were cancelled. */
      $kind: "persistenceFailed";
      sessionId: string;
      seq: number;
      createdAt: string;
      reason: string;
    }
  | {
      $kind: "dispatchDecided";
      sessionId: string;
      seq: number;
      createdAt: string;
      strategy: string;
      rationale: string;
    }
  | {
      $kind: "orchestrationObservationRecorded";
      sessionId: string;
      seq: number;
      createdAt: string;
      observation: OrchestrationObservation;
    }
  | {
      $kind: "agentQueryIssued";
      sessionId: string;
      seq: number;
      createdAt: string;
      queryId: string;
      fromRole: string;
      toRole: string;
      queryType: string;
      expectedFormat: string;
    }
  | {
      $kind: "agentQueryResponded";
      sessionId: string;
      seq: number;
      createdAt: string;
      queryId: string;
      fromRole: string;
      toRole: string;
      confidence: number;
    }
  | {
      $kind: "humanSteeringInjected";
      sessionId: string;
      seq: number;
      createdAt: string;
      injectedBy: string;
      contentPreview: string;
    }
  | {
      $kind: "subAgentSpawned";
      sessionId: string;
      seq: number;
      createdAt: string;
      subSessionId: string;
      role: string;
      taskTitle: string;
    }
  | {
      $kind: "subAgentCompleted";
      sessionId: string;
      seq: number;
      createdAt: string;
      subSessionId: string;
      role: string;
      success: boolean;
      summary: string;
    }
  | {
      $kind: "subAgentCancelled";
      sessionId: string;
      seq: number;
      createdAt: string;
      subSessionId: string;
      role: string;
      cancelledBy: string;
    }
  | {
      $kind: "userMessageSubmitted";
      sessionId: string;
      seq: number;
      createdAt: string;
      text: string;
      nonce?: string;
      trust?: TrustEnvelope;
      /** Reference chips the user attached (images, files, links, generated
       * docs, artifacts, change sets). **Metadata only** — image bytes travel
       * in the engine delivery path and never in the events table; a restart
       * rebuilds the chip strip from these records and the user re-attaches
       * the bytes if they want to re-send. Optional so legacy senders (pre-D2)
       * parse cleanly. */
      attachments?: ReferenceMeta[];
    }
  | {
      $kind: "userQuestionAsked";
      sessionId: string;
      seq: number;
      createdAt: string;
      questionId: string;
      questions: UserQuestion[];
    }
  | {
      $kind: "userQuestionAnswered";
      sessionId: string;
      seq: number;
      createdAt: string;
      questionId: string;
    }
  | {
      $kind: "planPresented";
      sessionId: string;
      seq: number;
      createdAt: string;
      planId: string;
      plan: string;
    }
  | {
      $kind: "agentRosterUpdated";
      sessionId: string;
      seq: number;
      createdAt: string;
      agents: AgentRosterRow[];
    }
  | {
      $kind: "checkRunCompleted";
      sessionId: string;
      seq: number;
      createdAt: string;
      checkId: string;
      command: string;
      exitCode: number;
      attempts: number;
      durationMs: number;
      passed: boolean;
      jurySkipped: boolean;
    }
  | {
      $kind: "goalProgressUpdated";
      sessionId: string;
      seq: number;
      createdAt: string;
      goalId: string;
      objective: string;
      status: string;
      startedAt: string;
      closedAt: string | null;
      dismissedAt: string | null;
      decompositionState: string;
      planRevision: number;
      completionPercent: number | null;
      now: string | null;
      next: string | null;
      steps: GoalStepV1[];
      verificationState: string;
      tokensUsed: number;
      budgetTokens: number;
      continuationCount: number;
      noProgressCount: number;
    }
  | {
      $kind: "modelRouteDecision";
      sessionId: string;
      seq: number;
      createdAt: string;
      agentRole: string;
      modelId: string;
      /** "cheap" | "capable" */
      tier: string;
      reason: string;
      signalsDigest: string;
      priceSnapshot: {
        promptPerM: number;
        completionPerM: number;
        source: string;
        fetchedAt: string;
        hash: string;
      };
      /** Pre-call estimate — never an actual. Actuals reconcile via usageUpdated. */
      estCostUsd: number;
      schemaVersion: number;
    }
  | {
      $kind: "runGovernanceRejected";
      sessionId: string;
      seq: number;
      createdAt: string;
      kind: string;
      toolCallId: string;
      budget?: unknown;
      ownership?: unknown;
      origin?: unknown;
    };
