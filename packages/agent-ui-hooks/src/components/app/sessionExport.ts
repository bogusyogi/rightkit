import type { SessionRow, TranscriptMessage } from "@rightkit/agent-ui-core/lib/types";
import { projectName } from "@rightkit/agent-ui-core/lib/utils";
import { hostAppName } from "../../appName.js";

export interface SessionExportOptions {
  exportedAt?: string;
  costUsd?: number;
}

export interface StructuredSessionExport {
  schemaVersion: "coderight.session-export.v1";
  exportedAt: string;
  session: {
    id: string;
    title: string;
    model: string;
    workspaceRoot: string;
    createdAt: string;
    updatedAt: string;
    status?: string;
    workspaceTrust?: string;
  };
  cost: { totalUsd: number | null };
  transcript: TranscriptMessage[];
  toolCalls: Array<{
    id: string;
    name: string;
    argumentsJson: string;
    createdAt: string;
    result?: {
      createdAt: string;
      summary: string;
      isError: boolean;
      trust?: Extract<TranscriptMessage, { $kind: "tool" }>["trust"];
    };
  }>;
  diffs: Array<{ messageId: string; createdAt: string; content: string }>;
  trustVerdicts: Array<{
    messageId: string;
    messageKind: TranscriptMessage["$kind"];
    createdAt: string;
    trust: NonNullable<Extract<TranscriptMessage, { trust?: unknown }>["trust"]>;
  }>;
}

export function renderStructuredSessionExport(
  session: SessionRow,
  transcript: TranscriptMessage[],
  options: SessionExportOptions = {},
): StructuredSessionExport {
  const toolResults = new Map(
    transcript
      .filter(
        (message): message is Extract<TranscriptMessage, { $kind: "tool" }> =>
          message.$kind === "tool",
      )
      .map((message) => [message.toolCallId, message]),
  );
  const assistantMessages = transcript.filter(
    (message): message is Extract<TranscriptMessage, { $kind: "assistant" }> =>
      message.$kind === "assistant",
  );
  return {
    schemaVersion: "coderight.session-export.v1",
    exportedAt: options.exportedAt ?? new Date().toISOString(),
    session: {
      id: session.id,
      title: session.title,
      model: session.model,
      workspaceRoot: session.workspaceRoot,
      createdAt: session.createdAt,
      updatedAt: session.updatedAt,
      status: session.status,
      workspaceTrust: session.workspaceTrust,
    },
    cost: { totalUsd: typeof options.costUsd === "number" ? options.costUsd : null },
    transcript,
    toolCalls: assistantMessages.flatMap((message) =>
      message.toolCalls.map((call) => {
        const result = toolResults.get(call.id);
        return {
          id: call.id,
          name: call.name,
          argumentsJson: call.argumentsJson,
          createdAt: message.createdAt,
          result: result
            ? {
                createdAt: result.createdAt,
                summary: result.summary,
                isError: result.isError,
                trust: result.trust,
              }
            : undefined,
        };
      }),
    ),
    diffs: transcript.flatMap((message) =>
      "content" in message
        ? extractDiffBlocks(message.content).map((content) => ({
            messageId: message.id,
            createdAt: message.createdAt,
            content,
          }))
        : [],
    ),
    trustVerdicts: transcript.flatMap((message) =>
      "trust" in message && message.trust
        ? [
            {
              messageId: message.id,
              messageKind: message.$kind,
              createdAt: message.createdAt,
              trust: message.trust,
            },
          ]
        : [],
    ),
  };
}

export function renderStructuredSessionMarkdown(
  session: SessionRow,
  transcript: TranscriptMessage[],
  options: SessionExportOptions = {},
) {
  const exported = renderStructuredSessionExport(session, transcript, options);
  const lines = [
    `# ${hostAppName()} session export`,
    "",
    `- Session: ${session.title || session.id}`,
    `- Session ID: ${session.id}`,
    `- Model: ${session.model || "unknown"}`,
    `- Workspace: ${session.workspaceRoot || "unknown"}`,
    `- Exported: ${exported.exportedAt}`,
    "",
    "## Cost",
    "",
    `- Total USD: ${exported.cost.totalUsd ?? "unknown"}`,
    "",
    "## Transcript",
    "",
  ];

  for (const message of transcript) {
    if (message.$kind === "user") {
      lines.push(...markdownMessageSection("User", message.createdAt, message.content));
    } else if (message.$kind === "assistant") {
      lines.push(...markdownMessageSection("Assistant", message.createdAt, message.content));
    } else if (message.$kind === "compact") {
      lines.push(
        ...markdownMessageSection(
          "Compaction",
          message.createdAt,
          `${message.summary}\n\nDropped messages: ${message.droppedCount}`,
        ),
      );
    } else if (message.$kind === "tool") {
      lines.push(
        ...markdownMessageSection(
          "Tool",
          message.createdAt,
          `${message.summary}\n\nCall ID: ${message.toolCallId}\nStatus: ${
            message.isError ? "error" : "ok"
          }`,
        ),
      );
    }
  }

  if (transcript.length === 0) {
    lines.push("_No visible conversation messages yet._", "");
  }

  lines.push("## Tool Calls", "");
  if (exported.toolCalls.length === 0) {
    lines.push("_No tool calls recorded._", "");
  } else {
    for (const call of exported.toolCalls) {
      lines.push(
        `- ${call.name} (${call.id})`,
        `  - Created: ${call.createdAt}`,
        `  - Result: ${call.result ? (call.result.isError ? "error" : "ok") : "pending"}`,
        "",
      );
    }
  }

  lines.push("## Diffs", "");
  if (exported.diffs.length === 0) {
    lines.push("_No diff blocks recorded._", "");
  } else {
    for (const diff of exported.diffs) {
      lines.push(`### ${diff.messageId}`, "", "```diff", diff.content.trim(), "```", "");
    }
  }

  lines.push("## Trust Verdicts", "");
  if (exported.trustVerdicts.length === 0) {
    lines.push("_No trust verdicts recorded._", "");
  } else {
    for (const verdict of exported.trustVerdicts) {
      lines.push(
        `- ${verdict.messageKind} ${verdict.messageId}: ${verdict.trust.trust} from ${verdict.trust.source}`,
      );
    }
    lines.push("");
  }

  return `${lines.join("\n").trimEnd()}\n`;
}

export function renderTranscriptMarkdown(session: SessionRow, transcript: TranscriptMessage[]) {
  return renderStructuredSessionMarkdown(session, transcript);
}

export function markdownExportFilename(session: SessionRow) {
  const title = safeFilenamePart(session.title || projectName(session.workspaceRoot) || "session");
  return `coderight-${title}-${session.id.slice(0, 8)}.md`;
}

export function jsonExportFilename(session: SessionRow) {
  const title = safeFilenamePart(session.title || projectName(session.workspaceRoot) || "session");
  return `coderight-${title}-${session.id.slice(0, 8)}.json`;
}

export function downloadTextFile(filename: string, content: string, mimeType: string) {
  const blob = new Blob([content], { type: mimeType });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  document.body.appendChild(link);
  link.click();
  link.remove();
  URL.revokeObjectURL(url);
}

function markdownMessageSection(
  role: "User" | "Assistant" | "Compaction" | "Tool",
  createdAt: string,
  body: string,
) {
  return [`## ${role}`, "", `_${createdAt}_`, "", body.trim() || "_Empty message_", ""];
}

function extractDiffBlocks(content: string) {
  const blocks: string[] = [];
  const fenced = /```diff\s*([\s\S]*?)```/gi;
  let match = fenced.exec(content);
  while (match) {
    blocks.push(match[1].trim());
    match = fenced.exec(content);
  }
  return blocks;
}

export function safeFilenamePart(value: string) {
  return (
    value
      .trim()
      .replace(/[^a-zA-Z0-9._-]+/g, "-")
      .replace(/^-+|-+$/g, "")
      .slice(0, 64) || "session"
  );
}
