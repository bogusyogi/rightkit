// The Cargo home is named by where it really is (bin/rightkit-gate.mjs cargoHomeOf). Dell 2026-11-19: an agent profile's `.cargo` is a link to
// the user's Cargo home; the workspace proof refuses any discovery input reached through a link (snapshot-metadata.mjs cargoWorkspaceTopology),
// so every workspace came back unproven and its `cargo metadata` row ran real Cargo on every run (NN01, NN03, NN04, NN10, NN21, NN24).
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { cargoHomeOf } from "./bin/rightkit-gate.mjs";
import { cargoWorkspaceTopology } from "./snapshot-metadata.mjs";

/** A real Cargo home holding a config file, and a second name that reaches it through a link (a junction on Windows). */
function homes(t) {
  const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-cargo-home-")));
  t.after(() => fs.rmSync(base, { recursive: true, force: true }));
  const real = path.join(base, "user", ".cargo"); fs.mkdirSync(real, { recursive: true });
  fs.writeFileSync(path.join(real, "config.toml"), "[net]\nretry = 2\n");
  const profile = path.join(base, "agent-profile"); fs.mkdirSync(profile);
  const linked = path.join(profile, ".cargo");
  fs.symlinkSync(real, linked, process.platform === "win32" ? "junction" : "dir");
  return { base, real, linked, profile };
}

test("a linked Cargo home is named by its target: CARGO_HOME, the Windows profile and the POSIX home alike", (t) => {
  const h = homes(t);
  assert.equal(cargoHomeOf({ CARGO_HOME: h.linked }), h.real);
  assert.equal(cargoHomeOf({ USERPROFILE: h.profile }), h.real);
  assert.equal(cargoHomeOf({ HOME: h.profile }), h.real);
  assert.equal(cargoHomeOf({}, { homedir: () => h.profile }), h.real);
  assert.equal(cargoHomeOf({ CARGO_HOME: h.real }), h.real, "a Cargo home that is no link is itself");
  // CARGO_HOME wins over the profile, as it does for Cargo.
  assert.equal(cargoHomeOf({ CARGO_HOME: h.real, USERPROFILE: path.join(h.base, "elsewhere"), HOME: path.join(h.base, "elsewhere") }), h.real);
});

test("a Cargo home that does not exist keeps its spelled path", (t) => {
  const h = homes(t);
  const missing = path.join(h.base, "nobody", ".cargo");
  assert.equal(cargoHomeOf({ CARGO_HOME: missing }), missing);
  assert.equal(cargoHomeOf({ USERPROFILE: path.join(h.base, "nobody") }), missing);
});

test("the workspace proof refuses the linked name and accepts the name cargoHomeOf gives", (t) => {
  const h = homes(t);
  const root = path.join(h.base, "snapshot"); fs.mkdirSync(path.join(root, "src"), { recursive: true });
  fs.writeFileSync(path.join(root, "Cargo.toml"), "[package]\nname = \"witness\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
  fs.writeFileSync(path.join(root, "src", "lib.rs"), "");
  const direct = cargoWorkspaceTopology({ root, dir: ".", cargoHome: h.real });
  // The proof also reads every directory above the workspace. Where one of those already defeats it (a Cargo.toml or a linked `.cargo` above
  // the temporary directory), no Cargo home can be proven on this host and there is nothing to compare.
  if (!direct) { t.skip("this host's temporary directory cannot be proven at all (a Cargo.toml or a linked .cargo above it)"); return; }
  assert.equal(cargoWorkspaceTopology({ root, dir: ".", cargoHome: h.linked }), null, "through the link the proof refuses: this is what made every workspace unproven");
  assert.deepEqual(cargoWorkspaceTopology({ root, dir: ".", cargoHome: cargoHomeOf({ CARGO_HOME: h.linked }) }), direct);
});
