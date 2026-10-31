// `rightkit-gate candidate-receipt --commit <full sha> --broker <file> --client <file> [--out <dir>]` (GB-73): writes the candidate build receipt (candidate-build-receipt.mjs) for the
// binaries a gate SNAPSHOT build of that commit produced, records its digest apart from it, and prints one JSON line with the record and the installer's two arguments
// (`--candidate-receipt <file> --candidate-receipt-sha256 <digest>`). The commit's tree is read from the repository the gate runs in; the artifact digests are computed here, never taken from a flag.
import path from "node:path";
import { resolveManagedRoots } from "./managed-roots.mjs";
import { artifactDigest, buildCandidateReceipt, candidateReceiptArgs, writeCandidateReceipt } from "./candidate-build-receipt.mjs";

export const CANDIDATE_RECEIPT_USAGE = "usage: rightkit-gate candidate-receipt --commit <full lowercase sha> --broker <broker binary> --client <client binary> [--out <dir>]   (the binaries a gate snapshot build of that commit produced)";

export async function candidateReceiptCommand(o, args, { artifact = artifactDigest, write = writeCandidateReceipt } = {}) {
  const take = (name) => { const i = args.indexOf(name); return i < 0 ? undefined : args[i + 1]; };
  const known = new Set(["--commit", "--broker", "--client", "--out"]);
  const stray = args.filter((a, i) => (a.startsWith("--") ? !known.has(a) : !known.has(args[i - 1] ?? "")));
  const commit = take("--commit"); const broker = take("--broker"); const client = take("--client");
  if (stray.length || !commit || !broker || !client) { o.err(`rightkit-gate: ${CANDIDATE_RECEIPT_USAGE}`); return 2; }
  const fail = (e) => { o.err(`rightkit-gate: ${e.code ?? "RK_CANDIDATE_RECEIPT_INVALID"}: ${e.message}`); return 1; };
  let record;
  try {
    const resolved = o.spawn("git", ["rev-parse", "--verify", "--quiet", `${commit}^{tree}`], { cwd: o.cwd, encoding: "utf8", env: o.env });
    const tree = String(resolved?.stdout ?? "").trim();
    if (resolved?.status !== 0 || !/^[0-9a-f]{40}$/.test(tree)) throw Object.assign(new Error(`the commit ${commit} is not in this repository, so its tree cannot be read`), { code: "RK_CANDIDATE_RECEIPT_INVALID" });
    const receipt = buildCandidateReceipt({ commit, tree, brokerSha256: artifact(broker), clientSha256: artifact(client) });
    const out = take("--out") ?? path.join(o.env.RIGHTKIT_SCRATCH_ROOT || path.join(resolveManagedRoots({ env: o.env, platform: o.platform }).dataRoot, "runs"), "candidate-receipts", commit);
    if (!path.isAbsolute(out)) throw Object.assign(new Error(`--out ${out} must be an absolute directory`), { code: "RK_CANDIDATE_RECEIPT_INVALID" });
    record = write({ dir: out, receipt });
  } catch (e) { return fail(e); }
  o.out(JSON.stringify({ ...record, installerArgs: candidateReceiptArgs(record) }));
  return 0;
}
