import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { parseLaunch } from "./launch_vast.mjs";

const sha = (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`;
const writeJson = (path, value) => writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`);
const signReceipt = (payload) => ({ ...payload, signature: { algorithm: "sha256", payload_sha256: sha(Buffer.from(JSON.stringify(payload))) } });

function createFullLaunchFixture(root) {
  const packet = join(root, "packet"), data = join(packet, "data"), scripts = join(packet, "scripts", "local-model-training");
  mkdirSync(data, { recursive: true });
  mkdirSync(scripts, { recursive: true });
  for (const name of ["smoke-2.jsonl", "development-100.jsonl", "training-1500.jsonl", "gemma-preflight-30.jsonl"]) {
    writeFileSync(join(data, name), "{}\n");
  }
  const recipes = join(scripts, "model-recipes.json");
  writeJson(recipes, { lanes: ["falcon", "granite", "gemma", "sotto"] });
  const control = join(packet, "control.json");
  writeJson(control, { runtime_files: {} });
  const manifest = join(packet, "bundle-manifest.json");
  writeJson(manifest, { control_sha256: sha(readFileSync(control)), files: [] });

  const payload = {
    schema_version: "heardright.l1.smoke_gate_receipt.v2",
    status: "infrastructure_pass",
    source_smoke_packet: {
      kind: "current_packet_smoke",
      manifest_sha256: sha(readFileSync(manifest)),
      control_sha256: sha(readFileSync(control)),
      smoke_dataset_sha256: sha(readFileSync(join(data, "smoke-2.jsonl"))),
      recipes_sha256: sha(readFileSync(recipes)),
      archive_sha256: null,
      archive_manifest_sha256: null,
    },
    target_full_packet: { manifest_sha256: sha(readFileSync(manifest)), control_sha256: sha(readFileSync(control)) },
    smoke: { accepted_run_status: "completed_no_viable_candidates" },
    lane_order: ["falcon", "granite", "gemma", "sotto"],
    lanes: ["falcon", "granite", "gemma", "sotto"].map((lane) => ({ lane, disposition: "rejected_no_safe_checkpoint" })),
  };
  const receipt = join(root, "smoke-gate-receipt.json");
  writeJson(receipt, signReceipt(payload));
  const authorization = join(root, "authorization.json");
  const approval = {
    approved: true,
    scope: "heardright_l1_four_family_training",
    profile: "full",
    offer_id: "offer-test",
    hourly_rate_usd: 0.25,
    max_usd: 20,
    max_hours: 24,
    smoke_gate_receipt: receipt,
    smoke_gate_receipt_sha256: sha(readFileSync(receipt)),
  };
  writeJson(authorization, approval);
  return { packet, receipt, authorization, approval };
}

test("launcher rejects an unapproved authorization", () => {
  const root = mkdtempSync(join(tmpdir(), "heardright-vast-launch-"));
  try {
    const authorization = join(root, "authorization.json");
    writeFileSync(authorization, JSON.stringify({ approved: false, scope: "heardright_l1_four_family_training", profile: "smoke" }));
    assert.throws(() => parseLaunch(["--profile", "smoke", "--authorization", authorization, "--output-dir", join(root, "out")]), /authorization must approve/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("shell launch prepends the verified bundled Node binary to PATH", () => {
  const shell = readFileSync("scripts/local-model-training/run_vast.sh", "utf8");
  assert.match(shell, /export PATH="\$\{RUNTIME_DIR\}\/bin:\$\{PATH\}"/u);
});

test("launcher binds a profile-specific sequential orchestrator invocation", () => {
  const root = mkdtempSync(join(tmpdir(), "heardright-vast-launch-"));
  try {
    const { packet, authorization } = createFullLaunchFixture(root);
    const parsed = parseLaunch(["--profile", "full", "--authorization", authorization, "--output-dir", join(root, "out")], { packetDir: packet });
    assert.equal(parsed.orchestratorArgs.includes("--live"), true);
    assert.equal(parsed.orchestratorArgs.includes("--gemma-preflight-input"), true);
    assert.equal(parsed.orchestratorArgs.includes("--profile"), true);
    assert.equal(parsed.orchestratorArgs.includes("--evaluation-policy"), true);
    assert.equal(parsed.orchestratorArgs.includes("--evaluation-policy-control"), true);
    assert.equal(parsed.orchestratorArgs[parsed.orchestratorArgs.indexOf("--python") + 1], "/workspace/heardright-runtime-cpython311/bin/python3");
    assert.equal(parsed.runtimeDir, "/workspace/heardright-v15-shared-runtime");
    assert.equal(parsed.smokeGate.status, "infrastructure_pass");
    const optIn = parseLaunch(["--profile", "full", "--authorization", authorization, "--output-dir", join(root, "out"), "--include-opt-in-candidates"], { packetDir: packet });
    assert.equal(optIn.orchestratorArgs.includes("--include-opt-in-candidates"), true);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("full launcher rejects missing, tampered, and wrongly hash-bound smoke receipts", () => {
  const root = mkdtempSync(join(tmpdir(), "heardright-vast-launch-"));
  try {
    const { packet, receipt, authorization, approval } = createFullLaunchFixture(root);
    const argv = ["--profile", "full", "--authorization", authorization, "--output-dir", join(root, "out")];

    writeJson(authorization, { ...approval, smoke_gate_receipt: undefined, smoke_gate_receipt_sha256: undefined });
    assert.throws(() => parseLaunch(argv, { packetDir: packet }), /full authorization requires smoke_gate_receipt/);

    writeJson(authorization, approval);
    const signedReceipt = JSON.parse(readFileSync(receipt, "utf8"));
    writeJson(receipt, { ...signedReceipt, status: "tampered" });
    assert.throws(() => parseLaunch(argv, { packetDir: packet }), /smoke gate receipt signature or schema is invalid/);

    const { signature: _signature, ...payload } = signedReceipt;
    writeJson(receipt, signReceipt({ ...payload, target_full_packet: { ...payload.target_full_packet, manifest_sha256: `sha256:${"1".repeat(64)}` } }));
    assert.throws(() => parseLaunch(argv, { packetDir: packet }), /different target packet(?: or lane roster)?/);

    writeJson(receipt, signReceipt({ ...payload, source_smoke_packet: { ...payload.source_smoke_packet, smoke_dataset_sha256: `sha256:${"2".repeat(64)}` } }));
    assert.throws(() => parseLaunch(argv, { packetDir: packet }), /source evidence hashes do not match an accepted source packet/);

    writeJson(receipt, signedReceipt);
    writeJson(authorization, { ...approval, smoke_gate_receipt_sha256: `sha256:${"0".repeat(64)}` });
    assert.throws(() => parseLaunch(argv, { packetDir: packet }), /full authorization smoke gate receipt hash mismatch/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
