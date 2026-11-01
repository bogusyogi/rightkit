// Machine-facing text that leaks into the human transcript.
//
// The engine and the bounded-run harness emit two shapes of text that are
// written *for the model* — a spilled-tool-output stub and a bracketed
// status/gate notice — and both were being rendered to the user verbatim,
// styled like an ordinary reply (UI-ISSUES §2 and §13). These pure detectors
// let the transcript render them as what they are: a compact "N KB output"
// affordance and a dim status line, instead of a wall of hashes or a
// bracketed sentence dressed up as prose.

export interface ExternalizedOutput {
  bytes: number;
  refId: string;
  path: string;
  /** The head/tail preview the engine already inlined, minus the plumbing header. */
  preview: string;
}

/** Human-readable byte size, e.g. 12667 -> "12.4 KB". */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return `${bytes} B`;
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${kb.toFixed(kb < 10 ? 1 : 0)} KB`;
  return `${(kb / 1024).toFixed(1)} MB`;
}

/**
 * Detect the engine's spilled-output stub
 * (`tool_result_store.rs::large_output_preview`) and pull the parts a human
 * cares about out of the model-facing header. Returns null for ordinary
 * output so the caller renders it unchanged.
 */
export function parseExternalizedOutput(text: string): ExternalizedOutput | null {
  if (!text.startsWith("[Large tool output externalized]")) return null;
  const bytes = Number.parseInt(/^Bytes:\s*(\d+)/m.exec(text)?.[1] ?? "", 10);
  const refId = /^Reference:\s*ref:(\S+)/m.exec(text)?.[1] ?? "";
  const path = /^Full output:\s*(\S+)/m.exec(text)?.[1] ?? "";
  // Everything from the first "--- head ---" onward is the actual preview the
  // engine inlined; drop the four-line plumbing header above it.
  const headIndex = text.indexOf("--- head ---");
  const preview = headIndex >= 0 ? text.slice(headIndex) : "";
  if (!Number.isFinite(bytes)) return null;
  return { bytes, refId, path, preview };
}

/**
 * Strip agent bookkeeping from tool output before it is shown to a person.
 *
 * Two kinds of noise leak through verbatim (UI-ISSUES §2):
 *  - `Todos (2/7 done):` plus its `[x]`/`[~]`/`[ ]` lines. The same list is
 *    rendered as structured state in the todo dock above the composer, so
 *    repeating the raw checklist inside a tool blob is pure duplication.
 *  - `---EXIT 0---`, a sentinel some harness commands echo to mark the end of
 *    a pipeline. It is protocol, not output.
 *
 * Everything else is left byte-for-byte: this only removes lines it can
 * positively identify, and never edits the middle of a line.
 */
export function stripAgentBookkeeping(text: string): string {
  const lines = text.split("\n");
  const kept: string[] = [];
  let inTodoBlock = false;
  for (const line of lines) {
    if (/^Todos \(\d+\/\d+ done\):\s*$/.test(line)) {
      inTodoBlock = true;
      continue;
    }
    if (inTodoBlock) {
      // Consume the checklist body; the first line that is not a checkbox item
      // (or blank) ends the block.
      if (/^\s*\[[ x~]\]\s/.test(line) || line.trim() === "") continue;
      inTodoBlock = false;
    }
    if (/^-{2,}\s*EXIT\s+-?\d+\s*-{2,}$/.test(line.trim())) continue;
    kept.push(line);
  }
  return kept.join("\n").trim();
}

/**
 * Detect a message whose entire body is a single bracketed machine notice —
 * the bounded-run loop-safety gate and its siblings. Conservative on purpose:
 * it only matches when the whole trimmed message is one `[ … ]` block, so a
 * reply that merely *contains* a bracketed aside is left as prose.
 */
export function parseBracketedNotice(text: string): string | null {
  const trimmed = text.trim();
  if (!trimmed.startsWith("[") || !trimmed.endsWith("]")) return null;
  // Reject if there is a second top-level "]" before the end — that means the
  // brackets are inline punctuation inside a longer message, not a wrapper.
  const firstClose = trimmed.indexOf("]");
  if (firstClose !== trimmed.length - 1) return null;
  const inner = trimmed.slice(1, -1).trim();
  if (!inner) return null;
  // Only treat it as a status notice when it reads like one; otherwise a user
  // pasting "[TODO]" would get restyled.
  if (!/gate|failing closed|could not verify|not archived|externalized|diagnostic/i.test(inner)) {
    return null;
  }
  return inner;
}
