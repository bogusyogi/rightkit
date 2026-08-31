import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const legion = readFileSync(new URL("./bootstraps/legion/install.ps1", import.meta.url), "utf8");
const membrane = readFileSync(new URL("./bootstraps/membrane/install.ps1", import.meta.url), "utf8");

test("Legion bootstrap binds current signed GitHub installer & waits for cutover", () => {
  assert.match(legion, /releases\/download\/v0\.3\.4\/Legion-0\.3\.4-windows-x86_64-setup\.exe/);
  assert.match(legion, /1972f7a1eb1ccd428f758f0e4a05d50b7aac9264f7d1689bfc6ec122a2a0925b/);
  assert.match(legion, /AddSeconds\(15\)/);
  assert.match(legion, /Get-AuthenticodeSignature/);
});

test("Membrane bootstrap binds published signed GitHub installer", () => {
  assert.match(membrane, /releases\/download\/v0\.1\.12\/Membrane\.Hub_0\.1\.12_x64-setup\.exe/);
  assert.match(membrane, /8ad86f35b403c54f5f47e3398bb48bd4f426027e91f5e3c9d126f3498761719e/);
  assert.match(membrane, /Get-AuthenticodeSignature/);
});
