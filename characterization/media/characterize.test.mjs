import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { characterizeMedia } from "./characterize.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));

test("records the approved Windows artifact and both staged copies", async () => {
  const result = await characterizeMedia();
  const windows = result.artifacts.find((artifact) => artifact.id === "ffmpeg-7.1.5-windows-x64");

  assert.ok(windows, "Windows artifact missing from characterization");
  assert.equal(windows.status, "approved");
  assert.equal(windows.sha256, "ded75c0edf344b9eed5876cd6cc469b7366b00c4dd0f25b3965062965c33990d");

  // The app runtime copies are staged only on Windows, and characterize only runs the exe there.
  // Assert what is checkable everywhere — every copy present on this machine matches the approved
  // digest — so the gate can run on the Mac it is meant to be gating.
  for (const location of windows.locations) {
    if (location.exists) assert.equal(location.sha256, windows.sha256, `${location.role} drifted`);
  }
  assert.equal(windows.locations.find(({ role }) => role === "vendor-source")?.exists, true);

  if (process.platform === "win32") {
    assert.match(windows.versionOutput, /^ffmpeg version 7\.1\.5/m);
    assert.doesNotMatch(windows.versionOutput, /--enable-(?:gpl|nonfree)/i);
  }
});

test("the approved Mac artifacts are 7.1.5 and hash as recorded", async () => {
  const result = await characterizeMedia();
  const mac = result.artifacts.filter((artifact) => artifact.os === "macos");

  // Rebuilt from ffmpeg-7.1.5.tar.xz on 2026-07-15, LGPL/decode-only, and approved. Status alone is
  // a claim; the digest is the evidence, so both are asserted.
  assert.equal(mac.length, 2);
  for (const artifact of mac) {
    assert.equal(artifact.status, "approved");
    assert.equal(artifact.version, "7.1.5");
    assert.match(artifact.sha256, /^[a-f0-9]{64}$/);
    for (const location of artifact.locations ?? []) {
      if (location.exists) assert.equal(location.sha256, artifact.sha256, `${artifact.id} drifted`);
    }
  }
});

test("captures matching PCM output but divergent argument and lifecycle behavior", async () => {
  const result = await characterizeMedia();
  const { heardright, scraperight } = result.apps;

  assert.deepEqual(heardright.pcm, scraperight.pcm);
  assert.deepEqual(heardright.pcm, {
    container: "wav",
    codec: "pcm_s16le",
    channels: 1,
    sampleRateHz: 16000,
    sampleScale: "i16 / i16::MAX",
  });
  assert.deepEqual(heardright.ffmpegArgs, [
    "-y", "-hide_banner", "-loglevel", "error", "-i", "<input>",
    "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le", "<output>",
  ]);
  assert.deepEqual(scraperight.ffmpegArgs, [
    "-y", "-i", "<input>", "-ac", "1", "-ar", "16000",
    "-c:a", "pcm_s16le", "<output>",
  ]);
  assert.deepEqual(heardright.lifecycle, {
    cancel: false,
    timeout: false,
    childWait: "blocking-output",
    processTreeKill: false,
  });
  assert.deepEqual(scraperight.lifecycle, {
    cancel: true,
    timeout: false,
    childWait: "poll-100ms-kill-and-wait",
    processTreeKill: false,
  });
});

test("records resolution, checksum, nonfree rejection, and attribution gaps from source", async () => {
  const result = await characterizeMedia();

  assert.deepEqual(result.apps.heardright.resolution, [
    "HR_FFMPEG_PATH-existing",
    "executable-sibling/runtime",
    "macos-Contents/Resources/runtime",
    "system-PATH",
  ]);
  assert.deepEqual(result.apps.scraperight.resolution, [
    "SCRAPERIGHT_FFMPEG-nonempty",
    "Tauri-resource/runtime",
    "engine-system-PATH-fallback",
  ]);
  assert.deepEqual(result.gaps.map(({ id }) => id), [
    "no-approved-digest-enforcement",
    "heardright-version-spawn-failure-passes",
    "absence-only-license-classification",
    "runtime-system-path-fallback",
    "heardright-no-cancel-or-timeout",
    "scraperight-no-timeout-or-process-tree-kill",
    "macos-approved-artifacts-missing",
    "heardright-incomplete-lgpl-conveyance",
  ]);
});

test("the extraction contract requires every identified gap to close before publication", async () => {
  const contract = JSON.parse(await readFile(path.join(here, "contract.json"), "utf8"));
  const result = await characterizeMedia();
  const required = new Set(contract.publicationGate.requiredGapClosures);

  assert.equal(required.size, result.gaps.length);
  for (const gap of result.gaps) {
    assert.ok(required.has(gap.id), `publication gate omits ${gap.id}`);
  }
  assert.equal(contract.publicationGate.publishRightkitMedia, false);
  assert.equal(contract.publicationGate.migrateApps, false);
});
