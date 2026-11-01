// Goal-update line parser — tolerant of id variations but strict on op shape.
// Returns one GoalUpdate per parsed bracket-tagged line; bad lines log via
// console.debug and the partial valid lines still come back. Empty input → [].
//
// Canonical: "[goal-XYZ: complete] evidence text"
//             "[goal-XYZ: block]    reason text"
//             "[goal-XYZ: progress] note text"
// Variants:  "[goal-XYZ:complete] …"  (no space before id)
//            "[goal-ABC:complete] …"  (uppercase id kept as-is)
//            "[goal-XYZ : complete] …"  (extra spaces trimmed)

export type GoalUpdate =
  | { id: string; op: "complete"; evidence: string }
  | { id: string; op: "block"; reason: string }
  | { id: string; op: "progress"; note: string };

// Match a `[goal-<id>: <op>]` (or no-space variant) header. ID is one or more
// non-whitespace, non-`]`, non-`:` characters. Op is one of complete/block/progress
// (case-insensitive). Whatever follows on the line is the payload.
const HEADER_RE = /^\[\s*goal-([^\]\s:]+)\s*:\s*(complete|block|progress)\s*\](\s*)(.*)$/i;

const OPS = new Set(["complete", "block", "progress"] as const);

export function parseGoals(text: string): GoalUpdate[] {
  if (!text || text.trim().length === 0) return [];

  const updates: GoalUpdate[] = [];
  for (const rawLine of text.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line) continue;
    const match = HEADER_RE.exec(line);
    if (!match) {
      console.debug("[parseGoals] malformed line skipped:", rawLine);
      continue;
    }
    const [, rawId, rawOp, , rest] = match;
    const id = rawId.trim();
    const opLower = rawOp.toLowerCase();
    if (!OPS.has(opLower as "complete" | "block" | "progress")) {
      console.debug("[parseGoals] unknown op:", rawOp);
      continue;
    }
    const payload = rest.trim();
    if (opLower === "complete") {
      updates.push({ id, op: "complete", evidence: payload });
    } else if (opLower === "block") {
      updates.push({ id, op: "block", reason: payload });
    } else {
      updates.push({ id, op: "progress", note: payload });
    }
  }
  return updates;
}
