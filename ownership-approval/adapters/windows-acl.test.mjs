import assert from "node:assert/strict";
import test from "node:test";
import { evaluateWindowsAcl } from "../trust-schema.mjs";
import { evaluateWindowsNativeAclForTest, evaluateWindowsCheckpointStateForTest } from "./private-local.mjs";

const ADMIN = "S-1-5-32-544";
const TRUSTED_INSTALLER = "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";
const USERS = "S-1-5-32-545";
const AUTHENTICATED_USERS = "S-1-5-11";
const SERVICE = "S-1-5-21-1-2-3-4001";
const AGENT = "S-1-5-21-1-2-3-4002";
const ace = (identitySID, mask, propagationFlags = 0) => ({ identitySID, type: "Allow", mask, propagationFlags });
const item = (target, access) => ({ path: target, ownerSID: TRUSTED_INSTALLER, ownerName: "NT SERVICE\\TrustedInstaller", attributes: "Directory", directory: true, nonempty: true, elevated: false, access });

test("native Windows read masks & documented ancestor-create masks pass without accepting write authority", () => {
  const readMask = 0x001200a9;
  const ancestorMask = 278;
  for (const [target, sid, mask] of [
    ["C:\\", AUTHENTICATED_USERS, 4],
    ["C:\\ProgramData", USERS, ancestorMask],
    ["C:\\Program Files\\Orthic Labs\\RightKit", USERS, readMask],
  ]) {
    const result = evaluateWindowsAcl(item(target, [ace(sid, mask)]), { target });
    assert.deepEqual(result, { ok: true, problems: [] }, `${target} mask ${mask} is a supported native grant`);
  }
  const isolated = "C:\\Program Files\\Orthic Labs\\RightKit";
  for (const mask of [2, 4, 0x00010000, 0x40000000, 0x10000000, 0x04000000]) {
    const result = evaluateWindowsAcl(item(isolated, [ace(USERS, mask)]), { target: isolated });
    assert.equal(result.ok, false, `untrusted writable/unknown mask ${mask} must be rejected`);
  }
  assert.equal(evaluateWindowsAcl(item(isolated, [ace(USERS, 0x40000000, 2)]), { target: isolated }).ok, true, "InheritOnly ACE does not grant access to current object");
});

test("public native audit requires service SID & denies agent App-key read", { skip: process.platform === "win32" ? false : "Windows path semantics: the native ACL evaluator runs only under PowerShell on win32" }, () => {
  const root = "C:\\Program Files\\Orthic Labs\\RightKit";
  const config = { service: { windowsSid: SERVICE, agentWindowsSids: [AGENT], windowsCheckoutRoot: "D:\\runner", macosCheckoutRoot: "/var/lib/rightkit/checkouts", macosUid: 600, agentMacosUids: [501] } };
  const keyPath = `${root}\\ownership\\public-check-app.pem`;
  const key = { path: keyPath, ownerSID: SERVICE, attributes: "Archive", directory: false, access: [ace(SERVICE, 0x001200a9), ace(ADMIN, 0x001200a9)] };
  const checkout = [
    { path: "D:\\runner\\repo", ownerSID: SERVICE, attributes: "Directory", access: [ace(USERS, 0x001200a9)] },
    { path: "D:\\runner", ownerSID: SERVICE, ownerName: "RightKit public service", attributes: "Directory", directory: true, nonempty: true, elevated: false, ancestor: true, access: [ace(SERVICE, 0x001f01ff), ace(USERS, 0x001200a9)] },
  ];
  const checkpoints = `${root}\\ownership\\checkpoints`;
  const checkpointDir = { ...item(checkpoints, [ace(ADMIN, 0x001f01ff), ace(SERVICE, 0x42)]), ownerSID: ADMIN, directory: true };
  const native = { items: [item(root, [ace(USERS, 0x001200a9)]), checkpointDir], identitySID: SERVICE, groupSIDs: [], elevated: false, public: { config, key, checkout } };
  const problems = [];
  evaluateWindowsNativeAclForTest(native, { problems, isPublic: true, root, repoRoot: "D:\\runner\\repo" });
  assert.deepEqual(problems, []);
  const tampered = structuredClone(native);
  tampered.public.key.access.push(ace(AGENT, 0x001200a9));
  const rejected = [];
  evaluateWindowsNativeAclForTest(tampered, { problems: rejected, isPublic: true, root, repoRoot: "D:\\runner\\repo" });
  assert.match(rejected.join(";"), /key ACL grants access to non-service principal/);
  const wrongIdentity = structuredClone(native);
  wrongIdentity.identitySID = AGENT;
  const wrong = [];
  evaluateWindowsNativeAclForTest(wrongIdentity, { problems: wrong, isPublic: true, root, repoRoot: "D:\\runner\\repo" });
  assert.match(wrong.join(";"), /exact isolated service identity/);
  const writableKey = structuredClone(native);
  writableKey.public.key.access[0].mask |= 2;
  const keyProblems = [];
  evaluateWindowsNativeAclForTest(writableKey, { problems: keyProblems, isPublic: true, root, repoRoot: "D:\\runner\\repo" });
  assert.match(keyProblems.join(";"), /read-only key access/);
  const writableCheckout = structuredClone(native);
  writableCheckout.public.checkout[0].access.push(ace(USERS, 278));
  const checkoutProblems = [];
  evaluateWindowsNativeAclForTest(writableCheckout, { problems: checkoutProblems, isPublic: true, root, repoRoot: "D:\\runner\\repo" });
  assert.match(checkoutProblems.join(";"), /checkout grants write authority/);
});

test("checkpoint state permits service-only writes in fixed subtree and rejects agent or broad temp-file writes", { skip: process.platform === "win32" ? false : "Windows path semantics: the native ACL evaluator runs only under PowerShell on win32" }, () => {
  const root = "C:\\Program Files\\Orthic Labs\\RightKit";
  const checkpoints = `${root}\\ownership\\checkpoints`;
  const stateName = `${"a".repeat(64)}.json`;
  const entries = [
    { ...item(checkpoints, [ace(ADMIN, 0x001f01ff), ace(SERVICE, 0x42)]), ownerSID: ADMIN, directory: true },
    { ...item(`${checkpoints}\\${stateName}`, [ace(ADMIN, 0x001f01ff), ace(SERVICE, 2)]), ownerSID: SERVICE, directory: false, attributes: "Archive" },
  ];
  assert.deepEqual(evaluateWindowsCheckpointStateForTest(entries, root, SERVICE, [AGENT], []), []);
  const agentWrite = structuredClone(entries);
  agentWrite[0].access.push(ace(AGENT, 2));
  assert.match(evaluateWindowsCheckpointStateForTest(agentWrite, root, SERVICE, [AGENT], []).join(";"), /agent principal/);
  const broadTempWrite = [...entries, { ...item(`${checkpoints}\\.${"a".repeat(64)}.42.99.tmp`, [ace(ADMIN, 0x001f01ff), ace(USERS, 0x40000000)]), ownerSID: SERVICE, directory: false, attributes: "Archive" }];
  assert.match(evaluateWindowsCheckpointStateForTest(broadTempWrite, root, SERVICE, [AGENT], []).join(";"), /non-service principal/);
});
