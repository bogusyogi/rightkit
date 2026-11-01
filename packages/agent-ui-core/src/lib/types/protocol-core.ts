import type { OutputRef, ToolCall } from "./benchmarks-artifacts";
import type { ReferenceMeta } from "./references";

export type ContentTrust =
  | "system"
  | "user"
  | "workspaceTrusted"
  | "toolTrusted"
  | "toolUntrusted"
  | "remoteAgentUntrusted";

export interface TrustEnvelope {
  trust: ContentTrust;
  source: string;
  instructionsAreData: boolean;
  contentHash: string;
}

export interface UntrustedPolicyAction {
  action: "review_required" | "treat_as_data" | "none";
  approvalAuthority: boolean;
  policyChangeAuthority: boolean;
  instructionsAreData: boolean;
  blockedAuthorityRequests: Array<{
    kind: "approval_authority" | "policy_change_authority";
    blocked: boolean;
    reason: string;
  }>;
  reason: string;
}

export interface UntrustedContentItem {
  index: number;
  messageId: string;
  messageKind: "user" | "tool";
  createdAt: string;
  trust: TrustEnvelope;
  policyAction: UntrustedPolicyAction;
  preview: string;
  toolCallId?: string;
  isError?: boolean;
}

export interface UntrustedContentAudit {
  sessionId: string;
  untrustedCount: number;
  items: UntrustedContentItem[];
}

export type TranscriptMessage =
  | {
      $kind: "system";
      id: string;
      createdAt: string;
      content: string;
      pinned: boolean;
    }
  | {
      $kind: "user";
      id: string;
      createdAt: string;
      content: string;
      trust?: TrustEnvelope;
      pinned: boolean;
      nonce?: string;
      /** Reference chips visible on this user message in the desktop
       * transcript. **Metadata only** — image bytes live in the legacy
       * `Attachment` field of the in-memory composer state and never on the
       * wire. Optional so legacy transcript rows (pre-references) parse
       * cleanly. */
      attachments?: ReferenceMeta[];
    }
  | {
      $kind: "assistant";
      id: string;
      createdAt: string;
      content: string;
      reasoningContent?: string;
      toolCalls: ToolCall[];
      pinned: boolean;
    }
  | {
      $kind: "tool";
      id: string;
      createdAt: string;
      toolCallId: string;
      summary: string;
      output?: OutputRef;
      trust?: TrustEnvelope;
      isError: boolean;
      pinned: boolean;
    }
  | {
      $kind: "compact";
      id: string;
      createdAt: string;
      summary: string;
      droppedCount: number;
      pinned: boolean;
    };

export interface VerificationIssue {
  tool: string;
  filePath: string;
  line?: number;
  column?: number;
  message: string;
  repairSuggestion: string;
}

export interface OrchestrationObservation {
  id: string;
  kind: string;
  subject: string;
  summary: string;
  fields: Record<string, string>;
  createdAt: string;
}

export type TaskState =
  | "open"
  | "inProgress"
  | "blocked"
  | "done"
  | "failed"
  | "escalated"
  | "split"
  | "merged";

export interface DoneWhenPredicate {
  predicate: string;
  weight: number;
}

export interface CompletionPredicateResult {
  predicate: string;
  weight: number;
  satisfied: boolean;
}

export interface TaskNode {
  id: string;
  title: string;
  deps: string[];
  doneWhen: string;
  doneWhenPredicates: DoneWhenPredicate[];
  state: TaskState;
  category: string;
  estimatedDuration: string;
  assignedRole: string;
  escalationPath: string;
  openQuestions: string[];
  createdAt: string;
  updatedAt: string;
}

export interface TaskRow extends TaskNode {
  source?: "session-taskgraph" | "workspace-record" | string;
  kind?: string;
  command?: string;
  shellId?: string;
  processStatus?: "running" | "exited" | "killed" | "spawnFailed" | "unavailable" | string;
  processAttachment?: "attached" | "detached" | string;
  processStatusReason?: string;
  canPollOutput?: boolean;
  canStop?: boolean;
  lifecycleStatusReason?: string;
  taskStorePath?: string;
  outputBytes?: number;
  outputPreview?: string;
}
