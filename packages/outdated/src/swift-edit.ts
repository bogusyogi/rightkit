// In-place version edits for Package.swift and project.yml, matching the same
// dependency and requirement syntax `check` reads.

export interface SwiftEditResult {
  text: string;
  count: number;
}

function repoNameFromUrl(url: string): string {
  const last = url.replace(/\/+$/, "").split("/").pop() ?? url;
  return last.replace(/\.git$/, "");
}

/** Offsets of the argument text of each `.package(` call. */
function packageCallRanges(text: string): Array<{ start: number; end: number }> {
  const ranges: Array<{ start: number; end: number }> = [];
  let index = text.indexOf(".package(");
  while (index !== -1) {
    let depth = 0;
    let inString = false;
    const open = index + ".package".length;
    let end = open;
    for (let i = open; i < text.length; i++) {
      const ch = text[i];
      if (ch === '"') inString = !inString;
      if (inString) continue;
      if (ch === "(") depth++;
      if (ch === ")") {
        depth--;
        if (depth === 0) {
          end = i;
          break;
        }
      }
    }
    ranges.push({ start: open + 1, end });
    index = text.indexOf(".package(", end);
  }
  return ranges;
}

function assertSafe(version: string): void {
  if (!/^[0-9A-Za-z.+-]+$/.test(version)) throw new Error(`refusing unsafe version text: ${version}`);
}

/** Replace the `exact:`/`from:` requirement of dependency `name` when it equals `oldReq`. */
export function replaceSwiftPackageVersion(text: string, name: string, oldReq: string, newReq: string): SwiftEditResult {
  assertSafe(newReq);
  const edits: Array<{ start: number; end: number }> = [];
  for (const range of packageCallRanges(text)) {
    const call = text.slice(range.start, range.end);
    const url = /url:\s*"([^"]+)"/.exec(call)?.[1];
    if (!url || repoNameFromUrl(url) !== name) continue;
    const match = /\b(?:exact|from)\s*:\s*"([^"]+)"/.exec(call);
    if (!match || match[1] !== oldReq) continue;
    const valueStart = range.start + match.index + match[0].lastIndexOf(match[1]);
    edits.push({ start: valueStart, end: valueStart + match[1].length });
  }
  let out = text;
  for (const edit of [...edits].reverse()) out = out.slice(0, edit.start) + newReq + out.slice(edit.end);
  return { text: out, count: edits.length };
}

/** Same for XcodeGen `project.yml`: the version key in the block that declares the dependency's url. */
export function replaceProjectYmlVersion(text: string, name: string, oldReq: string, newReq: string): SwiftEditResult {
  assertSafe(newReq);
  const lines = text.split("\n");
  const offsets: number[] = [];
  let acc = 0;
  for (const line of lines) {
    offsets.push(acc);
    acc += line.length + 1;
  }
  const edits: Array<{ start: number; end: number }> = [];
  lines.forEach((line, i) => {
    const match = /^(\s*)url:\s*"?([^"\s]+)"?\s*\r?$/.exec(line);
    if (!match || repoNameFromUrl(match[2]) !== name) return;
    const indent = match[1].length;
    for (let j = i + 1; j < lines.length; j++) {
      const next = lines[j];
      if (next.trim() === "") continue;
      const nextIndent = /^\s*/.exec(next)?.[0].length ?? 0;
      if (nextIndent < indent) break;
      if (nextIndent !== indent) continue;
      const version = /^(\s*(?:from|exactVersion|exact|version|minorVersion|majorVersion)\s*:\s*"?)([^"\s]+)/.exec(next);
      if (version) {
        if (version[2] === oldReq) {
          const start = offsets[j] + version[1].length;
          edits.push({ start, end: start + version[2].length });
        }
        break;
      }
    }
  });
  let out = text;
  for (const edit of [...edits].reverse()) out = out.slice(0, edit.start) + newReq + out.slice(edit.end);
  return { text: out, count: edits.length };
}
