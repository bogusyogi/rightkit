// The shared candidate claim at the authenticated dispatch boundary (GB-47; GATE 51/E9). Hosts keep local pipeline state, but nothing local stops the
// Mac and the Dell from both dispatching one candidate with different broker request ids. The one place both go through is GitHub, so the claim lives
// there, atomically: an annotated tag object (carrying the claim's identity and state) behind the ref
//   refs/rightkit-claims/<registry>/<unit hash>/<version>/<identity digest>
// whose creation fails if it exists. The identity binds repository, workflow, source commit, tree, registry, unit, version, the qualified artifact
// digest, the allow-set policy and the route. Reading `matching-refs` under <version>/ answers every question before a POST:
//   - another digest for the same version: CONFLICT (different bytes or policy for one version): refuse, never dispatch;
//   - the same digest with a recorded run: JOIN that run (verified through the Actions API), send nothing;
//   - the same digest with only an intent: discover the run by the intent's own request token (a lost acknowledgement) and join it; without one the
//     intent's owner may still be sending, so another request waits (refuses) and only the SAME request id, which is a retry of that very send, may send;
//   - no claim: reserve it (intent), then send, then record the run on the claim. Version/hash conflicts at the registry stay the uploader's.
// The functions take `gh(args, env) -> { ok, stdout }` (the dispatcher's own bounded gh); they add no authority and no new tool.
import crypto from "node:crypto";

export const CLAIM_SCHEMA = "rightkit.dispatch-claim/1";
const sha256 = (text) => crypto.createHash("sha256").update(text).digest("hex");
const canonical = (value) => JSON.stringify(sortKeys(value));
function sortKeys(v) { return Array.isArray(v) ? v.map(sortKeys) : v && typeof v === "object" ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, sortKeys(v[k])])) : v; }

/** What a candidate IS: any difference is a different candidate (and, for one version, a conflict). */
export function claimIdentity({ repo, workflow, sha, registry, unit, version, qualification, allow = {} }) {
  return { schema: CLAIM_SCHEMA, repo, workflow, sha, registry, unit, version, tree: qualification?.tree ?? null, artifactSha256: qualification?.artifactSha256 ?? null, policy: sha256(canonical(allow)), route: "broker" };
}
export const claimDigest = (identity) => sha256(canonical(identity));
/** The ref namespace of one version of one unit (the unit is hashed: a scoped npm name is not a safe ref segment). */
export function claimPrefix({ registry, unit, version }) {
  return `rightkit-claims/${registry}/${sha256(String(unit)).slice(0, 16)}/${String(version).replace(/[^0-9A-Za-z._+-]/g, "_")}`;
}

const json = (r) => { try { return JSON.parse(r.stdout); } catch { return null; } };

/** The claims recorded for this version: [{ ref, sha, claim }] (a tag object whose message is the claim JSON). */
export function listClaims({ gh, env, repo, prefix }) {
  const listed = gh(["api", `repos/${repo}/git/matching-refs/${prefix}/`], env);
  if (!listed.ok) return { ok: false, claims: [] };
  const refs = json(listed);
  if (!Array.isArray(refs)) return { ok: false, claims: [] };
  const claims = [];
  for (const ref of refs) {
    const tag = gh(["api", `repos/${repo}/git/tags/${ref.object?.sha}`], env);
    const body = tag.ok ? json(tag) : null;
    let claim = null;
    try { claim = JSON.parse(body?.message ?? ""); } catch { claim = null; }
    // An unreadable claim is still a claim: it can only be read as conflicting, never as absent (fail closed).
    claims.push({ ref: ref.ref, sha: ref.object?.sha, claim });
  }
  return { ok: true, claims };
}

function writeClaimObject({ gh, env, repo, sha, name, claim, now }) {
  const made = gh(["api", "-X", "POST", `repos/${repo}/git/tags`, "-f", `tag=${name}`, "-f", `message=${JSON.stringify(claim)}`, "-f", `object=${sha}`, "-f", "type=commit",
    "-f", "tagger[name]=rightkit-dispatch", "-f", "tagger[email]=noreply@rightkit.invalid", "-f", `tagger[date]=${new Date(now()).toISOString()}`], env);
  return made.ok ? json(made)?.sha ?? null : null;
}

/**
 * Decides before any send. Returns:
 *   { status: "reserved", ref, record(run) }   this request owns the claim and may send; `record(run)` writes the run onto it;
 *   { status: "joined", run }                  an existing run of this very candidate: send nothing;
 *   { status: "conflict" | "pending" | "unavailable", message }   refuse.
 * `discover(token)` returns the runs whose title carries `token` (the dispatcher's own matcher).
 */
export function reserveClaim({ gh, env, repo, identity, requestId, discover, verifyRun, now = Date.now }) {
  const digest = claimDigest(identity);
  const prefix = claimPrefix(identity);
  const ref = `refs/${prefix}/${digest}`;
  for (let attempt = 0; attempt < 3; attempt += 1) {
    const existing = listClaims({ gh, env, repo, prefix });
    if (!existing.ok) return { status: "unavailable", message: "the shared claim namespace cannot be read, so a candidate claim cannot be proved; nothing was sent" };
    const others = existing.claims.filter((c) => c.ref !== ref);
    if (others.length) return { status: "conflict", message: `${identity.unit}@${identity.version} is already claimed for different bytes, source or policy (${others.map((c) => c.ref.split("/").pop().slice(0, 12)).join(", ")}); this candidate (${digest.slice(0, 12)}) is refused` };
    const mine = existing.claims.find((c) => c.ref === ref);
    if (mine) {
      const claim = mine.claim;
      if (!claim || claim.identityDigest !== digest) return { status: "conflict", message: `the claim ${digest.slice(0, 12)} for ${identity.unit}@${identity.version} cannot be read as this candidate's; refused` };
      if (claim.run?.id) {
        const run = verifyRun(claim.run.id);
        if (!run) return { status: "pending", message: `the claim for ${identity.unit}@${identity.version} names run ${claim.run.id}, which the Actions API does not confirm; reconcile it before any send` };
        return { status: "joined", run, claim };
      }
      // An intent with no run: a lost acknowledgement, or a send still in flight. Find the run by the intent's own token before anything else.
      const found = discover(claim.requestId);
      if (found.length === 1) return { status: "joined", run: found[0], claim, adopt: true, ref, claimSha: mine.sha };
      if (found.length > 1) return { status: "pending", message: `${found.length} runs carry the claim's request token ${claim.requestId}; ambiguous, nothing is sent` };
      if (claim.requestId === requestId) return { status: "reserved", ref, retry: true, record: (run) => recordRun({ gh, env, repo, ref, identity, digest, requestId, run, now }) };
      return { status: "pending", message: `${identity.unit}@${identity.version} is claimed by request ${claim.requestId} (intent recorded ${claim.at}); no run of it is visible yet, so it may still be sending: reconcile that request before sending another` };
    }
    // No claim: reserve it. Creating the ref fails if another origin got there first; then the next pass reads theirs.
    const claim = { schema: CLAIM_SCHEMA, identity, identityDigest: digest, requestId, state: "intent", at: new Date(now()).toISOString() };
    const tagSha = writeClaimObject({ gh, env, repo, sha: identity.sha, name: `claim-${digest.slice(0, 16)}`, claim, now });
    if (!tagSha) return { status: "unavailable", message: "the candidate claim object could not be written; nothing was sent" };
    const created = gh(["api", "-X", "POST", `repos/${repo}/git/refs`, "-f", `ref=${ref}`, "-f", `sha=${tagSha}`], env);
    if (created.ok) return { status: "reserved", ref, record: (run) => recordRun({ gh, env, repo, ref, identity, digest, requestId, run, now }) };
  }
  return { status: "unavailable", message: "the candidate claim could not be created or read after three attempts; nothing was sent" };
}

/** Writes the run onto this request's claim (a new tag object, the ref moved by its owner). Returns true when recorded. */
export function recordRun({ gh, env, repo, ref, identity, digest, requestId, run, now = Date.now }) {
  const claim = { schema: CLAIM_SCHEMA, identity, identityDigest: digest, requestId, state: "dispatched", run: { id: run.id, attempt: run.run_attempt ?? null, url: run.html_url ?? null }, at: new Date(now()).toISOString() };
  const sha = writeClaimObject({ gh, env, repo, sha: identity.sha, name: `claim-${digest.slice(0, 16)}-run-${run.id}`, claim, now });
  if (!sha) return false;
  return gh(["api", "-X", "PATCH", `repos/${repo}/git/${ref}`, "-f", `sha=${sha}`, "-F", "force=true"], env).ok;
}
