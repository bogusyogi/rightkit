import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { appendFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { notarizeMacosArtifact, stapleMacosArtifact, validateNotarizationReceipt } from "./macos-notarization.mjs";

function fixture(t, extension = ".dmg") {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-notary-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const archive = path.join(root, `MailRight_0.2.131_universal${extension}`);
  writeFileSync(archive, "signed artifact");
  return { archive, receiptPath: path.join(root, "state", "macos-notarization.json"), authArgs: [] };
}
function mockXcrun({ accepted = true, alreadyStapled = false, failStaple = false, failValidation = false, mutate = false } = {}) {
  const calls = [];
  let stapled = alreadyStapled;
  const run = (cmd, args) => {
    if (cmd === "/bin/launchctl") return { status: 0, stdout: "Aqua" };
    if (cmd === "/usr/bin/security") return { status: 0 };
    calls.push([cmd, ...args]);
    if (args[0] === "notarytool") return { status: 0, stdout: JSON.stringify({ status: accepted ? "Accepted" : "Invalid", id: "submission" }) };
    if (args[1] === "staple") {
      if (failStaple) return { status: 1, stderr: "no ticket" };
      stapled = true;
      if (mutate && !args[2].endsWith(".app")) appendFileSync(args[2], "ticket");
      return { status: 0, stdout: "stapled" };
    }
    return { status: stapled && !failValidation ? 0 : 65, stderr: "missing or invalid ticket" };
  };
  return { calls, run };
}

test("Accepted DMG is stapled & validated before receipt hashes final bytes", (t) => {
  const files = fixture(t);
  const mock = mockXcrun({ mutate: true });
  const receipt = notarizeMacosArtifact({ ...files, run: mock.run });
  assert.deepEqual(mock.calls.map((call) => call.slice(1, 3)), [["notarytool", "submit"], ["stapler", "validate"], ["stapler", "staple"], ["stapler", "validate"]]);
  assert.equal(receipt.archive.sha256, createHash("sha256").update(readFileSync(files.archive)).digest("hex"));
  assert.equal(receipt.stapling[0].validated, true);
  assert.doesNotThrow(() => validateNotarizationReceipt({ ...files, run: mock.run }));
  assert.equal(mock.calls.at(-1)[2], "validate");
});

test("already stapled artifact skips staple & asserts validation", (t) => {
  const files = fixture(t);
  const mock = mockXcrun({ alreadyStapled: true });
  const result = stapleMacosArtifact(files.archive, { run: mock.run });
  assert.equal(result.alreadyStapled, true);
  assert.deepEqual(mock.calls.map((call) => call[2]), ["validate", "validate"]);
});

for (const options of [{ accepted: false }, { failStaple: true }, { failValidation: true }]) {
  test(`notarization failure ${JSON.stringify(options)} never writes sealable receipt`, (t) => {
    const files = fixture(t);
    const mock = mockXcrun(options);
    assert.throws(() => notarizeMacosArtifact({ ...files, run: mock.run }), /not accepted|staple.*failed|validate.*failed/);
    assert.equal(existsSync(files.receiptPath), false);
    if (options.accepted === false) assert.equal(mock.calls.length, 1);
  });
}

test("separately notarized app ZIP staples app bundle, never ZIP", (t) => {
  const files = fixture(t, ".zip");
  const app = path.join(path.dirname(files.archive), "MailRight.app");
  mkdirSync(app);
  const mock = mockXcrun();
  const receipt = notarizeMacosArtifact({ ...files, app, run: mock.run });
  assert.deepEqual(receipt.stapling.map((item) => item.file), [app]);
  assert.ok(mock.calls.filter((call) => call[1] === "stapler").every((call) => call[3] === app));
  assert.doesNotThrow(() => validateNotarizationReceipt({ ...files, run: mock.run }));
});

test("ZIP without app path fails before submission", (t) => {
  const files = fixture(t, ".zip");
  const mock = mockXcrun();
  assert.throws(() => notarizeMacosArtifact({ ...files, run: mock.run }), /require notarize.app/);
  assert.equal(mock.calls.length, 0);
});

test("pre-seal validation rejects missing staple, altered archive & invalid final ticket", (t) => {
  const files = fixture(t);
  const mock = mockXcrun();
  const receipt = notarizeMacosArtifact({ ...files, run: mock.run });
  assert.throws(() => validateNotarizationReceipt({ ...files, run: mockXcrun().run }), /validate failed/);
  writeFileSync(files.receiptPath, JSON.stringify({ ...receipt, stapling: [] }));
  assert.throws(() => validateNotarizationReceipt({ ...files, run: mock.run }), /missing macOS stapling evidence/);
  writeFileSync(files.receiptPath, JSON.stringify(receipt));
  appendFileSync(files.archive, "different");
  assert.throws(() => validateNotarizationReceipt({ ...files, run: mock.run }), /invalid macOS notarization receipt/);
});
