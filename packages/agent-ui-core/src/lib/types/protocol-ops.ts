export interface AgentRosterRow {
  subSessionId: string;
  role: string;
  status: "running" | "done" | "failed" | "cancelled";
  tokensIn: number;
  tokensOut: number;
  cacheRead: number;
  toolUses: number;
  elapsedMs: number;
  lastTools: string[];
}

export type UserQuestionOption = { label: string; description: string };
export type UserQuestion = {
  question: string;
  header: string;
  options: UserQuestionOption[];
  multiSelect: boolean;
};

// ── Command operations (Ops) ────────────────────────────────────────────────

export type Op =
  | {
      $kind: "submitUserMessage";
      opId: string;
      sessionId: string;
      text: string;
    }
  | { $kind: "interruptRun"; opId: string; sessionId: string }
  | { $kind: "requestCompaction"; opId: string; sessionId: string }
  | {
      $kind: "injectSteeringMessage";
      opId: string;
      sessionId: string;
      content: string;
      injectedAt: string;
      injectedBy: string;
    }
  | {
      $kind: "resolveToolApproval";
      opId: string;
      sessionId: string;
      toolCallId: string;
      approved: boolean;
      reason?: string;
    };
