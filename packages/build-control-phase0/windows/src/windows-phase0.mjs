import { execFile as execFileCb } from "node:child_process";
import { createHash } from "node:crypto";
import { promisify } from "node:util";
import { blocker, probe, report } from "./schema.mjs";

const execFile = promisify(execFileCb);
const PS = "powershell.exe";

export async function powershell(script, exec = execFile) {
  const { stdout } = await exec(PS, ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script], { maxBuffer: 8 * 1024 * 1024 });
  return stdout.trim();
}

export function parseJson(text, label) {
  try { return JSON.parse(text); } catch (error) { throw new Error(`${label} returned invalid JSON: ${error.message}`); }
}

export async function inventory(exec = execFile) {
  const script = "$ErrorActionPreference='Stop';$os=Get-CimInstance Win32_OperatingSystem;$cs=Get-CimInstance Win32_ComputerSystem;$vol=Get-Volume|Select-Object DriveLetter,FileSystem,Size,SizeRemaining,HealthStatus,Path;$cargo=Get-Command cargo,rustup,rustc,node,pnpm,python,cl,go,swift -ErrorAction SilentlyContinue|Select-Object Name,Source,Version;$mods=@();Get-ChildItem -Path $env:USERPROFILE -Recurse -Force -ErrorAction SilentlyContinue -Include *.node,*.pyd,*.dll | Select-Object -First 2000 FullName,Length,LastWriteTimeUtc | ForEach-Object {$mods += $_};[pscustomobject]@{os=@{caption=$os.Caption;build=$os.BuildNumber;version=$os.Version;architecture=$os.OSArchitecture};machine=@{name=$env:COMPUTERNAME;model=$cs.Model;ramBytes=[int64]$cs.TotalPhysicalMemory};volumes=$vol;entrypoints=$cargo;nativeModules=$mods;nativeModuleInventoryTruncated=($mods.Count -ge 2000)}|ConvertTo-Json -Depth 6 -Compress";
  return parseJson(await powershell(script, exec), "inventory");
}

export async function appControl(exec = execFile) {
  const script = "$ErrorActionPreference='SilentlyContinue';$ci=Get-Command CiTool.exe -ErrorAction SilentlyContinue;$config=Get-Command Get-CIPolicy -ErrorAction SilentlyContinue;$pol=@();if($ci){$raw=& $ci.Source -lp -json 2>$null;if($LASTEXITCODE -eq 0 -and $raw){try{$pol=$raw|ConvertFrom-Json}catch{$pol=@{raw=$raw}}}};[pscustomobject]@{ciTool=if($ci){$ci.Source}else{$null};configCi=if($config){$config.Source}else{$null};policies=$pol;isgEnabled=$null;managedInstallerEnabled=$null}|ConvertTo-Json -Depth 8 -Compress";
  return parseJson(await powershell(script, exec), "app-control audit");
}

export async function vhdx(exec = execFile) {
  const script = "$ErrorActionPreference='SilentlyContinue';$cmd=Get-Command Get-VHD -ErrorAction SilentlyContinue;$v=@();if($cmd){$v=Get-VHD -ErrorAction SilentlyContinue|Select-Object Path,VhdType,Size,FileSize,Attached,MinimumSize};[pscustomobject]@{getVhdAvailable=[bool]$cmd;vhdx=$v;requiresAdmin=($null -eq $v)}|ConvertTo-Json -Depth 5 -Compress";
  return parseJson(await powershell(script, exec), "vhdx audit");
}

export async function nativeModuleCatalog(exec = execFile) {
  // Discovery only.  It deliberately hashes installed payloads but never creates,
  // signs, deploys, or activates a Code Integrity policy.
  const script = "$ErrorActionPreference='Stop';$roots=@();$py=Get-Command python -ErrorAction SilentlyContinue;if($py){$roots += (& $py.Source -c \"import json,site; print(json.dumps([*site.getsitepackages(),site.getusersitepackages()]))\" 2>$null|ConvertFrom-Json)};$node=Get-Command node -ErrorAction SilentlyContinue;if($node){$global=(& $node.Source -p \"require('module').globalPaths.join('\\n')\" 2>$null);if($global){$roots += $global -split \"`n\"}};$workspaceNode=Join-Path ((Get-Location).Path.Substring(0,((Get-Location).Path.IndexOf('\\tools\\rightkit')))) 'node_modules';if(Test-Path $workspaceNode){$roots += $workspaceNode};$roots += @((Join-Path $env:USERPROFILE '.cargo\\registry'),(Join-Path $env:USERPROFILE '.cargo\\git'),(Join-Path ((Get-Location).Path.Substring(0,((Get-Location).Path.IndexOf('\\tools\\rightkit')))) '.cache'));$seen=@{};$items=@();foreach($root in $roots|Where-Object {$_ -and (Test-Path $_)}){Get-ChildItem -LiteralPath $root -Recurse -File -Force -ErrorAction SilentlyContinue|Where-Object {$_.Extension -in '.node','.pyd','.dll'}|ForEach-Object {$key=$_.FullName.ToLowerInvariant();if(!$seen.ContainsKey($key)){$seen[$key]=$true;$kind=if($_.Extension -eq '.node'){'node'}elseif($_.Extension -eq '.pyd'){'python'}elseif($_.FullName.ToLowerInvariant().Contains('.cargo\\')){'rust'}else{'shared'};$sha=[System.Security.Cryptography.SHA256]::Create();try{$bytes=[System.IO.File]::ReadAllBytes($_.FullName);$hash=([System.BitConverter]::ToString($sha.ComputeHash($bytes))).Replace('-','').ToLowerInvariant()}finally{$sha.Dispose()};$items += [pscustomobject]@{path=$_.FullName;kind=$kind;sizeBytes=$_.Length;sha256=$hash}}}};[pscustomobject]@{schema='rightkit.build-control.windows.native-module-catalog.v1';capturedAt=(Get-Date).ToUniversalTime().ToString('o');roots=($roots|Where-Object {$_}|Sort-Object -Unique);modules=($items|Sort-Object path);count=$items.Count;hashAlgorithm='SHA256';discoveryOnly=$true}|ConvertTo-Json -Depth 5 -Compress";
  return parseJson(await powershell(script, exec), "native-module catalog");
}

export function catalogScaleChurn(catalog, factor = 2, churnPercent = 10) {
  const modules = Array.isArray(catalog.modules) ? catalog.modules : [];
  const targetCount = modules.length * factor;
  const churnCount = Math.ceil(targetCount * churnPercent / 100);
  const rules = Array.from({ length: targetCount }, (_, index) => {
    const source = modules[index % Math.max(modules.length, 1)] || { path: "<no-observed-module>", kind: "unknown", sha256: "0".repeat(64), sizeBytes: 0 };
    // Policy test identities must be distinct without pretending they are payload copies.
    return { id: createHash("sha256").update(`${source.sha256}:${index}`).digest("hex"), sourceSha256: source.sha256, kind: source.kind, synthetic: index >= modules.length };
  });
  return { schema: "rightkit.build-control.windows.policy-load-input.v1", sourceModuleCount: modules.length, scaleFactor: factor, targetRuleCount: targetCount, churnRuleCount: churnCount, rules, activationPermitted: false, reason: "Load input only; activation requires sacrificial enforced ring and recovery policy." };
}

export function supplementalPolicyPackage(loadInput, generatedAt = new Date().toISOString()) {
  const rules = Array.isArray(loadInput.rules) ? loadInput.rules : [];
  const digest = createHash("sha256").update(JSON.stringify(rules)).digest("hex");
  return {
    schema: "rightkit.build-control.windows.supplemental-policy-package.v1",
    generatedAt,
    policyType: "WDAC supplemental hash policy input",
    ruleCount: rules.length,
    ruleDigest: digest,
    deployment: { mode: "NOT_DEPLOYED", requires: ["sacrificial Windows 11 Pro/Enterprise ring", "administrator-operated recovery policy", "base-policy identity", "post-activation attestation"], prohibitedOnCurrentHost: true },
    rollback: { required: true, priorPolicyDigestRequired: true, bootRecoveryDrillRequired: true },
    rules
  };
}

export function packageBenchmark(loadInput, startedAt = performance.now()) {
  const policy = supplementalPolicyPackage(loadInput, "1970-01-01T00:00:00.000Z");
  const elapsedMs = performance.now() - startedAt;
  return { schema: "rightkit.build-control.windows.package-benchmark.v1", samples: 1, catalogAndPackageMs: elapsedMs, activationP50Ms: null, activationP95Ms: null, activationP99Ms: null, endToEndAdmissionP99Ms: null, limits: { activationP99Ms: 60000, endToEndAdmissionP99Ms: 300000 }, policyRuleCount: policy.ruleCount, status: "BLOCKED_EXTERNAL", reason: "No policy activation or admission was attempted on non-sacrificial host." };
}

export function capacityModel(data, reserveBytes = 20 * 1024 ** 3) {
  const volumes = Array.isArray(data.volumes) ? data.volumes : [];
  const freeBytes = volumes.reduce((sum, volume) => sum + Number(volume.SizeRemaining || 0), 0);
  return { schema: "rightkit.build-control.capacity-model.v1", reserveBytes, observedFreeBytes: freeBytes, protectedNonCacheClasses: ["runtime", "policy", "receipts", "stage", "vhdx-host"], status: freeBytes > reserveBytes ? "OBSERVED" : "INSUFFICIENT" };
}

export function stagingPreflight(machine, stageRoot = "<external-stage-root>") {
  const entrypoints = Array.isArray(machine.entrypoints) ? machine.entrypoints : [];
  const writableUserEntrypoints = entrypoints.filter((entry) => /\\Users\\|\\Claude\\|\\AppData\\/i.test(entry.Source || entry.path || ""));
  return {
    schema: "rightkit.build-control.windows.staging-preflight.v1",
    proposedStageRoot: stageRoot,
    required: ["external-volume-guid", "filesystem-serial", "service-SID ACL", "git/worktree provenance", "external-path-dependency closure", "hash-based sync-back"],
    observedUserWritableEntrypoints: writableUserEntrypoints,
    status: "BLOCKED_EXTERNAL",
    reason: "No service-owned external stage or sacrificial admission ring has been provisioned."
  };
}

export function admissionBenchmark(samples, limitMs = 5 * 60 * 1000) {
  const values = samples.filter(Number.isFinite).sort((a, b) => a - b);
  const percentile = (p) => values.length ? values[Math.min(values.length - 1, Math.ceil(values.length * p) - 1)] : null;
  return { schema: "rightkit.build-control.windows.admission-benchmark.v1", samples: values.length, p50Ms: percentile(.5), p95Ms: percentile(.95), p99Ms: percentile(.99), limitMs, status: values.length && percentile(.99) <= limitMs ? "PASS" : "BLOCKED_EXTERNAL" };
}

export async function phase0(exec = execFile, now = new Date().toISOString()) {
  if (process.platform !== "win32") {
    return report({ outcome: "BLOCKED_EXTERNAL", generatedAt: now, machine: { platform: process.platform }, probes: [], blockers: [blocker("windows-host", "Run this harness on Windows through supported win bridge or locally.", "Windows Phase-0 probes")] });
  }
  const [machine, policy, disk, catalog] = await Promise.all([inventory(exec), appControl(exec), vhdx(exec), nativeModuleCatalog(exec)]);
  const loadInput = catalogScaleChurn(catalog);
  const policyPackage = supplementalPolicyPackage(loadInput, now);
  const base = [probe("machine-inventory", "PASS", "Read-only Windows inventory captured.", machine), probe("app-control-audit", "PASS", "Read-only App Control/CiTool state captured; no policy changed.", policy), probe("vhdx-audit", "PASS", "Read-only VHDX capability captured.", disk), probe("capacity-model", "PASS", "Capacity model derived from live volume inventory.", capacityModel(machine)), probe("native-module-catalog", "PASS", "Read-only Python/Node/Rust native-module catalog hashed.", catalog), probe("supplemental-policy-package", "PASS", "Deterministic deployment package generated without activation.", policyPackage), probe("package-benchmark", "BLOCKED_EXTERNAL", "2x policy package benchmark completed; activation remains sacrificial-only.", packageBenchmark(loadInput)), probe("staging-preflight", "BLOCKED_EXTERNAL", "Staging requires service-owned external storage and authenticated identity.", stagingPreflight(machine))];
  const blockers = [
    blocker("sacrificial-enforce-ring", "Dedicated sacrificial Windows 11 Pro/Enterprise VM or ring for enforced UMCI testing.", "UMCI default-deny, copied/renamed Cargo denial, generated-executable tests"),
    blocker("local-admin-recovery", "Administrator-operated App Control deployment plus tested recovery policy/boot path.", "Policy activation, rollback & recovery drill"),
    blocker("sacrificial-policy-activation", "Activate generated supplemental hash policy only in sacrificial ring, then measure compile/sign/deploy/refresh and self-service admission.", "2x catalog scale/churn activation P99 <= 60 seconds & end-to-end admission P99 <= five minutes")
  ];
  return report({ outcome: "BLOCKED_EXTERNAL", generatedAt: now, machine, probes: base, blockers, notes: ["Harness intentionally never enables UMCI, changes boot configuration, or deploys App Control policy."] });
}
