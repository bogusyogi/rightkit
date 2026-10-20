import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { generateBinaryNotices, parseBinaryNoticeArgs, renderBinaryNotices, resolveBinaryNoticeOptions, uncoveredBinaries } from "./binary-license-notices.mjs";

const APP = path.resolve("/rk-app");
const binary = (over = {}) => ({ name: "yt-dlp", version: "self-updating", license: "Unlicense", sourceUrl: "https://github.com/yt-dlp/yt-dlp", files: ["legal/yt-dlp.txt"], ...over });
const config = (over = {}, extra = {}) => ({ legal: { binaryNotices: { output: "legal/BIN.html", productName: "ScrapeRight", binaries: [binary()], ...extra, ...over } } });
const key = (name) => path.normalize(path.join(APP, name));
const fakeFiles = (initial) => {
  const files = new Map(Object.entries(initial).map(([k, v]) => [key(k), v]));
  return { files, exists: (f) => files.has(path.normalize(f)), readText: (f) => files.get(path.normalize(f)), writeText: (f, t) => files.set(path.normalize(f), t) };
};

test("the appendix lists every declared program with its licence text and hashes; the same input gives the same bytes", () => {
  const io = fakeFiles({ "legal/yt-dlp.txt": "This is free and unencumbered software\r\n" });
  const first = generateBinaryNotices({ appRoot: APP, config: config(), ...io });
  const html = io.files.get(key("legal/BIN.html"));
  assert.match(html, /<h2>yt-dlp self-updating<\/h2>/);
  assert.match(html, /free and unencumbered/);
  assert.match(html, /Source: https:\/\/github\.com\/yt-dlp\/yt-dlp/);
  assert.equal(first.binaryCount, 1);
  assert.equal(generateBinaryNotices({ appRoot: APP, config: config(), check: true, ...io }).upToDate, true);
  io.files.set(key("legal/BIN.html"), "stale");
  assert.equal(generateBinaryNotices({ appRoot: APP, config: config(), check: true, ...io }).upToDate, false);
  assert.equal(renderBinaryNotices({ productName: "P<", binaries: [binary()], texts: new Map([["legal/yt-dlp.txt", "a&b"]]) }).includes("P&lt;"), true);
});

test("a missing or empty licence text is an error (no text is invented)", () => {
  assert.throws(() => generateBinaryNotices({ appRoot: APP, config: config(), ...fakeFiles({}) }), /does not exist/);
  assert.throws(() => generateBinaryNotices({ appRoot: APP, config: config(), ...fakeFiles({ "legal/yt-dlp.txt": "  " }) }), /is empty/);
});

test("with a stage-sidecar receipt every staged non-catalog program must be declared", () => {
  const receipt = { files: [{ kind: "remote", name: "yt-dlp" }, { kind: "remote", name: "base.zip", entryName: "yt-dlp" }, { kind: "command", name: "helper-0", command: "helper" }, { kind: "ffmpeg", name: "ffmpeg" }, { kind: "cargo-bin", name: "engine" }] };
  assert.deepEqual(uncoveredBinaries(receipt, [binary()]), ["helper"]);
  assert.deepEqual(uncoveredBinaries(receipt, [binary({ covers: ["helper"] })]), []);
  const io = fakeFiles({ "legal/yt-dlp.txt": "text", "stage.json": JSON.stringify(receipt) });
  assert.throws(() => generateBinaryNotices({ appRoot: APP, config: config({}, { receipt: "stage.json" }), ...io }), /no declared licence notice: helper/);
  assert.equal(generateBinaryNotices({ appRoot: APP, config: config({ binaries: [binary({ covers: ["helper"] })] }, { receipt: "stage.json" }), ...io }).uncovered.length, 0);
  assert.throws(() => generateBinaryNotices({ appRoot: APP, config: config({}, { receipt: "missing.json" }), ...io }), /receipt missing\.json does not exist/);
});

test("options are validated", () => {
  for (const bad of [{ output: undefined }, { productName: "" }, { binaries: [] }, { binaries: [binary({ sourceUrl: "http://x" })] }, { binaries: [binary({ files: [] })] }, { binaries: [binary({ extra: 1 })] }, { binaries: [binary(), binary()] }, { binaries: [binary({ files: ["../x"] })] }, { nope: 1 }]) {
    assert.throws(() => resolveBinaryNoticeOptions(config(bad)), /legal\.binaryNotices|no output|no product/, JSON.stringify(bad));
  }
  assert.throws(() => resolveBinaryNoticeOptions({}), /must be an object/);
  assert.deepEqual(parseBinaryNoticeArgs(["--receipt", "r.json", "--check"]).receipt, "r.json");
});
