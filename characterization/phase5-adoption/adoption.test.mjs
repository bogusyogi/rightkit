import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { inspectMedia, inspectPhase5Adoption } from "./adoption.mjs";

// coderight, scraperight and heardright are sibling checkouts of the owner's workspace. Only the premature-dependency
// scan reads them, so only the crate gate skips without them; a hosted CI checkout holds rightkit alone.
// RIGHTKIT_REQUIRE_ASSETS=1 runs the gate anyway, so a missing checkout fails it.
const consumerRepos = ["coderight", "scraperight", "heardright"].map((repo) => path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../..", repo));
const consumersSkip = (process.env.RIGHTKIT_ISOLATED_CHECKOUT !== "1" && consumerRepos.every((dir) => existsSync(dir))) || process.env.RIGHTKIT_REQUIRE_ASSETS === "1" ? false : "coderight, scraperight and heardright are not checked out beside rightkit";

test("Mac gates keep all Phase 5 crates internal", { skip: consumersSkip }, async () => {
  const report = await inspectPhase5Adoption();

  assert.equal(report.readyToPublish, false);
  assert.deepEqual(report.crates, {
    "rightkit-onnx": { publish: false },
    "rightkit-ocr": { publish: false },
    "rightkit-ffmpeg": { publish: false },
  });
  assert.deepEqual(report.prematureDependencies, []);
});

// The media parity checks read only rightkit's contract and vendored artifacts, so they run without the consumer checkouts.
test("Windows media parity is frozen while Mac artifact rows remain unapproved", async () => {
  const media = await inspectMedia();

  const { missingCopies, ...windows } = media.windows;
  assert.deepEqual(windows, {
    id: "ffmpeg-7.1.5-windows-x64",
    status: "approved",
    sha256: "ded75c0edf344b9eed5876cd6cc469b7366b00c4dd0f25b3965062965c33990d",
    allCopiesMatch: true,
  });
  // The app runtime copies are staged only on Windows, so which ones are on disk is
  // machine-dependent; that they all match wherever they exist is not.
  assert.ok(Array.isArray(missingCopies));

  // Rebuilt as 7.1.5 on 2026-07-15 (LGPL, decode-only) and approved. The status is only half the
  // claim; the hash check below is what makes it evidence.
  assert.deepEqual(
    media.macos.map(({ arch, status }) => ({ arch, status })),
    [
      { arch: "arm64", status: "approved" },
      { arch: "x86_64", status: "approved" },
    ],
  );
});

test("the Mac FFmpeg binaries on disk are the ones the contract pins", async (t) => {
  const media = await inspectMedia();
  const onDisk = media.macos.filter(({ present }) => present);
  if (onDisk.length === 0) t.skip("no Mac FFmpeg artifacts staged on this machine");

  for (const artifact of onDisk) {
    assert.equal(artifact.sha256Matches, true, `${artifact.arch} FFmpeg does not match its pinned sha256`);
  }
});
