import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { PINS_FILE, pinFor, platformKey, provision } from "./provision-actionlint.mjs";

const pins = JSON.parse(fs.readFileSync(PINS_FILE, "utf8"));
const scratch = (t) => { const dir = fs.mkdtempSync(path.join(process.env.RIGHTKIT_TEST_SCRATCH ?? os.tmpdir(), "rk-actionlint-")); t.after(() => fs.rmSync(dir, { recursive: true, force: true })); return dir; };
const served = (bytes, status = 200) => async (url) => ({ ok: status === 200, status, url, arrayBuffer: async () => bytes });

test("the pins cover both verify hosts and Linux, and Linux is the scans job's own pin", () => {
  for (const key of ["linux-x64", "darwin-arm64", "darwin-x64", "win32-x64"]) assert.match(pinFor(pins, key).url, /^https:\/\/github\.com\/rhysd\/actionlint\/releases\/download\/v1\.7\.12\/actionlint_1\.7\.12_/);
  const workflow = fs.readFileSync(new URL("../../.github/workflows/publish.yml", import.meta.url), "utf8");
  assert.ok(workflow.includes(`ACTIONLINT_VERSION: "${pins.version}"`) && workflow.includes(`ACTIONLINT_SHA256: "${pins.platforms["linux-x64"].sha256}"`), "scans and verify pin the same actionlint");
  assert.equal(platformKey("darwin", "arm64"), "darwin-arm64"); assert.equal(platformKey("win32", "x64"), "win32-x64");
  assert.throws(() => platformKey("win32", "arm64"), /no pinned actionlint for win32-arm64/);
  assert.throws(() => pinFor({ ...pins, version: "1.7.11" }, "linux-x64"), /not the pinned version's/);
  assert.throws(() => pinFor({ ...pins, platforms: { ...pins.platforms, "win32-x64": { archive: pins.platforms["win32-x64"].archive, sha256: "" } } }, "win32-x64"), /no complete entry/);
});

test("red: bytes that are not the pinned archive, a failed download, or a binary reporting another version are refused", async (t) => {
  const dest = scratch(t);
  await assert.rejects(provision({ pins, key: "linux-x64", dest, fetchImpl: served(Buffer.from("not actionlint")) }), /sha256 is [0-9a-f]{64}, pinned 8aca8db9/);
  await assert.rejects(provision({ pins, key: "linux-x64", dest, fetchImpl: served(Buffer.alloc(0), 404) }), /returned 404/);
  // Matching bytes, but the unpacked binary reports another version.
  const bytes = Buffer.from("pretend archive");
  const forged = { ...pins, platforms: { ...pins.platforms, "linux-x64": { ...pins.platforms["linux-x64"], sha256: (await import("node:crypto")).createHash("sha256").update(bytes).digest("hex") } } };
  const run = (cmd) => (cmd === "tar" ? { status: 0 } : { status: 0, stdout: "1.7.11\ninstalled by building from source\n" });
  await assert.rejects(provision({ pins: forged, key: "linux-x64", dest, fetchImpl: served(bytes), run }), /reports "1\.7\.11", pinned 1\.7\.12/);
});

// The real pinned Linux archive, when this host has a copy (RIGHTKIT_ACTIONLINT_ARCHIVE): unpacked and run for real.
const archive = process.env.RIGHTKIT_ACTIONLINT_ARCHIVE;
test("positive: the real pinned Linux archive unpacks and reports the pinned version", { skip: !(archive && process.platform === "linux" && process.arch === "x64") && "set RIGHTKIT_ACTIONLINT_ARCHIVE to the pinned linux_amd64 archive" }, async (t) => {
  const dest = scratch(t);
  const dir = await provision({ pins, key: "linux-x64", dest, fetchImpl: served(fs.readFileSync(archive)) });
  assert.ok(fs.statSync(path.join(dir, "actionlint")).isFile());
});
