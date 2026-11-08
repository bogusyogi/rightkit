// The seam's one broker control query from the gate side (reset plan section 3): where the broker listens and what identity a query carries, and the
// reclaim judge built on it. Kept apart from broker-handshake.mjs (which the plan deletes) so the seam survives step 5; it imports only kept modules.
import fs from "node:fs";
import path from "node:path";
import { resolveBrokerClient } from "./broker-client.mjs";
import { BUILD_CONTROL_PROTOCOL, RUNTIME_MANIFEST_FILE, parseRuntimeIdentity } from "./broker-handshake.mjs";
import { discoverBrokerSocket } from "./managed-roots.mjs";
import { snapshotStatusAllowsReclaim, snapshotStatusMessage, snapshotStatusSync } from "./gate-seam.mjs";

const isPipe = (endpoint) => /^\\\\\.\\pipe\\/i.test(String(endpoint));

/**
 * Where a control query to the broker goes and what identity it carries: the endpoint RustRight's discovery names (a user-owned socket, or a pipe) and the
 * runtime identity of the manifest beside the installed `client`. `{ ok, endpoint, identity }` or `{ ok: false, problem }`.
 */
export function brokerControlTarget(o, { client = o?.client } = {}) {
  const platform = o?.platform ?? process.platform;
  const env = o?.env ?? process.env;
  const read = o?.readFile ?? ((file) => fs.readFileSync(file, "utf8"));
  if (typeof client !== "string" || !client) return { ok: false, problem: "the RustRight client is not on PATH, so its runtime identity cannot be read" };
  let exe;
  try { exe = resolveBrokerClient(client, { platform, readFile: read }).file; } catch (error) { return { ok: false, problem: `the RustRight client cannot be resolved (${error.message})` }; }
  let real = exe;
  if (platform !== "win32") { try { real = (o?.realpath ?? fs.realpathSync.native)(exe); } catch { real = exe; } }
  const paths = platform === "win32" ? path.win32 : path.posix;
  const manifest = paths.join(paths.dirname(real), RUNTIME_MANIFEST_FILE);
  let text;
  try { text = read(manifest); } catch (error) { return { ok: false, problem: `cannot read the runtime manifest ${manifest} beside the client (${error?.message ?? error})` }; }
  const identity = parseRuntimeIdentity(text);
  if (!identity.ok) return { ok: false, problem: `${manifest}: ${identity.problem}` };
  let endpoint;
  try { endpoint = (o?.discoverEndpoint ?? discoverBrokerSocket)(env, { platform, exists: fs.existsSync }); } catch (error) { return { ok: false, problem: `the broker endpoint cannot be discovered (${error.message})` }; }
  if (!endpoint) return { ok: false, problem: "no broker endpoint is configured (no explicit socket variable and no broker.sock under a managed control root)" };
  if (platform !== "win32" && !isPipe(endpoint)) {
    try {
      const stat = (o?.lstat ?? fs.lstatSync)(endpoint);
      const uid = typeof (o?.uid ?? process.getuid?.()) === "number" ? (o?.uid ?? process.getuid()) : null;
      if (!stat.isSocket()) return { ok: false, problem: `${endpoint} is not a socket, so it is not the broker's endpoint` };
      if (uid !== null && stat.uid !== uid) return { ok: false, problem: `${endpoint} is owned by another user (uid ${stat.uid}), so it is not this user's broker` };
    } catch (error) { return { ok: false, problem: `cannot connect to the broker at ${endpoint} (${error?.code ?? error?.message ?? error})` }; }
  }
  return { ok: true, endpoint, identity: identity.identity };
}

/**
 * The seam's reclaim judge (snapshot.mjs acquireSnapshotLock `judge`): asks the broker SNAPSHOT_STATUS for the dead holder's lock and returns true only on
 * an exact IDLE echo. Everything it needs is bound here, at construction, from values that exist at that moment (`o`, the resolved `client`, the snapshot
 * path): nothing is read from an outer scope at call time (two closure bugs of 2026-11-09 read a const that was out of scope or not yet initialized, and the
 * catch turned them into "no proof"). `query` is injectable for tests; the default crosses the real socket.
 */
export function snapshotReclaimJudge({ o, client, snapshotPath, query = snapshotStatusSync }) {
  if (typeof snapshotPath !== "string" || !path.isAbsolute(snapshotPath)) throw new TypeError("snapshotReclaimJudge: snapshotPath must be absolute");
  const bound = { o, client: typeof client === "string" && client ? client : null, snapshotPath };
  return (holder) => {
    try {
      if (!bound.client) return false;
      const target = brokerControlTarget(bound.o, { client: bound.client });
      if (!target.ok) return false;
      const message = snapshotStatusMessage({ protocol: BUILD_CONTROL_PROTOCOL, runtime: target.identity, snapshot: bound.snapshotPath, owner: holder });
      return snapshotStatusAllowsReclaim(query({ endpoint: target.endpoint, message }), { snapshot: bound.snapshotPath, owner: holder });
    } catch { return false; }
  };
}
