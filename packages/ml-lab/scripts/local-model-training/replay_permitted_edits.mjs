import { createHash } from "node:crypto";

const slice = (value, start, end) => value.slice(start, end);
const sha256 = (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`;

function anchoredEdit(edit) {
  if (edit.op === "replace_range") {
    return { start: edit.start, end: edit.end, expected: edit.expected_text, replacement: edit.replacement_text ?? "" };
  }
  if (["replace_literal", "delete_literal"].includes(edit.op) && edit.input_span && Number.isInteger(edit.input_span.start) && Number.isInteger(edit.input_span.end)) {
    return { start: edit.input_span.start, end: edit.input_span.end, expected: edit.input_span.text, replacement: edit.op === "delete_literal" ? "" : edit.replacement ?? "" };
  }
  return null;
}

function capitalizeFirst(value) {
  return value.replace(/\p{Ll}/u, (letter) => letter.toLocaleUpperCase("en-US"));
}

function capitalizeAfterOverlap(value) {
  return value.replace(/^(.*?[.!?]\s+)(\p{Ll})/u, (_, prefix, letter) => `${prefix}${letter.toLocaleUpperCase("en-US")}`);
}

export function replayPermittedEdits(input, edits = []) {
  if (typeof input !== "string" || !Array.isArray(edits)) throw new TypeError("input must be a string and edits must be an array");
  const supported = new Set(["replace_range", "replace_literal", "delete_literal", "capitalize_first", "capitalize_after_overlap", "append_terminal"]);
  for (const edit of edits) if (!supported.has(edit?.op)) throw new Error(`unsupported permitted edit: ${edit?.op ?? "<missing>"}`);

  const anchored = edits.map(anchoredEdit).filter(Boolean).sort((left, right) => right.start - left.start || right.end - left.end);
  let output = input;
  let previousStart = input.length + 1;
  for (const edit of anchored) {
    if (!Number.isInteger(edit.start) || !Number.isInteger(edit.end) || edit.start < 0 || edit.end < edit.start || edit.end > input.length) throw new Error("invalid permitted edit range");
    if (edit.end > previousStart) throw new Error("overlapping permitted edit ranges");
    if (slice(input, edit.start, edit.end) !== edit.expected) throw new Error(`expected_text mismatch at ${edit.start}:${edit.end}`);
    output = `${slice(output, 0, edit.start)}${edit.replacement}${slice(output, edit.end)}`;
    previousStart = edit.start;
  }

  for (const edit of edits) {
    if (["replace_literal", "delete_literal"].includes(edit.op) && !anchoredEdit(edit)) {
      const expected = edit.input_span?.text ?? edit.from;
      if (typeof expected !== "string" || !expected || output.indexOf(expected) < 0 || output.indexOf(expected) !== output.lastIndexOf(expected)) throw new Error(`literal authority must match exactly once: ${expected ?? "<missing>"}`);
      output = output.replace(expected, edit.op === "delete_literal" ? "" : edit.replacement ?? edit.to ?? "");
    } else if (edit.op === "capitalize_first") {
      output = capitalizeFirst(output);
    } else if (edit.op === "capitalize_after_overlap") {
      output = capitalizeAfterOverlap(output);
    } else if (edit.op === "append_terminal") {
      const mark = edit.mark ?? edit.replacement ?? ".";
      if (!/[.!?]$/u.test(output)) output += mark;
    }
  }
  return output;
}

export function buildReplayEvidence({ input, proposedOutput, edits = [] }) {
  const authorized = replayPermittedEdits(input, edits);
  return { authorized_output_hash: sha256(authorized), matches: proposedOutput === authorized };
}
