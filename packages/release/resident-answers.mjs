// Answers the one open gate process keeps for the runs that come after (GATE-CONTRACT NN25, NN26; Adrian: one open process, everything through it).
//
// A gate run used to start a child process for every question it had, the same ones on every run of an unchanged commit (witness fixture, Dell,
// 2026-11-19: 54 child processes on a warm check, three of them `pnpm config ...`). The resident gate (gate-serve.mjs) is the process that stays
// open, so it holds the answer; a run asks it over its socket, in process (serveRequest), and starts the tool only when it has none.
//
// An answer is never trusted on its key alone. It is stored with the files it depends on, each with a mark of what the file was BEFORE the tool
// was asked (the sha256 of its bytes, or "absent"; for a file over LARGE_FILE_BYTES its size and modification time), and it is given back only
// when every one of those files still has that mark. The key names the rest: the question, the directory, the environment. So an edited config
// file, a tool that was updated, another directory or another environment asks again. The store is the resident process's memory: it ends
// with that process and with any change of the gate's own code (gate-serve.mjs IDENTITY).
import fs from "node:fs";
import { createHash } from "node:crypto";
import { serveRequest } from "./gate-serve.mjs";

const sha256 = (value) => createHash("sha256").update(value).digest("hex");
/** A program file larger than this is marked by its size and modification time: hashing a 100 MB executable on every recall costs more than the child it saves. */
export const LARGE_FILE_BYTES = 4 * 1024 * 1024;

/** `[path, <mark>]`: what a dependency file is right now. The mark is `sha256:<hex>`, `stat:<size>:<mtimeMs>`, `absent` or `unreadable:<code>`. */
export function fileMark(file, { read = (f) => fs.readFileSync(f), stat = (f) => fs.statSync(f) } = {}) {
  try {
    const st = stat(file);
    if (!st.isFile()) return [file, `unreadable:not-a-file`];
    if (st.size > LARGE_FILE_BYTES) return [file, `stat:${st.size}:${Math.trunc(st.mtimeMs)}`];
    return [file, `sha256:${sha256(read(file))}`];
  } catch (error) { return [file, error?.code === "ENOENT" || error?.code === "ENOTDIR" ? "absent" : `unreadable:${error?.code ?? "unknown"}`]; }
}

/** The marks of `files` (distinct, in order). Taken BEFORE the tool is asked, so a file that changes while the tool runs cannot be stored as the answer's state. */
export function fileMarks(files, io = {}) {
  return [...new Set(files.filter((f) => typeof f === "string" && f))].map((f) => fileMark(f, io));
}

/** The key of one question: a sha256 over its parts (any JSON value). */
export const answerKey = (parts) => sha256(JSON.stringify(parts));

/**
 * The kept answer of `key`, or null: when the resident gate has none, cannot be reached, or any file the answer depends on no longer has the
 * mark it had when the answer was stored. `ask` is the transport (tests); the default asks the resident gate in process.
 */
export function recallAnswer(key, { env = process.env, ask = serveRequest, read, stat } = {}) {
  let reply = null;
  try { reply = ask({ op: "answer-get", key }, { env }); } catch { reply = null; }
  if (!reply || typeof reply.value !== "string") return null;
  let doc;
  try { doc = JSON.parse(reply.value); } catch { return null; }
  if (!doc || typeof doc.answer !== "string" || !Array.isArray(doc.files)) return null;
  const io = { ...(read ? { read } : {}), ...(stat ? { stat } : {}) };
  for (const mark of doc.files) {
    if (!Array.isArray(mark) || typeof mark[0] !== "string" || typeof mark[1] !== "string") return null;
    if (mark[1].startsWith("unreadable:")) return null; // a file that could not be read proves nothing either way
    if (fileMark(mark[0], io)[1] !== mark[1]) return null;
  }
  return doc.answer;
}

/**
 * Stores `answer` under `key` with `marks` (fileMarks taken before the tool was asked). Nothing is stored when a mark is unreadable. A store
 * that cannot be reached is not an error: the next run asks again.
 */
export function rememberAnswer(key, answer, marks, { env = process.env, ask = serveRequest } = {}) {
  if (typeof answer !== "string" || !Array.isArray(marks)) return;
  if (marks.some((m) => !Array.isArray(m) || typeof m[1] !== "string" || m[1].startsWith("unreadable:"))) return;
  try { ask({ op: "answer-put", key, value: JSON.stringify({ answer, files: marks }) }, { env }); } catch { /* not kept */ }
}
