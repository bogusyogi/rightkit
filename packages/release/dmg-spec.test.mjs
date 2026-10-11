import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { parseDmgSpec, readDmgSpec } from "./dmg-spec.mjs";

// Not run on Windows: disk image specs use macOS paths.
const WINDOWS_SKIP = process.platform === "win32" ? "disk image specs use macOS paths" : false;

// release/appdmg.json from Pulse, verbatim.
const PULSE_SPEC = { title: "Pulse", "icon-size": 128, window: { size: { width: 640, height: 420 } },
  contents: [{ x: 180, y: 210, type: "file", path: "Pulse.app" }, { x: 460, y: 210, type: "link", path: "/Applications" }] };

test("the Pulse appdmg spec maps onto buildDmg options", () => {
  assert.deepEqual(parseDmgSpec(PULSE_SPEC, { app: "/stage/Pulse.app" }), {
    volname: "Pulse", iconSize: 128, window: [640, 420], appPosition: [180, 210], applicationsPosition: [460, 210],
  });
});

test("background, icon and window position resolve against the spec directory", { skip: WINDOWS_SKIP }, () => {
  const options = parseDmgSpec({ title: "T", background: "art/bg.png", icon: "art/vol.icns", format: "UDZO", filesystem: "HFS+",
    window: { position: { x: 50, y: 60 }, size: { width: 500, height: 300 } } }, { baseDir: "/repo/release" });
  assert.deepEqual(options, { volname: "T", background: "/repo/release/art/bg.png", volumeIcon: "/repo/release/art/vol.icns", window: [500, 300], windowPosition: [50, 60] });
  assert.equal(parseDmgSpec({ background: "/abs/bg.tiff" }).background, "/abs/bg.tiff");
});

test("fields the pipeline cannot render fail loudly instead of being dropped", () => {
  const unsupported = (spec, pattern) => assert.throws(() => parseDmgSpec(spec), (error) => { assert.equal(error.code, "DMG_SPEC_UNSUPPORTED"); assert.match(error.message, pattern); return true; });
  unsupported({ "background-color": "#fff" }, /background-color/);
  unsupported({ "code-sign": { "signing-identity": "x" } }, /right-release Developer ID signs the DMG/);
  unsupported({ license: "x.json" }, /license/);
  unsupported({ format: "ULFO" }, /format "ULFO"/);
  unsupported({ filesystem: "APFS" }, /filesystem "APFS"/);
  unsupported({ mystery: 1 }, /unsupported field "mystery"/);
  unsupported({ window: { size: { width: 1, height: 1 }, mystery: 1 } }, /window\.mystery/);
  unsupported({ contents: [{ x: 1, y: 1, type: "link", path: "/Users" }] }, /only a link to \/Applications/);
  unsupported({ contents: [{ x: 1, y: 1, type: "file", path: "README.txt" }] }, /only the \.app can be a file entry/);
  unsupported({ contents: [{ x: 1, y: 1, type: "position", path: ".VolumeIcon.icns" }] }, /position entries/);
  unsupported({ contents: [{ x: 1, y: 1, type: "file", path: "A.app", mystery: 1 }] }, /unsupported field "mystery"/);
});

test("malformed specs fail with DMG_SPEC_INVALID", () => {
  const invalid = (spec, pattern, options) => assert.throws(() => parseDmgSpec(spec, options), (error) => { assert.equal(error.code, "DMG_SPEC_INVALID"); assert.match(error.message, pattern); return true; });
  invalid([], /JSON object/);
  invalid({ title: "" }, /title/);
  invalid({ "icon-size": 4 }, /icon-size/);
  invalid({ window: { size: { width: 0, height: 10 } } }, /window\.size\.width/);
  invalid({ window: { position: { x: -1, y: 0 } } }, /window\.position\.x/);
  invalid({ contents: [{ x: 1.5, y: 1, type: "link", path: "/Applications" }] }, /contents\[0\]\.x/);
  invalid({ contents: [{ x: 1, y: 1, type: "alias", path: "x" }] }, /type must be/);
  invalid({ contents: [{ x: 1, y: 1, type: "file", path: "Other.app" }] }, /names Other\.app but the app being packed is Pulse\.app/, { app: "/s/Pulse.app" });
  invalid({ contents: [{ x: 1, y: 1, type: "link", path: "/Applications" }, { x: 2, y: 2, type: "link", path: "/Applications" }] }, /duplicate/);
});

test("readDmgSpec reads a file and reports unreadable or non-JSON specs", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "rk-dmg-spec-"));
  try {
    const file = path.join(dir, "appdmg.json");
    writeFileSync(file, JSON.stringify({ ...PULSE_SPEC, background: "bg.png" }));
    assert.equal(readDmgSpec(file, { app: "/s/Pulse.app" }).background, path.join(dir, "bg.png"));
    writeFileSync(file, "{");
    assert.throws(() => readDmgSpec(file), /not valid JSON/);
    assert.throws(() => readDmgSpec(path.join(dir, "missing.json")), /not readable/);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
