import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test, { after } from "node:test";
import { listNsisPayload, parseSevenZipListing, resolveSevenZip, verifyNsisEmbeddedBinary } from "./nsis-payload.mjs";

const workspace = mkdtempSync(path.join(tmpdir(), "rightkit-nsis-"));
after(() => rmSync(workspace, { recursive: true, force: true }));

// 7-Zip's technical listing: an archive record, a rule, then one record per member.
// The blank `Attributes` and absent timestamps are what the column-aligned listing
// gets wrong, so they are reproduced here deliberately.
const LISTING = [
  "Path = D:\\out\\App_1.2.3_x64-setup.exe",
  "Type = Nsis",
  "",
  "----------",
  "Path = $PLUGINSDIR\\System.dll",
  "Size = 12288",
  "Attributes = ",
  "",
  "Path = app with spaces.exe",
  "Size = 13750736",
  "Attributes = ",
  "",
  "Path = legal",
  "Size = 0",
  "Folder = +",
  "Attributes = D",
  "",
  "Path = legal\\EULA.md",
  "Size = 16723",
  "Attributes = A",
  "",
].join("\n");

test("parses 7-Zip technical listings, including blank dates and spaced names", () => {
  const entries = parseSevenZipListing(LISTING);
  assert.deepEqual(entries.map((entry) => entry.name), [
    "$PLUGINSDIR/System.dll",
    "app with spaces.exe",
    "legal/EULA.md",
  ]);
  assert.equal(entries[1].sizeBytes, 13_750_736);
});

test("ignores the archive's own record above the rule", () => {
  const entries = parseSevenZipListing(LISTING);
  assert.equal(entries.some((entry) => entry.name.endsWith("setup.exe")), false);
});

test("listNsisPayload surfaces the 7-Zip failure instead of reporting an empty payload", () => {
  assert.throws(
    () => listNsisPayload("installer.exe", {
      sevenZip: "7z",
      run: () => ({ status: 2, stdout: "", stderr: "Cannot open the file as archive" }),
    }),
    /could not read the NSIS payload.*Cannot open the file as archive/s,
  );
});

test("verifyNsisEmbeddedBinary accepts a payload whose bytes match the signed EXE", () => {
  const installer = path.join(workspace, "setup.exe");
  writeFileSync(installer, "installer-container");
  const payload = Buffer.from("signed-executable-bytes");
  const sha256 = createHash("sha256").update(payload).digest("hex");

  const receipt = verifyNsisEmbeddedBinary({
    installer,
    entryName: "app.exe",
    expectedSha256: sha256,
    sevenZip: "7z",
    run: (_command, args) => {
      const destination = args.find((arg) => arg.startsWith("-o")).slice(2);
      writeFileSync(path.join(destination, "app.exe"), payload);
      return { status: 0, stdout: "Everything is Ok", stderr: "" };
    },
  });

  assert.equal(receipt.sha256, sha256);
  assert.equal(receipt.entry, "app.exe");
  assert.equal(receipt.sizeBytes, payload.length);
  assert.equal(receipt.installerSha256, createHash("sha256").update(readFileSync(installer)).digest("hex"));
});

test("verifyNsisEmbeddedBinary rejects a payload that drifted from the signed EXE", () => {
  const installer = path.join(workspace, "setup-drifted.exe");
  writeFileSync(installer, "installer-container");
  assert.throws(
    () => verifyNsisEmbeddedBinary({
      installer,
      entryName: "app.exe",
      expectedSha256: createHash("sha256").update("signed-executable-bytes").digest("hex"),
      sevenZip: "7z",
      run: (_command, args) => {
        const destination = args.find((arg) => arg.startsWith("-o")).slice(2);
        writeFileSync(path.join(destination, "app.exe"), "rewritten-after-signing");
        return { status: 0, stdout: "Everything is Ok", stderr: "" };
      },
    }),
    /does not embed the signed app\.exe.*Authenticode signature is invalid/s,
  );
});

test("verifyNsisEmbeddedBinary fails when the entry is absent rather than passing vacuously", () => {
  const installer = path.join(workspace, "setup-missing.exe");
  writeFileSync(installer, "installer-container");
  assert.throws(
    () => verifyNsisEmbeddedBinary({
      installer,
      entryName: "app.exe",
      expectedSha256: "a".repeat(64),
      sevenZip: "7z",
      run: () => ({ status: 2, stdout: "", stderr: "No files to process" }),
    }),
    /could not extract app\.exe/,
  );
});

test("verifyNsisEmbeddedBinary refuses a caller that has no digest to compare against", () => {
  assert.throws(
    () => verifyNsisEmbeddedBinary({ installer: "setup.exe", entryName: "app.exe", expectedSha256: undefined, sevenZip: "7z", run: () => ({ status: 0 }) }),
    /expected a sha256 digest/,
  );
});

test("resolveSevenZip names the fix when 7-Zip is absent", () => {
  assert.throws(
    () => resolveSevenZip({ env: {}, exists: () => false, which: () => null }),
    /install 7-Zip.*RIGHT_RELEASE_SEVENZIP/s,
  );
});

test("resolveSevenZip honours an explicit override and rejects a bad one", () => {
  assert.equal(resolveSevenZip({ env: { RIGHT_RELEASE_SEVENZIP: "C:\\tools\\7z.exe" }, exists: () => true }), "C:\\tools\\7z.exe");
  assert.throws(() => resolveSevenZip({ env: { RIGHT_RELEASE_SEVENZIP: "C:\\gone\\7z.exe" }, exists: () => false }), /does not exist/);
});
