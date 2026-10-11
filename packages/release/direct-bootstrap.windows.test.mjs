import assert from "node:assert/strict";
import { spawnSync } from "./background-process.mjs";
import { mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { renderPowerShellBootstrap } from "./direct-bootstrap.mjs";
import { RIGHTRELEASE_MANIFEST_SIGNER } from "./sign-release-manifest.mjs";

const ps = (value) => `'${value.replaceAll("'", "''")}'`;
const bootstrap = () => renderPowerShellBootstrap({
  product: "probe", repository: "example/probe", bootstrapVersion: "1.0.1",
  acceptedManifestSigners: [{ ...RIGHTRELEASE_MANIFEST_SIGNER, certificateSha256: "e".repeat(64) }],
  installRootSubdir: "Bootstrap Test", executablePath: "probe.exe",
  activationArgs: ["activate"], statusArgs: ["status"],
  healthAssertions: [{ path: "healthy", equals: true }],
});

function runProbe(shell, body) {
  // The long form of the temp directory: the script compares junction targets, which Windows reports in long form.
  const root = mkdtempSync(join(realpathSync.native(tmpdir()), "rightrelease-process-"));
  try {
    const script = join(root, "install.ps1");
    const child = join(root, "child with spaces.cjs");
    writeFileSync(script, bootstrap());
    writeFileSync(child, `
const [mode, ...args] = process.argv.slice(2);
if (mode === 'args') process.stdout.write(JSON.stringify(args));
if (mode === 'fail') { process.stdout.write('child stdout'); process.stderr.write('child stderr'); process.exitCode = 7; }
if (mode === 'slow') { process.stdout.write('before timeout'); setTimeout(() => {}, 30000); }
`);
    const command = `
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false
$Root = ${ps(root)}
$Node = ${ps(process.execPath)}
$Child = ${ps(child)}
$Tokens = $null; $Errors = $null
$Ast = [System.Management.Automation.Language.Parser]::ParseFile(${ps(script)}, [ref]$Tokens, [ref]$Errors)
if ($Errors.Count) { throw ($Errors | Out-String) }
foreach ($Definition in $Ast.FindAll({ param($n) $n -is [System.Management.Automation.Language.FunctionDefinitionAst] }, $false)) {
  . ([scriptblock]::Create($Definition.Extent.Text))
}
${body}
`;
    const result = spawnSync(shell, ["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", Buffer.from(command, "utf16le").toString("base64")], {
      encoding: "utf8", windowsHide: true, timeout: 20000,
    });
    assert.equal(result.status, 0, [result.error?.message, result.stderr, result.stdout].filter(Boolean).join("\n"));
    const line = result.stdout.split(/\r?\n/).find((value) => value.startsWith("RESULT:"));
    assert.ok(line, result.stdout);
    return JSON.parse(line.slice(7));
  } finally {
    // root is a freshly allocated fixture, never the real install root.
    assert.ok(root.startsWith(join(realpathSync.native(tmpdir()), "rightrelease-process-")));
    rmSync(root, { recursive: true, force: true });
  }
}

for (const shell of ["powershell.exe", "pwsh.exe"]) {
  const options = { skip: process.platform !== "win32" };
  test(`${shell}: bounded launcher preserves arguments and zero exit`, options, () => {
    const args = ["hello world", "", 'say "hi"', "C:\\trailing space\\", "ü·中"];
    const result = runProbe(shell, `
$Output = Invoke-BoundedExe $Node @($Child, 'args', ${args.map(ps).join(", ")}) 5
Write-Output ('RESULT:' + (@{ exitCode=$LASTEXITCODE; output=$Output } | ConvertTo-Json -Compress))
`);
    assert.equal(result.exitCode, 0);
    assert.deepEqual(JSON.parse(result.output), args);
  });

  test(`${shell}: bounded launcher preserves nonzero diagnostics`, options, () => {
    const result = runProbe(shell, `
try { Invoke-BoundedExe $Node @($Child, 'fail') 5 | Out-Null; throw 'failure was accepted' }
catch { Write-Output ('RESULT:' + (@{ message=$_.Exception.Message; data=$_.Exception.Data; exitCode=$LASTEXITCODE } | ConvertTo-Json -Depth 8 -Compress)) }
`);
    assert.equal(result.exitCode, 7);
    assert.equal(result.data.ExitCode, 7);
    assert.equal(result.data.Stdout, "child stdout");
    assert.equal(result.data.Stderr, "child stderr");
    assert.match(result.message, /child stderr/);
  });

  test(`${shell}: timeout keeps partial output and is bounded`, options, () => {
    const result = runProbe(shell, `
try { Invoke-BoundedExe $Node @($Child, 'slow') 1 | Out-Null; throw 'timeout was accepted' }
catch { Write-Output ('RESULT:' + (@{ message=$_.Exception.Message; data=$_.Exception.Data } | ConvertTo-Json -Depth 8 -Compress)) }
`);
    assert.match(result.message, /Timed out after 1s/);
    assert.equal(result.data.Stdout, "before timeout");
    assert.equal(result.data.TimedOut, true);
  });

  for (const preserveActive of [false, true]) {
  test(`${shell}: failed rollback preserves primary error and ${preserveActive ? "keeps active" : "cleans inactive"} candidate`, options, () => {
    const result = runProbe(shell, `
$InstallRoot = $Root; $VersionsRoot = Join-Path $Root 'versions'
$PriorTarget = Join-Path $VersionsRoot '1.0.0'; $VersionRoot = Join-Path $VersionsRoot '1.0.1'
$Current = Join-Path $Root 'current'; $PreviousLink = Join-Path $Root '.current-previous'; $NextLink = Join-Path $Root '.current-next'
$Work = Join-Path $Root 'work'; $Journal = Join-Path $Root 'integration-journal.json'
New-Item -ItemType Directory -Path $PriorTarget,$VersionRoot,$Work -Force | Out-Null
Set-Content -LiteralPath (Join-Path $PriorTarget 'keep.txt') -Value 'prior data'
Set-Content -LiteralPath $Journal -Value '{}'
New-Item -ItemType Junction -Path $Current -Target $PriorTarget | Out-Null
New-Item -ItemType Junction -Path $NextLink -Target $VersionRoot | Out-Null
$Version = '1.0.1'; $PriorVersion = '1.0.0'; $PriorCliCapable = $true
$ActivationArgsTemplate = @('activate'); $StatusArgsTemplate = @('status')
$Switched = $false; $PriorDetached = $false; $script:Calls = 0
function Invoke-BoundedExe { $script:Calls++; if ($script:Calls -eq 1) { $Failure = New-Object System.Exception 'original activation failure'; $Failure.Data['ExitCode'] = 7; throw $Failure }; throw 'secondary rollback failure' }
${preserveActive ? "function Remove-StableJunction { throw 'pointer restoration failure' }" : ""}
$Transaction = $Ast.Find({ param($n) $n -is [System.Management.Automation.Language.TryStatementAst] -and $n.Body.Extent.Text.Contains('$StableExe =') }, $false)
try { . ([scriptblock]::Create($Transaction.Extent.Text)); throw 'installation unexpectedly succeeded' }
catch { Write-Output ('RESULT:' + (@{ message=$_.Exception.Message; data=@{ ExitCode=$_.Exception.Data['ExitCode']; RollbackErrors=$_.Exception.Data['RollbackErrors'] }; priorRestored=([string](Get-Item $Current).Target -eq $PriorTarget); priorData=([IO.File]::ReadAllText((Join-Path $PriorTarget 'keep.txt')).Trim()); candidateExists=(Test-Path $VersionRoot); workExists=(Test-Path $Work) } | ConvertTo-Json -Depth 8 -Compress)) }
`);
    assert.equal(result.message, "original activation failure");
    assert.equal(result.data.ExitCode, 7);
    assert.match(JSON.stringify(result.data.RollbackErrors), preserveActive ? /pointer restoration failure.*still active/ : /secondary rollback failure/);
    assert.equal(result.priorRestored, !preserveActive);
    assert.equal(result.priorData, "prior data");
    assert.equal(result.candidateExists, preserveActive);
    assert.equal(result.workExists, false);
  });
  }
}
