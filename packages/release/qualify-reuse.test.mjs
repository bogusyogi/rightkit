// GB-14 (GATE 37/E11): the gate's qualify record journals the protected installed-QA receipt by digest and reuses it only while the existing protected
// verifier still accepts that receipt over the current state; the record never stands alone on a manifest digest.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { QaAdmissionError } from "./release-qa-receipt.mjs";
import { installedQaReceiptDigest, qualifyReuseProblem } from "./qualify-reuse.mjs";
import { doorFixture, tmpDir } from "./test-support/door-fixture.mjs";

const POSIX_SIM = process.platform === "win32" ? "simulates a Linux host, whose ownership reads ps" : false;
const DIGEST = "c".repeat(64);
const MANIFEST = "d".repeat(64);
const sealedNow = { id: "app-1.0.0-00000000", manifest: "/m", digest: MANIFEST };

test("[GB-14] a record is reusable only with a receipt digest AND a passing protected verification of exactly that receipt", () => {
  const recorded = { ok: true, manifestSha256: MANIFEST, qaReceiptDigest: DIGEST };
  const verify = (admit) => () => { if (admit instanceof Error) throw admit; return { digest: admit }; };
  const problem = (over = {}, v = verify(DIGEST)) => qualifyReuseProblem({ recorded: { ...recorded, ...over }, sealedNow, repoRoot: "/r", platform: "win", verify: v, readFile: () => "{}" });
  assert.equal(problem(), null, "a valid identical protected receipt reuses");
  assert.match(problem({ qaReceiptDigest: undefined }), /carries no protected QA receipt digest/);
  assert.match(problem({ manifestSha256: "e".repeat(64) }), /sealed bytes differ/);
  assert.match(problem({ ok: false }), /no passed qualify result/);
  assert.match(problem({}, verify(new QaAdmissionError("protected policy bundle changed since the receipt was written"))), /no longer verifies: .*policy bundle changed/, "a policy change invalidates at once");
  assert.match(problem({}, verify(new QaAdmissionError("QA floor changed since the receipt was written"))), /floor changed/);
  assert.match(problem({}, verify(new QaAdmissionError("required check app.launch is not recorded in the receipt"))), /check coverage|required check/);
  assert.match(problem({}, verify("f".repeat(64))), /not the one the qualify record names/, "another receipt on disk is not the journaled one");
  assert.equal(qualifyReuseProblem({ recorded, sealedNow, repoRoot: "/r", platform: "win", verify: null }), null, "the publisher, which holds no receipt key, is held to the digests");
  assert.match(qualifyReuseProblem({ recorded: { ...recorded, qaReceiptDigest: undefined }, sealedNow, repoRoot: "/r", platform: "win", verify: null }), /no protected QA receipt digest/);
});

test("[GB-14] the default verifier is the EXISTING protected admission: with no receipt on disk the record does not stand", (t) => {
  const repoRoot = tmpDir(t, "rk-gb14-");
  const manifestFile = path.join(repoRoot, "release-manifest.json");
  fs.writeFileSync(manifestFile, JSON.stringify({ releaseId: sealedNow.id, files: [], commit: "x" }));
  const problem = qualifyReuseProblem({ recorded: { ok: true, manifestSha256: MANIFEST, qaReceiptDigest: DIGEST }, sealedNow: { ...sealedNow, manifest: manifestFile }, repoRoot, platform: "win" });
  assert.match(problem, /protected installed-QA receipt no longer verifies: no installed QA receipt for app-1.0.0-00000000/);
  assert.equal(installedQaReceiptDigest({ repoRoot, releaseId: sealedNow.id, platform: "win" }), null);
  assert.equal(installedQaReceiptDigest({ repoRoot, releaseId: "r", platform: "win", readReceipt: () => ({ digest: DIGEST }) }), DIGEST);
});

test("[GB-14] door: qualify journals the receipt digest; an unchanged repeat reuses; a protected-policy/floor/coverage change re-runs qualify; a record without a digest re-runs it", { skip: POSIX_SIM }, async (t) => {
  const fx = doorFixture(t);
  const launches = [];
  const sealed = { id: "app-1.0.0-00000000", manifest: "m", digest: MANIFEST };
  let admit = DIGEST;
  const seams = () => ({
    sealedFor: () => sealed, installedQaReceiptDigest: () => DIGEST, spawnInherit: (c, a) => { launches.push(a[1]); return { status: 0 }; },
    verifyQa: () => { if (admit instanceof Error) throw admit; return { digest: admit }; },
  });
  const first = await fx.door(["--to", "qualify", "--platform", "win"], seams());
  assert.equal(first.code, 0, first.all);
  assert.equal(fx.receipts.get(fx.commit()).stages.qualify.qaReceiptDigest, DIGEST, "the protected receipt's digest is in the stage journal");
  assert.deepEqual(launches, ["qa", "build", "qa"]);

  launches.length = 0;
  const again = await fx.door(["--to", "qualify", "--platform", "win"], seams());
  assert.equal(again.code, 0, again.all);
  assert.deepEqual(launches.filter((v) => v === "qa"), ["qa"], "--to qualify itself always runs installed QA");

  // --to qualify always runs QA, so reuse is observed through a later stage target on the host: `stage`-like reuse is `to !== qualify` with sealed intact.
  const reuse = await fx.door(["--to", "release", "--platform", "win", "--tier", "patch"], { ...seams(), env: { ...fx.env, GITHUB_ACTIONS: "true", RIGHTKIT_PUBLISHER: "github-environment" } });
  assert.match(reuse.all, /\[qualify\] already passed for the sealed set .*protected QA receipt cccccccccccc verified/);
  launches.length = 0;
  admit = new QaAdmissionError("protected policy bundle changed since the receipt was written");
  const changed = await fx.door(["--to", "release", "--platform", "win", "--tier", "patch"], { ...seams(), env: { ...fx.env, GITHUB_ACTIONS: "true", RIGHTKIT_PUBLISHER: "github-environment" } });
  assert.match(changed.all, /\[qualify\] the recorded result no longer stands: .*policy bundle changed/);
  assert.notEqual(changed.code, 0, "the publisher never requalifies: it refuses rather than trust the stale record");
  assert.deepEqual(launches, [], "nothing was uploaded");
  admit = DIGEST;
  delete fx.receipts.get(fx.commit()).stages.qualify.qaReceiptDigest;
  const nodigest = await fx.door(["--to", "release", "--platform", "win", "--tier", "patch"], { ...seams(), env: { ...fx.env, GITHUB_ACTIONS: "true", RIGHTKIT_PUBLISHER: "github-environment" } });
  assert.match(nodigest.all, /carries no protected QA receipt digest/);
  assert.notEqual(nodigest.code, 0);
});
