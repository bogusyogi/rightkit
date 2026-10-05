import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const legion = readFileSync(new URL("./bootstraps/legion/install.ps1", import.meta.url), "utf8");
const membrane = readFileSync(new URL("./bootstraps/membrane/install.ps1", import.meta.url), "utf8");

test("Legion bootstrap binds signed installer to stable current repair, status, & bounded doctor", () => {
  assert.match(legion, /releases\/download\/v0\.3\.12\/Legion-0\.3\.12-windows-x86_64-setup\.exe/);
  assert.match(legion, /d9a8b90c58d1758363eb0ad94559dc4e0629a7c5966ad2b2b7b5cec35c333d3a/);
  assert.match(legion, /Get-AuthenticodeSignature/);
  assert.match(legion, /Orthic Labs\\Legion\\current/);
  assert.match(legion, /bin\\legion\.exe/);
  assert.match(legion, /@\('--json', 'setup', 'repair', '--confirm'\)/);
  assert.match(legion, /@\('--json', 'setup', 'status'\)/);
  assert.match(legion, /Invoke-BoundedCommand \$Executable @\('doctor'\)/);
  assert.match(legion, /@\('claude-code', 'codex'\)/);
  assert.match(legion, /projections\.claudePlugin\.state -ne 'current'/);
  assert.match(legion, /projections\.codexPlugin\.state -ne 'current'/);
  assert.match(legion, /WaitForExit\(\$CommandTimeoutSeconds \* 1000\)/);
  assert.match(legion, /rightkit\.branded-bootstrap\.lifecycle\.v1/);
  assert.doesNotMatch(legion, /(?:D:\\Claude|node_modules|target\\|repo(?:sitory)? fallback)/i);
});

test("Membrane bootstrap binds signed installer to stable current activation & bounded status", () => {
  assert.match(membrane, /releases\/download\/v0\.1\.12\/Membrane\.Hub_0\.1\.12_x64-setup\.exe/);
  assert.match(membrane, /8ad86f35b403c54f5f47e3398bb48bd4f426027e91f5e3c9d126f3498761719e/);
  assert.match(membrane, /Get-AuthenticodeSignature/);
  assert.match(membrane, /Orthic Labs\\Membrane\\current/);
  assert.match(membrane, /Join-Path \$InstallRoot 'membrane\.exe'/);
  assert.match(membrane, /@\('activate', '--install-root', \$QuotedInstallRoot\)/);
  assert.match(membrane, /@\('activate', '--install-root', \$QuotedInstallRoot, '--dry-run'\)/);
  assert.match(membrane, /MEMBRANE_STATUS_BINDING_INVALID/);
  assert.match(membrane, /Status\.versionRoot/);
  assert.match(membrane, /Status\.trayExecutable/);
  assert.match(membrane, /WaitForExit\(\$CommandTimeoutSeconds \* 1000\)/);
  assert.match(membrane, /rightkit\.branded-bootstrap\.lifecycle\.v1/);
  assert.doesNotMatch(membrane, /Join-Path \$env:LOCALAPPDATA 'Membrane Hub'/);
  assert.doesNotMatch(membrane, /(?:D:\\Claude|node_modules|target\\|repo(?:sitory)? fallback)/i);
});
