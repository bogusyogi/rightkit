// The twelve-line result record (rightkit.acceptance-result/1) an admission reads (docs/GATE-CONTRACT.md GC-A27, rightkit#1514 step 9). Built from three things this module
// does not decide: the launcher's record of the run (launch.json), the judge's verdict (acceptance-lines.mjs --json) and the broker's own receipts that appeared during the run
// (each holds the small signed `publication` claim). It writes no verdict of its own and it changes neither the judge nor the launcher.
//
//   node acceptance-result.mjs --run <run dir> --repo <dir> --receipts <broker receipts dir> --out <result dir> [any other acceptance-lines.mjs option]
//
// <result dir> then holds acceptance-result.json, job-claims/<id>.json (one { id, publication } per broker job of the run) and, when the run's gate is a sealed install,
// gate-seal/manifest.txt and gate-seal/trusted-gate-source.json: the folder layout `gate-release-admission.mjs` reads.
import fs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { ACCEPTANCE_RESULT_SCHEMA, judgeFileHashes } from "./acceptance-admission.mjs";
import { CONTRACT_FILE } from "./acceptance-lines.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
const HOST_OF_PLATFORM = Object.freeze({ win32: "dell", darwin: "mac" });
const SEALED_FOLDER = /[\\/]trusted-gates[\\/](?:candidates[\\/])?([0-9a-f]{64})[\\/]/i;

/** The sealed gate a gate path is inside: the 64-hex folder an installed candidate or trusted gate is named after (its manifest's sha256), else null. */
export const sealedOfGatePath = (gatePath) => SEALED_FOLDER.exec(String(gatePath ?? ""))?.[1]?.toLowerCase() ?? null;

/** The claim a broker receipt hands over: { id, publication } with its bytes and sha256; null when the receipt has no publication claim or no job id. */
export function claimOf(receiptDoc) {
  const id = receiptDoc?.provenance?.payload?.jobId; const publication = receiptDoc?.publication;
  if (typeof id !== "string" || !/^[A-Za-z0-9._-]{1,128}$/.test(id) || !publication || typeof publication !== "object") return null;
  const bytes = Buffer.from(JSON.stringify({ id, publication }));
  return { id, bytes, sha256: sha256(bytes) };
}

/**
 * `launch`: launch.json. `verdict`: the judge's JSON ({ lines: [{ id, text, verdict }], pass }). `contractText`, `judgeFiles`: of this checkout. `tree`: the selected commit's tree.
 * `claims`: claimOf of each broker receipt of the run. Returns the record.
 */
export function buildAcceptanceResult({ launch, verdict, contractText, judgeFiles, tree, claims }) {
  return {
    schema: ACCEPTANCE_RESULT_SCHEMA, runId: launch?.runId ?? null, commit: launch?.commit ?? null, tree: tree ?? null, host: HOST_OF_PLATFORM[launch?.fingerprint?.platform] ?? null, kind: launch?.kind ?? null,
    startedAt: launch?.startedAt ?? null, endedAt: launch?.endedAt ?? null, gate: { path: launch?.gate?.path ?? null, sealedSha256: sealedOfGatePath(launch?.gate?.path) },
    contractSha256: sha256(contractText), judgeFiles: { ...judgeFiles }, lines: (verdict?.lines ?? []).map((l) => ({ id: l.id, text: l.text, verdict: l.verdict })), pass: verdict?.pass === true,
    jobs: claims.filter(Boolean).map((c) => ({ requestId: c.id, claimSha256: c.sha256 })), jobClaims: "job-claims", gateSeal: "gate-seal",
  };
}

/** `node acceptance-result.mjs ...` (see the head of this file). Exit 0 when the result is written, whether or not the run passed: the verdict is in the record. */
export function main(argv = process.argv.slice(2), { out = console.log } = {}) {
  const opt = (name) => { const i = argv.indexOf(name); return i < 0 ? null : argv[i + 1]; };
  const runDir = opt("--run"); const repo = opt("--repo"); const receiptsDir = opt("--receipts"); const outDir = opt("--out");
  if (!runDir || !repo || !receiptsDir || !outDir) { out("usage: acceptance-result.mjs --run <run dir> --repo <dir> --receipts <broker receipts dir> --out <result dir> [acceptance-lines.mjs options]"); return 2; }
  const launch = JSON.parse(fs.readFileSync(path.join(runDir, "launch.json"), "utf8"));
  // The judge, as a child process with the caller's options: its exit status is 1 for a run that did not pass, which is not a failure here.
  const passThrough = argv.filter((a, i) => !["--out"].includes(a) && !["--out"].includes(argv[i - 1]));
  let judged; try { judged = execFileSync(process.execPath, [path.join(here, "acceptance-lines.mjs"), ...passThrough, "--json"], { encoding: "utf8", maxBuffer: 1 << 28, windowsHide: true }); } catch (e) { judged = e.stdout; }
  let verdict; try { verdict = JSON.parse(judged); } catch { out(`acceptance-result REFUSED: the judge gave no verdict (${String(judged).slice(0, 200)})`); return 1; }
  let before = new Set(); try { before = new Set(fs.readFileSync(path.join(runDir, "before", "receipts.txt"), "utf8").split(/\r?\n/).filter(Boolean)); } catch { /* none */ }
  const claims = [];
  for (const name of fs.readdirSync(receiptsDir)) {
    if (!name.endsWith(".json") || before.has(name)) continue;
    try { const c = claimOf(JSON.parse(fs.readFileSync(path.join(receiptsDir, name), "utf8"))); if (c) claims.push(c); } catch { /* not a receipt */ }
  }
  const tree = String(execFileSync("git", ["-C", repo, "rev-parse", `${launch.commit}^{tree}`], { encoding: "utf8", windowsHide: true })).trim();
  const result = buildAcceptanceResult({ launch, verdict, contractText: String(fs.readFileSync(CONTRACT_FILE)), judgeFiles: judgeFileHashes(), tree, claims });
  fs.mkdirSync(path.join(outDir, "job-claims"), { recursive: true });
  for (const c of claims) fs.writeFileSync(path.join(outDir, "job-claims", `${c.id}.json`), c.bytes);
  const gatePath = launch.gate?.path; const sealed = result.gate.sealedSha256;
  if (gatePath && sealed) {
    const m = SEALED_FOLDER.exec(String(gatePath)); const folder = String(gatePath).slice(0, m.index + m[0].length - 1); const beside = `${folder}.manifest.txt`;
    fs.mkdirSync(path.join(outDir, "gate-seal"), { recursive: true });
    for (const [from, to] of [[beside, "manifest.txt"], [path.join(folder, "trusted-gate-source.json"), "trusted-gate-source.json"]]) { try { fs.copyFileSync(from, path.join(outDir, "gate-seal", to)); } catch { /* admission refuses without them */ } }
  }
  fs.writeFileSync(path.join(outDir, "acceptance-result.json"), `${JSON.stringify(result, null, 2)}\n`);
  out(`acceptance-result written: ${path.join(outDir, "acceptance-result.json")} (${result.lines.filter((l) => l.verdict === "pass").length} of ${result.lines.length} lines pass, ${claims.length} broker claim(s), sealed gate ${sealed ?? "none"})`);
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exit(main());
