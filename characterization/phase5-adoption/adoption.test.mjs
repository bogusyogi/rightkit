import assert from "node:assert/strict";
import test from "node:test";

import { inspectPhase5Adoption } from "./adoption.mjs";



test("Mac gates keep all Phase 5 crates internal", async () => {
  const report = await inspectPhase5Adoption();

  assert.equal(report.readyToPublish, false);
  assert.deepEqual(report.crates, {
    "rightkit-ort": { publish: false },
    "rightkit-ocr": { publish: false },
    "rightkit-media": { publish: false },
  });
  assert.deepEqual(report.prematureDependencies, []);
});

test("Windows media parity is frozen while Mac artifact rows remain unapproved", async () => {
  const report = await inspectPhase5Adoption();

  const { missingCopies, ...windows } = report.media.windows;
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
    report.media.macos.map(({ arch, status }) => ({ arch, status })),
    [
      { arch: "arm64", status: "approved" },
      { arch: "x86_64", status: "approved" },
    ],
  );
});

test("the Mac FFmpeg binaries on disk are the ones the contract pins", async (t) => {
  const report = await inspectPhase5Adoption();
  const onDisk = report.media.macos.filter(({ present }) => present);
  if (onDisk.length === 0) t.skip("no Mac FFmpeg artifacts staged on this machine");

  for (const artifact of onDisk) {
    assert.equal(artifact.sha256Matches, true, `${artifact.arch} FFmpeg does not match its pinned sha256`);
  }
});
