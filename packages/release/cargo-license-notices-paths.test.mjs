import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { generateCargoNotices } from "./cargo-license-notices.mjs";

// rightkit#1718: the broker runs cargo-about with no working directory of the app's, so every path it reads is absolute.
test("cargo about gets absolute --config, --manifest-path and template paths, whatever the runner's working directory", () => {
  const appRoot = path.resolve("/repo/apps/coderight-tauri");
  const calls = [];
  const config = { app: "CodeRight", legal: { cargoNotices: { output: "legal/THIRD_PARTY_RUST_LICENSES.html", template: "legal/rust-licenses.hbs", config: "about.toml", manifestPath: "src-tauri/Cargo.toml", version: "1.0.0" } } };
  generateCargoNotices({
    appRoot, config, check: true,
    run: (args) => { calls.push(args); return { status: 0, stdout: "<html><body><p>licenses</p></body></html>" }; },
    exists: (file) => !String(file).endsWith(`${path.sep}.git`) || path.dirname(file) === path.resolve("/repo"),
    readText: () => "", readBytes: () => Buffer.from("lock"),
  });
  assert.equal(calls.length, 1);
  const args = calls[0];
  assert.equal(args[args.indexOf("--config") + 1], path.join(appRoot, "about.toml"));
  assert.equal(args[args.indexOf("--manifest-path") + 1], path.join(appRoot, "src-tauri", "Cargo.toml"));
  assert.equal(args.at(-1), path.join(appRoot, "legal", "rust-licenses.hbs"));
  assert.ok(args.filter((a) => /\.(toml|hbs)$/.test(a)).every((a) => path.isAbsolute(a)), args.join(" "));
});
