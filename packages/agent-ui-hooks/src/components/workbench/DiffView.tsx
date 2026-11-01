import { useMemo, useState } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";

export type DiffRow =
  | { kind: "hunk"; text: string }
  | { kind: "file"; text: string }
  | { kind: "add"; text: string; oldN: number | null; newN: number | null }
  | { kind: "del"; text: string; oldN: number | null; newN: number | null }
  | { kind: "ctx"; text: string; oldN: number | null; newN: number | null };

export type DiffHunkDecision = "approve" | "reject";

export interface DiffHunk {
  id: string;
  index: number;
  filePath: string | null;
  header: string;
  patch: string;
  rows: DiffRow[];
}

export interface StageDiffHunkPayload {
  toolCallId: string;
  decision: DiffHunkDecision;
  hunkId: string;
  hunkIndex: number;
  filePath: string | null;
  hunkHeader: string;
  patch: string;
}

export function looksLikeDiff(text: string): boolean {
  if (!text.includes("\n")) return false;
  const lines = text.split("\n");
  if (
    lines.some((l) => l.startsWith("@@") || l.startsWith("```diff") || l.startsWith("diff --git"))
  ) {
    return true;
  }
  // Plain text with a stray bullet or a `--- head ---` divider is not a diff:
  // require both added and removed lines before taking the diff path.
  const hasAdded = lines.some((l) => /^\+(?!\+\+)/.test(l));
  const hasRemoved = lines.some((l) => /^-(?!--)/.test(l));
  return hasAdded && hasRemoved;
}

function cleanDiffText(text: string): string {
  return text.replace(/```diff\n?|```/g, "");
}

export function parseDiffRows(text: string): DiffRow[] {
  const lines = cleanDiffText(text).split("\n");
  let oldN = 0;
  let newN = 0;
  let insideHunk = false;
  const rows: DiffRow[] = [];
  for (const line of lines) {
    const hunk = /^@@\s*-(\d+)(?:,\d+)?\s+\+(\d+)(?:,\d+)?\s*@@/.exec(line);
    if (line.startsWith("diff --git")) {
      insideHunk = false;
      rows.push({ kind: "file", text: line });
    } else if (hunk) {
      oldN = Number(hunk[1]);
      newN = Number(hunk[2]);
      insideHunk = true;
      rows.push({ kind: "hunk", text: line });
    } else if (
      !insideHunk &&
      /^(\+\+\+|---|index |new file|deleted file|rename |similarity )/.test(line)
    ) {
      rows.push({ kind: "file", text: line });
    } else if (line.startsWith("+")) {
      rows.push({ kind: "add", text: line.slice(1), oldN: null, newN: newN++ });
    } else if (line.startsWith("-")) {
      rows.push({ kind: "del", text: line.slice(1), oldN: oldN++, newN: null });
    } else {
      rows.push({
        kind: "ctx",
        text: line.startsWith(" ") ? line.slice(1) : line,
        oldN: oldN++,
        newN: newN++,
      });
    }
  }
  return rows;
}

function filePathFromLine(line: string): string | null {
  const plusPath = /^\+\+\+\s+b\/(.+)$/.exec(line);
  if (plusPath) return plusPath[1];
  const diffPath = /^diff --git\s+a\/(.+?)\s+b\/(.+)$/.exec(line);
  if (diffPath) return diffPath[2];
  return null;
}

function isFileHeaderLine(line: string): boolean {
  return /^(\+\+\+|---|diff --git|index |new file|deleted file|rename |similarity )/.test(line);
}

export function parseDiffHunks(text: string): DiffHunk[] {
  const lines = cleanDiffText(text).split("\n");
  const hunks: DiffHunk[] = [];
  let fileHeaderLines: string[] = [];
  let filePath: string | null = null;
  let currentHeader: string | null = null;
  let currentLines: string[] = [];

  const flush = () => {
    if (!currentHeader) return;
    const hunkIndex = hunks.length;
    const id = `${filePath ?? "diff"}:${hunkIndex}`;
    const patchLines = [...fileHeaderLines, ...currentLines];
    hunks.push({
      id,
      index: hunkIndex,
      filePath,
      header: currentHeader,
      patch: patchLines.join("\n"),
      rows: parseDiffRows(currentLines.join("\n")),
    });
    currentHeader = null;
    currentLines = [];
  };

  for (const line of lines) {
    if (line.startsWith("@@")) {
      flush();
      currentHeader = line;
      currentLines = [line];
      continue;
    }

    if (currentHeader && !line.startsWith("diff --git")) {
      currentLines.push(line);
      continue;
    }

    if (isFileHeaderLine(line)) {
      if (currentHeader) flush();
      if (line.startsWith("diff --git")) {
        fileHeaderLines = [line];
      } else {
        fileHeaderLines.push(line);
      }
      filePath = filePathFromLine(line) ?? filePath;
      continue;
    }

    if (currentHeader) {
      currentLines.push(line);
    }
  }
  flush();
  return hunks;
}

export function buildStageDiffHunkPayload(
  toolCallId: string,
  hunk: Pick<DiffHunk, "id" | "index" | "filePath" | "header" | "patch">,
  decision: DiffHunkDecision,
): StageDiffHunkPayload {
  return {
    toolCallId,
    decision,
    hunkId: hunk.id,
    hunkIndex: hunk.index,
    filePath: hunk.filePath,
    hunkHeader: hunk.header,
    patch: hunk.patch,
  };
}

function rowTone(row: DiffRow, side: "before" | "after"): string {
  if (row.kind === "del" && side === "before") {
    return "bg-[color-mix(in_srgb,var(--color-danger)_13%,transparent)] text-text-2";
  }
  if (row.kind === "add" && side === "after") {
    return "bg-[color-mix(in_srgb,var(--color-accent-2)_13%,transparent)] text-text-2";
  }
  if (row.kind === "ctx") return "text-text-2";
  return "text-text-3";
}

function renderSplitCell(row: DiffRow, side: "before" | "after") {
  if (row.kind === "hunk" || row.kind === "file") return null;
  const hidden =
    (row.kind === "add" && side === "before") || (row.kind === "del" && side === "after");
  const lineNumber = side === "before" ? row.oldN : row.newN;
  const sign =
    hidden || row.kind === "ctx" ? " " : side === "before" && row.kind === "del" ? "-" : "+";
  const signCls = sign === "-" ? "text-danger" : sign === "+" ? "text-accent-2" : "text-text-3";

  return (
    <div className={`grid min-h-6 grid-cols-[44px_18px_minmax(0,1fr)] ${rowTone(row, side)}`}>
      <span className="select-none px-1 text-right text-text-3 tabular-nums">
        {hidden ? "" : lineNumber}
      </span>
      <span className={`select-none text-center ${signCls}`}>{sign}</span>
      <span className="whitespace-pre-wrap break-words pr-3">{hidden ? " " : row.text || " "}</span>
    </div>
  );
}

function renderFlatRows(rows: DiffRow[]) {
  return rows.map((r, i) => {
    if (r.kind === "hunk") {
      return (
        <div
          // biome-ignore lint/suspicious/noArrayIndexKey: diff rows have no stable id.
          key={i}
          className="select-none bg-panel-2 px-3 py-[1px] text-text-3"
        >
          {r.text}
        </div>
      );
    }
    if (r.kind === "file") {
      return (
        // biome-ignore lint/suspicious/noArrayIndexKey: diff rows have no stable id.
        <div key={i} className="select-none px-3 py-[1px] text-text-3">
          {r.text || " "}
        </div>
      );
    }
    const rowBg =
      r.kind === "add"
        ? "bg-[color-mix(in_srgb,var(--color-accent-2)_13%,transparent)]"
        : r.kind === "del"
          ? "bg-[color-mix(in_srgb,var(--color-danger)_13%,transparent)]"
          : "";
    const sign = r.kind === "add" ? "+" : r.kind === "del" ? "-" : " ";
    const signCls =
      r.kind === "add" ? "text-accent-2" : r.kind === "del" ? "text-danger" : "text-text-3";
    return (
      // biome-ignore lint/suspicious/noArrayIndexKey: diff rows have no stable id.
      <div key={i} className={`flex ${rowBg}`}>
        <span className="w-9 flex-none select-none px-1 text-right text-text-3 tabular-nums">
          {r.oldN ?? ""}
        </span>
        <span className="w-9 flex-none select-none px-1 text-right text-text-3 tabular-nums">
          {r.newN ?? ""}
        </span>
        <span className={`w-4 flex-none select-none text-center ${signCls}`}>{sign}</span>
        <span className="whitespace-pre-wrap break-words pr-3 text-text-2">{r.text || " "}</span>
      </div>
    );
  });
}

// Header strip with a Copy button (raw diff text). Mirrors the
// TranscriptBubble copy pattern: navigator.clipboard.writeText →
// Copied! / Copy failed acknowledgement that self-clears after ~1.5s.
function DiffHeader({ text, right }: { text: string; right?: React.ReactNode }) {
  const [copied, setCopied] = useState(false);
  const [copyFailed, setCopyFailed] = useState(false);

  function copyDiff() {
    navigator.clipboard
      .writeText(text ?? "")
      .then(() => {
        setCopied(true);
        setCopyFailed(false);
        window.setTimeout(() => setCopied(false), 1500);
      })
      .catch(() => {
        setCopyFailed(true);
        setCopied(false);
        window.setTimeout(() => setCopyFailed(false), 2000);
      });
  }

  return (
    <div className="flex items-center gap-2 border-b border-line bg-panel-2 px-3 py-1.5 text-[11px] font-semibold uppercase tracking-wide text-text-3">
      <span className="min-w-0 flex-1 truncate">Diff</span>
      {right}
      <button
        type="button"
        title={copyFailed ? "Copy failed" : copied ? "Copied" : "Copy diff"}
        aria-label={copyFailed ? "Copy failed" : copied ? "Copied diff" : "Copy diff"}
        data-testid="diff-copy-button"
        className="cr-control-pill inline-flex h-6 items-center gap-1 rounded-md border border-line bg-panel px-2 text-[11px] font-semibold text-text-3 hover:border-line-2 hover:text-text"
        onClick={copyDiff}
      >
        <Icon name={copyFailed ? "flag" : copied ? "check" : "copy"} size={11} />
        {copied ? "Copied!" : copyFailed ? "Failed" : "Copy"}
      </button>
    </div>
  );
}

export default function DiffView({
  text,
  onStageHunk,
}: {
  text: string;
  onStageHunk?: (hunk: DiffHunk, decision: DiffHunkDecision) => Promise<void> | void;
}) {
  const rows = useMemo(() => parseDiffRows(text), [text]);
  const hunks = useMemo(() => parseDiffHunks(text), [text]);
  const [hunkStates, setHunkStates] = useState<
    Record<string, DiffHunkDecision | "pending" | "error">
  >({});

  const handleHunkDecision = async (hunk: DiffHunk, decision: DiffHunkDecision) => {
    if (!onStageHunk) return;
    setHunkStates((current) => ({ ...current, [hunk.id]: "pending" }));
    try {
      await onStageHunk(hunk, decision);
      setHunkStates((current) => ({ ...current, [hunk.id]: decision }));
    } catch {
      setHunkStates((current) => ({ ...current, [hunk.id]: "error" }));
    }
  };

  if (hunks.length === 0) {
    return (
      <div className="m-0 overflow-hidden rounded-lg border border-line bg-bg font-mono text-[11px] leading-[1.65]">
        <DiffHeader text={text} right={null} />
        <div className="max-h-[360px] overflow-auto">{renderFlatRows(rows)}</div>
      </div>
    );
  }

  return (
    <div className="m-0 flex max-h-[460px] flex-col overflow-hidden rounded-lg border border-line bg-bg font-mono text-[11px] leading-[1.65]">
      <DiffHeader text={text} />
      <div className="min-h-0 flex-1 overflow-auto">
        {hunks.map((hunk) => {
          const state = hunkStates[hunk.id];
          const actionDisabled = state === "pending";
          return (
            <div key={hunk.id} className="border-b border-line last:border-b-0">
              <div className="flex min-h-9 items-center gap-2 bg-panel-2 px-3 py-1.5 text-text-3">
                <span className="min-w-0 flex-1 truncate">
                  {hunk.filePath ? `${hunk.filePath}  ${hunk.header}` : hunk.header}
                </span>
                {state && state !== "pending" ? (
                  <span className="select-none rounded-md border border-line bg-panel px-2 py-0.5 text-[10px] font-semibold uppercase text-text-3">
                    {state === "error" ? "Failed" : state === "approve" ? "Approved" : "Rejected"}
                  </span>
                ) : null}
                {onStageHunk ? (
                  <div className="flex items-center gap-1">
                    <button
                      type="button"
                      title="Approve hunk"
                      aria-label={`Approve hunk ${hunk.index + 1}`}
                      disabled={actionDisabled}
                      className="inline-flex h-7 items-center gap-1 rounded-md border border-line bg-panel px-2 text-[11px] font-semibold text-accent-2 hover:border-line-2 hover:bg-panel-2 disabled:cursor-not-allowed disabled:opacity-50"
                      onClick={() => void handleHunkDecision(hunk, "approve")}
                    >
                      <Icon name="check" size={13} />
                      Approve
                    </button>
                    <button
                      type="button"
                      title="Reject hunk"
                      aria-label={`Reject hunk ${hunk.index + 1}`}
                      disabled={actionDisabled}
                      className="inline-flex h-7 items-center gap-1 rounded-md border border-line bg-panel px-2 text-[11px] font-semibold text-danger hover:border-line-2 hover:bg-panel-2 disabled:cursor-not-allowed disabled:opacity-50"
                      onClick={() => void handleHunkDecision(hunk, "reject")}
                    >
                      <Icon name="close" size={13} />
                      Reject
                    </button>
                  </div>
                ) : null}
              </div>
              <div className="grid min-w-[620px] grid-cols-2 border-b border-line bg-panel text-[10px] font-semibold uppercase text-text-3">
                <div className="border-r border-line px-3 py-1">Before</div>
                <div className="px-3 py-1">After</div>
              </div>
              <div>
                {hunk.rows
                  .filter((row) => row.kind !== "hunk" && row.kind !== "file")
                  .map((row, rowIndex) => (
                    <div
                      // biome-ignore lint/suspicious/noArrayIndexKey: diff rows have no stable id.
                      key={rowIndex}
                      className="grid min-w-[620px] grid-cols-2"
                    >
                      <div className="border-r border-line">{renderSplitCell(row, "before")}</div>
                      <div>{renderSplitCell(row, "after")}</div>
                    </div>
                  ))}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
