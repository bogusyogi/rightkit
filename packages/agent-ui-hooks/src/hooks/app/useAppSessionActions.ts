import { answerQuestion, compactSession, getTranscript, resolvePlan } from "@rightkit/agent-ui-core/lib/daemon";
import type { DaemonConfig, SessionRow, SupportBundleDownload, UserQuestion } from "@rightkit/agent-ui-core/lib/types";
import {
  downloadTextFile,
  jsonExportFilename,
  markdownExportFilename,
  renderStructuredSessionExport,
  renderTranscriptMarkdown,
} from "../../components/app/sessionExport";
import { messageOf } from "@rightkit/agent-ui-core/lib/utils";

interface PendingQuestion {
  questionId: string;
  questions: UserQuestion[];
}

interface PendingPlan {
  planId: string;
  plan: string;
}

interface UseAppSessionActionsParams {
  config: DaemonConfig | null;
  selected: SessionRow | null;
  pendingQuestion: PendingQuestion | null;
  setPendingQuestion: (value: PendingQuestion | null) => void;
  pendingPlan: PendingPlan | null;
  setPendingPlan: (value: PendingPlan | null) => void;
  sessionExportPending: boolean;
  setSessionExportPending: (value: boolean) => void;
  setIsWorking: (value: boolean) => void;
  setStatus: (value: string) => void;
  sessionCostUsd: number;
}

export function downloadSupportBundle(bundle: SupportBundleDownload) {
  const binary = atob(bundle.contentBase64);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  const url = URL.createObjectURL(new Blob([bytes], { type: bundle.contentType }));
  const link = document.createElement("a");
  link.href = url;
  link.download = bundle.fileName;
  document.body.appendChild(link);
  link.click();
  link.remove();
  URL.revokeObjectURL(url);
}

export function useAppSessionActions({
  config,
  selected,
  pendingQuestion,
  setPendingQuestion,
  pendingPlan,
  setPendingPlan,
  sessionExportPending,
  setSessionExportPending,
  setIsWorking,
  setStatus,
  sessionCostUsd,
}: UseAppSessionActionsParams) {
  async function exportSessionMarkdown() {
    if (!config || !selected || sessionExportPending) return;
    setSessionExportPending(true);
    try {
      const transcript = await getTranscript(config, selected.id);
      const markdown = renderTranscriptMarkdown(selected, transcript);
      downloadTextFile(markdownExportFilename(selected), markdown, "text/markdown;charset=utf-8");
      setStatus("Session Markdown exported");
    } catch (err) {
      setStatus(messageOf(err, "Session export failed"));
    } finally {
      setSessionExportPending(false);
    }
  }

  async function exportSessionJson() {
    if (!config || !selected || sessionExportPending) return;
    setSessionExportPending(true);
    try {
      const transcript = await getTranscript(config, selected.id);
      const structured = renderStructuredSessionExport(selected, transcript, {
        costUsd: sessionCostUsd,
      });
      downloadTextFile(
        jsonExportFilename(selected),
        JSON.stringify(structured, null, 2),
        "application/json;charset=utf-8",
      );
      setStatus("Session JSON exported");
    } catch (err) {
      setStatus(messageOf(err, "Session export failed"));
    } finally {
      setSessionExportPending(false);
    }
  }

  async function compact() {
    if (!config || !selected) return;
    try {
      await compactSession(config, selected.id);
      setStatus("Compacting...");
    } catch (err) {
      setStatus(messageOf(err, "Compact failed"));
    }
  }

  async function submitQuestionAnswers(answers: Record<string, string | string[]>) {
    if (!config || !selected || !pendingQuestion) return;
    try {
      await answerQuestion(config, selected.id, pendingQuestion.questionId, answers);
      setPendingQuestion(null);
      setIsWorking(true);
      setStatus("Answered");
    } catch (err) {
      setStatus(messageOf(err, "Answer failed"));
    }
  }

  async function submitPlanDecision(approved: boolean) {
    if (!config || !selected || !pendingPlan) return;
    try {
      await resolvePlan(config, selected.id, pendingPlan.planId, approved);
      setPendingPlan(null);
      setIsWorking(true);
      setStatus(approved ? "Plan approved" : "Continuing planning");
    } catch (err) {
      setStatus(messageOf(err, "Plan decision failed"));
    }
  }

  return {
    compact,
    exportSessionJson,
    exportSessionMarkdown,
    submitQuestionAnswers,
    submitPlanDecision,
  };
}
