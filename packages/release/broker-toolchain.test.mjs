import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { BROKER_TOOLCHAIN_SCHEMA, brokerToolchain, cargoVersionText, channelVersion, hostConfigFor } from "./broker-toolchain.mjs";

const sha = (b) => createHash("sha256").update(b).digest("hex");

/** A managed root with a launcher, a toolchain of two programs and a host configuration registering them. `edit(config)` may change the configuration. */
function host(edit = (c) => c, { manifest = '[pkg.cargo]\nversion = "0.100.0 (5f94df478 2026-08-27)"\n\n[pkg.rust]\nversion = "9.9.9 (not this one)"\n\n[pkg.rustc]\nversion = "1.99.1 (58cf8f9c3 2026-10-13)"\n' } = {}) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-broker-toolchain-")));
  const bin = path.join(root, "toolchains", "1.99.1-x", "bin");
  fs.mkdirSync(bin, { recursive: true }); fs.mkdirSync(path.join(root, "toolchains", "1.99.1-x", "lib", "rustlib"), { recursive: true }); fs.mkdirSync(path.join(root, "managed", "agent-bin"), { recursive: true });
  const cargo = path.join(bin, "cargo.exe"); const rustc = path.join(bin, "rustc.exe");
  fs.writeFileSync(cargo, "cargo bytes"); fs.writeFileSync(rustc, "rustc bytes");
  if (manifest !== null) fs.writeFileSync(path.join(root, "toolchains", "1.99.1-x", "lib", "rustlib", "multirust-channel-manifest.toml"), manifest);
  const launcher = path.join(root, "managed", "agent-bin", "rustright.cmd"); fs.writeFileSync(launcher, "@echo off\n");
  const config = edit({ schemaVersion: 2, tools: [{ id: "cargo", path: cargo, sha256: sha("cargo bytes") }, { id: "rustc-tool", path: rustc, sha256: sha("rustc bytes") }], shims: { cargo: "cargo", rustc: "other" }, defaults: { rustcToolId: "rustc-tool", toolchain: "1.99.1-x" } });
  fs.writeFileSync(path.join(root, "managed", "config.json"), JSON.stringify(config));
  return { root, launcher, cargo, rustc, done: () => fs.rmSync(root, { recursive: true, force: true }) };
}

test("the host configuration is two levels above the launcher; a bare name has none", () => {
  assert.equal(hostConfigFor("D:\\managed\\agent-bin\\rustright.cmd", path.win32), "D:\\managed\\config.json");
  assert.equal(hostConfigFor("rustright", path.win32), null);
  assert.equal(brokerToolchain({ launcher: "rustright" }).ok, false);
});

test("channelVersion reads the version of the named package only", () => {
  const text = '[pkg.cargo]\nversion = "0.100.0 (a 2026-08-27)"\n[pkg.rust]\nversion = "x"\n[pkg.rustc]\ngit_commit_hash = "z"\nversion = "1.99.1 (b 2026-10-13)"\n[pkg.rustc.target.x]\nversion = "no"\n';
  assert.equal(channelVersion(text, "cargo"), "0.100.0 (a 2026-08-27)");
  assert.equal(channelVersion(text, "rustc"), "1.99.1 (b 2026-10-13)");
  assert.equal(channelVersion(text, "clippy"), null);
  assert.equal(channelVersion('[pkg.rustc]\n[pkg.other]\nversion = "1"\n', "rustc"), null);
});

test("cargo is named as cargo names itself: the Rust release it ships with and its own commit and date", () => {
  assert.equal(cargoVersionText("0.100.0 (5f94df478 2026-08-27)", "1.99.1 (58cf8f9c3 2026-10-13)"), "cargo 1.99.1 (5f94df478 2026-08-27)");
  assert.equal(cargoVersionText("0.99.0 (0123abc 2026-07-01)", "1.98.0 (fedcba9 2026-08-20)"), "cargo 1.98.0 (0123abc 2026-07-01)");
  assert.equal(cargoVersionText("0.101.0-beta.2 (0123abc 2026-09-01)", "1.100.0-beta.2 (fedcba9 2026-09-02)"), "cargo 1.100.0-beta.2 (0123abc 2026-09-01)");
  // Anything that is not of that shape gives no text, and the caller then asks the client as before.
  assert.equal(cargoVersionText("0.100.0", "1.99.1 (58cf8f9c3 2026-10-13)"), null);
  assert.equal(cargoVersionText("0.100.0 (5f94df478 2026-08-27)", "1.99.1"), null);
  assert.equal(cargoVersionText("0.100.0 (not-a-commit 2026-08-27)", "1.99.1 (58cf8f9c3 2026-10-13)"), null);
  assert.equal(cargoVersionText(null, null), null);
});

test("the registered cargo and rustc are answered with their versions and bytes, and no process is started", () => {
  const h = host();
  try {
    const a = brokerToolchain({ launcher: h.launcher, remember: false });
    assert.equal(a.ok, true, a.why);
    assert.equal(a.schema, BROKER_TOOLCHAIN_SCHEMA);
    assert.equal(a.cargo.path, h.cargo); assert.equal(a.rustc.path, h.rustc);
    // As cargo names itself (`cargo --version` on this toolchain, Dell 2026-11-18: "cargo 1.99.1 (5f94df478 2026-08-27)"), never the crate version 0.100.0.
    assert.equal(a.cargo.version, `cargo 1.99.1 (5f94df478 2026-08-27) sha256:${sha("cargo bytes").slice(0, 16)}`);
    assert.equal(a.rustc.version, `rustc 1.99.1 (58cf8f9c3 2026-10-13) sha256:${sha("rustc bytes").slice(0, 16)}`);
    assert.match(a.rustc.version, /rustc (\d+\.\d+\.\d+)/); // the contract test reads the version out of the receipt's binding this way
    assert.equal(a.toolchain, "1.99.1-x");
    assert.equal(a.files.length, 4);
  } finally { h.done(); }
});

test("a program whose bytes are not the registered ones is refused, naming both digests", () => {
  const h = host();
  try {
    fs.writeFileSync(h.cargo, "another cargo");
    const a = brokerToolchain({ launcher: h.launcher, remember: false });
    assert.equal(a.ok, false);
    assert.match(a.why, /cargo\.exe is sha256 [0-9a-f]{64}, the host configuration registers [0-9a-f]{64}/);
  } finally { h.done(); }
});

test("every doubt answers ok: false with its reason", () => {
  const cases = [
    [(c) => ({ ...c, tools: undefined }), {}, /registers no tools/],
    [(c) => ({ ...c, shims: {} }), {}, /does not name which registered tool is cargo/],
    [(c) => ({ ...c, defaults: {}, shims: { cargo: "cargo" } }), {}, /does not name which registered tool is rustc/],
    [(c) => ({ ...c, tools: [...c.tools, c.tools[0]] }), {}, /registers 2 tools with the id cargo/],
    [(c) => ({ ...c, tools: c.tools.map((t) => (t.id === "cargo" ? { ...t, sha256: "nope" } : t)) }), {}, /no absolute path with a sha256/],
    [(c) => ({ ...c, tools: c.tools.map((t) => (t.id === "rustc-tool" ? { ...t, path: path.join(path.dirname(path.dirname(path.dirname(t.path))), "other", "bin", "rustc.exe") } : t)) }), {}, /not of one toolchain directory/],
    [(c) => c, { manifest: null }, /not a readable file: .*multirust-channel-manifest\.toml/],
    [(c) => c, { manifest: '[pkg.cargo]\nversion = "0.1.0 (x)"\n' }, /names no version for rustc/],
    [(c) => c, { manifest: '[pkg.cargo]\nversion = "0.100.0"\n[pkg.rustc]\nversion = "1.99.1 (58cf8f9c3 2026-10-13)"\n' }, /do not give the text cargo prints for itself/],
  ];
  for (const [edit, options, why] of cases) {
    const h = host(edit, options);
    try { const a = brokerToolchain({ launcher: h.launcher, remember: false }); assert.equal(a.ok, false); assert.match(a.why, why); } finally { h.done(); }
  }
  const h = host();
  try {
    fs.writeFileSync(path.join(h.root, "managed", "config.json"), "{ not json");
    assert.match(brokerToolchain({ launcher: h.launcher, remember: false }).why, /is not JSON/);
    fs.rmSync(path.join(h.root, "managed", "config.json"));
    assert.match(brokerToolchain({ launcher: h.launcher, remember: false }).why, /cannot read .*config\.json: ENOENT/);
  } finally { h.done(); }
});

test("a kept answer is not given back after the configuration or a program changed", () => {
  const h = host();
  try {
    const first = brokerToolchain({ launcher: h.launcher });
    assert.equal(first.ok, true, first.why);
    assert.equal(brokerToolchain({ launcher: h.launcher }), first); // same files, same state: the kept answer
    fs.writeFileSync(h.rustc, "rustc bytes, changed and longer");
    const second = brokerToolchain({ launcher: h.launcher });
    assert.equal(second.ok, false);
    assert.match(second.why, /rustc\.exe is sha256/);
  } finally { h.done(); }
});

test("a file that changes while it is read is refused", () => {
  const h = host();
  try {
    let reads = 0;
    const readFile = (f) => { const b = fs.readFileSync(f); reads += 1; if (reads === 3) fs.writeFileSync(h.cargo, "cargo bytes, changed after it was hashed"); return b; };
    const a = brokerToolchain({ launcher: h.launcher, readFile, remember: false });
    assert.equal(a.ok, false);
    assert.match(a.why, /changed while it was read/);
  } finally { h.done(); }
});
