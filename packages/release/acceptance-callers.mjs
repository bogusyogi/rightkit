// GC-A27 NN13: every caller of `rightkit-gate check` in RightKit takes the installed gate's answer the way it is written (rightkit#1695: gate 0.6.67
// made a content-only check exit 3; CI, publish admission, publish-app and every consumer app's check job read exit 0 and broke, while the twelve
// lines stayed green because the acceptance run only ever called `--to compile`).
//
// Three parts, all decided here, none by the runner:
//   1. discovery: every file of RightKit that names `rightkit-gate` outside the gate's own package is listed in the cases file, either as a caller or as
//      a file that only names it (with the reason). A new file that names the gate fails NN13 until someone decides which it is.
//   2. drift: each caller's file still contains the exact command text the cases file records, so a caller that changed its call is looked at again.
//   3. the answer: each caller's command runs against the fixture's pinned commit with the installed gate, and the caller's own rule is applied to the
//      result: "exit0" callers (a shell step under set -e, an npm script, the rendered check job) need exit 0; "admission" callers apply their own
//      exported verdict function, which must accept the full run and refuse the content-only run (the PARTIAL output).
import { execFileSync, spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

export const PASS = "pass", FAIL = "fail";

/** Files of `repo` (git-tracked) that name `rightkit-gate`, outside the gate's own package, its tests and the docs. */
export function filesNamingGate(repo, { git = (args) => execFileSync("git", ["-C", repo, ...args], { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 }) } = {}) {
  let out = "";
  try { out = git(["grep", "-l", "rightkit-gate", "--", ".", ":!packages/release/**", ":!docs/**", ":!**/*.md", ":!**/*.test.mjs", ":!**/CHANGELOG*"]); } catch (e) { if (e.status !== 1) throw e; }
  return out.split(/\r?\n/).filter(Boolean).map((f) => f.replaceAll("\\", "/")).sort();
}

/** Discovery and drift: the problems with the cases file's caller list against the repository's files. */
export function callerListProblems(defs, repo, { files = filesNamingGate(repo), read = (f) => fs.readFileSync(path.join(repo, f), "utf8") } = {}) {
  const problems = [];
  const listed = new Set([...(defs.callers ?? []).map((c) => c.file), ...Object.keys(defs.namesOnly ?? {})]);
  for (const f of files) if (!listed.has(f)) problems.push(`${f} names rightkit-gate and is in neither callers nor namesOnly of the cases file: decide whether it calls the gate`);
  for (const c of defs.callers ?? []) {
    let text = null; try { text = read(c.file); } catch { problems.push(`caller ${c.id}: ${c.file} is gone`); continue; }
    if (!text.includes(c.marker)) problems.push(`caller ${c.id}: ${c.file} no longer contains its recorded call \`${c.marker}\`; the call changed and must be judged again`);
  }
  return problems;
}

/** Runs one gate invocation in the fixture; `{sha}` in an argument becomes the pinned commit. */
/** Runs one gate invocation in `dir`; `{sha}` in an argument becomes `sha`. */
export function runGateFor(gate, dir, args, sha, { env = process.env, spawn = spawnSync } = {}) {
  const argv = args.map((a) => a.replaceAll("{sha}", sha));
  const r = spawn(process.execPath, [gate, ...argv], { cwd: dir, env, encoding: "utf8", maxBuffer: 256 * 1024 * 1024, windowsHide: true });
  return { sha, args: argv, status: r.status, stdout: String(r.stdout ?? ""), stderr: String(r.stderr ?? ""), error: r.error ?? null };
}

const tail = (r) => `${r.stderr}\n${r.stdout}`.trim().split(/\r?\n/).slice(-3).join(" / ").slice(0, 400);

/** Judges every caller. `run(caller, args)` runs the gate where that caller runs and returns the result with the commit it judged (`sha`);
 * `verdictOf(c)` loads an admission caller's verdict function. */
export async function judgeCallers(defs, repo, { run, verdictOf = async (c) => (await import(pathToFileURL(path.join(repo, c.file)).href))[c.verdictExport], listProblems = callerListProblems(defs, repo) } = {}) {
  const fails = [...listProblems]; const seen = [];
  for (const c of defs.callers ?? []) {
    if (c.kind === "exit0") {
      const r = run(c, c.args);
      if (r.error || r.status !== 0) fails.push(`caller ${c.id} (${c.file}) needs exit 0 for the clean commit ${r.sha?.slice(0, 12)} of ${c.repo}; \`rightkit-gate ${r.args.join(" ")}\` exited ${r.error ? r.error.code ?? r.error.message : r.status}: ${tail(r)}`);
      else seen.push(c.id);
    } else if (c.kind === "admission") {
      let decide; try { decide = await verdictOf(c); } catch (e) { fails.push(`caller ${c.id}: cannot load ${c.verdictExport} from ${c.file}: ${e.message}`); continue; }
      if (typeof decide !== "function") { fails.push(`caller ${c.id}: ${c.file} exports no function ${c.verdictExport}`); continue; }
      const full = run(c, c.args);
      try { decide(full, full.sha, c.scope ?? []); seen.push(c.id); } catch (e) { fails.push(`caller ${c.id} (${c.file}) refused the full gate run of the clean commit ${full.sha?.slice(0, 12)} of ${c.repo}: ${e.message}`); }
      const partial = run(c, c.refuseArgs);
      let admitted = false; try { decide(partial, partial.sha, c.scope ?? []); admitted = true; } catch { /* refused, as it must */ }
      if (admitted) fails.push(`caller ${c.id} (${c.file}) admitted the content-only run \`rightkit-gate ${partial.args.join(" ")}\` (exit ${partial.status}), which skipped the checks that execute repository code`);
    } else fails.push(`caller ${c.id}: unknown kind ${c.kind}`);
  }
  return fails.length ? { verdict: FAIL, why: fails.join("; ") } : { verdict: PASS, why: `${seen.length} caller(s) of rightkit-gate check take the installed gate's answer as written: ${seen.join(", ")}; every file naming the gate is listed` };
}
