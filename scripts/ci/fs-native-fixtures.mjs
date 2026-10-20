// Native fixture evidence only. Cargo execution is restricted to GitHub Actions.
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { unpackArtifact } from "./reviewed-candidate.mjs";
import { childEnv, loadSealed, checkReceipts, sha } from "./upload.mjs";
import { archiveTool, isWithin, runRegistryConsumer } from "./registry-consumer.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const stateFile = (env) => path.join(env.RUNNER_TEMP, "rk-fs-native-state.json");
const read = (file) => JSON.parse(fs.readFileSync(file, "utf8"));
const write = (file, value) => fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
const need = (ok, message) => { if (!ok) throw new Error(message); };
function ci(env) { need(env.GITHUB_ACTIONS === "true" && env.GITHUB_REF === "refs/heads/main", "native qualification requires main GitHub Actions"); }
function command(exe, args, options = {}) {
  const { logFile, ...spawnOptions } = options;
  const result = spawnSync(exe, args, { encoding: "utf8", windowsHide: true, timeout: 120_000, maxBuffer: 32 * 1024 * 1024, ...spawnOptions });
  if (logFile) fs.writeFileSync(logFile, `${result.stdout ?? ""}${result.stderr ?? ""}`);
  need(!result.error && result.status === 0, `${exe} failed (status ${result.status}): ${result.error?.message ?? `${result.stderr ?? ""}${result.stdout ?? ""}`.trim()}`);
  return result.stdout;
}
export function validateSource(run, jobs, repository, id) {
  need(/^[1-9]\d*$/.test(id), "qualified_source_run must be numeric");
  need(String(run.id) === id && run.repository?.full_name === repository && run.head_repository?.full_name === repository && run.path === ".github/workflows/publish.yml" && run.head_branch === "main" && run.event === "workflow_dispatch" && run.status === "completed" && /^[a-f0-9]{40}$/.test(run.head_sha) && Number.isSafeInteger(run.run_attempt) && run.run_attempt > 0, "wrong source run provenance");
  need(jobs.total_count === jobs.jobs?.length && jobs.total_count <= 100, "incomplete source job listing");
  for (const name of ["guard", "plan", "pack"]) need(jobs.jobs.filter((job) => job.name === name && job.conclusion === "success").length === 1, `${name} must succeed`);
  const verify = jobs.jobs.filter((job) => /^verify \(/.test(job.name));
  need(verify.length === 2 && verify.every((job) => job.conclusion === "success") && verify.some((job) => /^verify \(xcode-27, /.test(job.name)) && verify.some((job) => /^verify \(windows-2025, /.test(job.name)), "both native source verify jobs must succeed");
  need(jobs.jobs.every((job) => String(job.run_id) === id && job.head_sha === run.head_sha), "source job identity mismatch");
  return { sourceSha: run.head_sha, runId: id, runAttempt: String(run.run_attempt), repository };
}
export function selectPinnedArtifact(artifacts, name, context) {
  const selected = artifacts.filter((item) => item.name === name && !item.expired);
  need(selected.length === 1, `need exactly one ${name}`);
  const item = selected[0];
  need(Number.isSafeInteger(item.id) && item.id > 0 && /^sha256:[a-f0-9]{64}$/.test(item.digest ?? "") && item.size_in_bytes > 0 && item.size_in_bytes <= 512 * 1024 * 1024 && String(item.workflow_run?.id) === context.runId && item.workflow_run?.head_sha === context.sourceSha, "artifact provenance/digest missing");
  return item;
}
async function resolve(env) {
  ci(env);
  const id = env.FS_QUALIFIED_SOURCE_RUN;
  need(/^[1-9]\d*$/.test(id ?? "") && /^[\w.-]+\/[\w.-]+$/.test(env.GITHUB_REPOSITORY ?? ""), "explicit numeric qualified source run required");
  const api = async (route, binary = false) => {
    const response = await fetch(`https://api.github.com/repos/${env.GITHUB_REPOSITORY}/${route}`, { headers: { authorization: `Bearer ${env.GITHUB_TOKEN}`, accept: "application/vnd.github+json" }, redirect: "manual", signal: AbortSignal.timeout(60_000) });
    if (binary && response.status === 302) {
      const url = new URL(response.headers.get("location"));
      need(url.protocol === "https:" && (url.hostname.endsWith(".blob.core.windows.net") || url.hostname.endsWith(".actions.githubusercontent.com")), "unsafe artifact redirect");
      const download = await fetch(url, { redirect: "error", signal: AbortSignal.timeout(120_000) });
      need(download.ok, "artifact download failed");
      return Buffer.from(await download.arrayBuffer());
    }
    need(response.ok, `GitHub API failed: ${response.status}`);
    return binary ? Buffer.from(await response.arrayBuffer()) : response.json();
  };
  const run = await api(`actions/runs/${id}`);
  const jobs = await api(`actions/runs/${id}/attempts/${run.run_attempt}/jobs?per_page=100`);
  const context = validateSource(run, jobs, env.GITHUB_REPOSITORY, id);
  const listing = await api(`actions/runs/${id}/artifacts?per_page=100`);
  need(listing.total_count === listing.artifacts?.length && listing.total_count <= 100, "incomplete artifact listing");
  const bundle = env.FS_NATIVE_BUNDLE;
  fs.mkdirSync(bundle, { recursive: true });
  context.artifacts = {};
  for (const [kind, name] of Object.entries({ packed: "publish-packed", macos: "publish-receipt-verify-xcode-27", windows: "publish-receipt-verify-windows-2025" })) {
    const item = selectPinnedArtifact(listing.artifacts, name, context);
    const bytes = await api(`actions/artifacts/${item.id}/zip`, true);
    need(bytes.length <= 512 * 1024 * 1024 && `sha256:${sha("sha256", bytes)}` === item.digest, "downloaded artifact digest mismatch");
    const destination = kind === "packed" ? path.join(bundle, "packed") : path.join(bundle, "receipts", kind);
    unpackArtifact({ bytes, zipFile: path.join(bundle, `${kind}.zip`), destination });
    context.artifacts[kind] = { id: item.id, digest: item.digest };
  }
  const raw = read(path.join(bundle, "packed", "manifest.json"));
  const sealed = loadSealed({ packDir: path.join(bundle, "packed"), registry: "crates", unitsText: raw.units.map((unit) => unit.name).join(" "), run: context });
  need(!sealed.problems.length && sealed.manifest.commit === context.sourceSha && sealed.manifest.repository === context.repository, sealed.problems.join("; ") || "manifest source mismatch");
  const unit = sealed.manifest.units.filter((unit) => unit.name === "rightkit-fs");
  need(unit.length === 1 && unit[0].version === "0.4.6", "sealed rightkit-fs =0.4.6 required");
  const problems = checkReceipts({ manifest: sealed.manifest, manifestSha256: sealed.manifestSha256, receiptsDir: path.join(bundle, "receipts"), run: context });
  need(!problems.length, problems.join("; "));
  write(path.join(bundle, "context.json"), { ...context, manifestSha256: sealed.manifestSha256, archiveSha256: unit[0].sha256 });
}
export function fixturePlan(platform, scratch) {
  need(["win32", "darwin"].includes(platform), "native fixture host must be Windows or macOS");
  need(!/[\r\n"\0]/.test(scratch), "unsafe fixture path");
  return { schema: 1, platform, scratch, other: path.join(scratch, "other"), fat: platform === "win32" ? path.join(scratch, "fat") : null, images: platform === "win32" ? [path.join(scratch, "other.vhd"), path.join(scratch, "fat.vhd")] : [path.join(scratch, "other.sparseimage")] };
}
function diskpart(script, file) { fs.writeFileSync(file, `${script.join("\r\n")}\r\n`); return command("diskpart.exe", ["/s", file]); }
function setup(env) {
  ci(env);
  const temp = fs.realpathSync(env.RUNNER_TEMP);
  need(!isWithin(ROOT, temp), "fixtures must be outside workspace");
  need(!fs.existsSync(stateFile(env)), "existing fixture state requires cleanup first");
  const plan = fixturePlan(process.platform, fs.mkdtempSync(path.join(temp, "rk-fs-volume-")));
  write(stateFile(env), plan); // Persist before any native mount can take effect.
  if (plan.platform === "win32") {
    for (const [index, format] of ["ntfs", "fat32"].entries()) {
      const mount = index ? plan.fat : plan.other;
      fs.mkdirSync(mount);
      diskpart([`create vdisk file="${plan.images[index]}" maximum=512 type=expandable`, `select vdisk file="${plan.images[index]}"`, "attach vdisk", "create partition primary", `format fs=${format} quick label=RKFS${index}`, `assign mount="${mount}"`, "exit"], path.join(plan.scratch, `create-${index}.txt`));
    }
  } else {
    command("hdiutil", ["create", "-size", "512m", "-type", "SPARSE", "-fs", "APFS", "-volname", "RKFSNative", plan.images[0]]);
    fs.mkdirSync(plan.other);
    command("hdiutil", ["attach", "-nobrowse", "-owners", "on", "-mountpoint", plan.other, plan.images[0]]);
  }
  for (const mount of [plan.other, plan.fat].filter(Boolean)) {
    const probe = path.join(mount, "fixture-probe");
    fs.writeFileSync(probe, "fixture"); fs.unlinkSync(probe);
    need(fs.statSync(mount).dev !== fs.statSync(temp).dev, "fixture did not mount a separate volume");
  }
  // fsutil fsinfo volumeinfo refuses a mounted-folder path; Get-Volume -FilePath resolves the volume mounted there.
  const winFs = (mount) => { need(!/['`$]/.test(mount), "unsafe fixture path"); return command("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", `(Get-Volume -FilePath '${mount}\\').FileSystemType`]); };
  const info = { other: plan.platform === "win32" ? winFs(plan.other) : command("diskutil", ["info", plan.other]), fat: plan.fat ? winFs(plan.fat) : null };
  write(path.join(plan.scratch, "native-volume-info.txt"), info);
  need(plan.platform === "win32" ? /NTFS/i.test(info.other) && /FAT32/i.test(info.fat) : /APFS/i.test(info.other), "mounted filesystem type differs from required native fixture");
}
function cleanup(env) {
  ci(env);
  if (!fs.existsSync(stateFile(env))) return;
  const plan = read(stateFile(env));
  need(isWithin(fs.realpathSync(env.RUNNER_TEMP), plan.scratch) && !isWithin(ROOT, plan.scratch) && path.basename(plan.scratch).startsWith("rk-fs-volume-"), "refusing foreign cleanup root");
  need(JSON.stringify(plan) === JSON.stringify(fixturePlan(process.platform, plan.scratch)), "fixture cleanup state changed");
  const errors = [];
  for (const [index, image] of plan.images.entries()) {
    if (!fs.existsSync(image)) continue;
    try {
      if (plan.platform === "win32") diskpart([`select vdisk file="${image}"`, "detach vdisk", "exit"], path.join(plan.scratch, `detach-${index}.txt`));
      else {
        const info = command("hdiutil", ["info", "-plist"]);
        if (info.includes(image)) command("hdiutil", ["detach", plan.other]);
      }
    } catch (error) { errors.push(error.message); }
  }
  need(!errors.length, errors.join("; "));
  // Never recursively delete a volume mount. Failure to detach leaves state for investigation.
  for (const mount of [plan.other, plan.fat].filter(Boolean)) if (fs.existsSync(mount)) need(fs.statSync(mount).dev === fs.statSync(env.RUNNER_TEMP).dev, "fixture remains mounted; refusing recursive cleanup");
  fs.rmSync(plan.scratch, { recursive: true, force: true });
  fs.unlinkSync(stateFile(env));
  if (env.FS_NATIVE_OUT) {
    fs.mkdirSync(env.FS_NATIVE_OUT, { recursive: true });
    write(path.join(env.FS_NATIVE_OUT, "cleanup.json"), { schema: 1, detached: true, removed: true, host: process.platform });
  }
}
function qualify(env) {
  ci(env);
  const plan = read(stateFile(env));
  const bundle = env.FS_NATIVE_BUNDLE;
  const context = read(path.join(bundle, "context.json"));
  const out = env.FS_NATIVE_OUT;
  fs.mkdirSync(out, { recursive: true });
  const cargoEnv = { ...env, GITHUB_SHA: context.sourceSha, GITHUB_RUN_ID: context.runId, GITHUB_RUN_ATTEMPT: context.runAttempt };
  let passed = false;
  try {
    const consumer = runRegistryConsumer({ packDir: path.join(bundle, "packed"), name: "rightkit-fs", root: ROOT, reportFile: path.join(out, "registry-consumer.json"), env: cargoEnv });
    need(consumer.archiveSha256 === context.archiveSha256 && consumer.manifestSha256 === context.manifestSha256, "consumer archive binding differs");
    const archive = path.join(bundle, "packed", "rightkit-fs-0.4.6.crate");
    need(sha("sha256", fs.readFileSync(archive)) === context.archiveSha256, "source test archive hash changed");
    const tar = archiveTool(process.platform, env);
    const members = command(tar, ["-tzf", archive]).trim().split(/\r?\n/);
    need(members.every((name) => name.startsWith("rightkit-fs-0.4.6/") && !name.includes("\\") && !name.includes(":") && !name.split("/").some((part) => part === ".." || part === ".")), "unsafe crate members");
    need(command(tar, ["-tvzf", archive]).split(/\r?\n/).filter(Boolean).every((line) => ["-", "d"].includes(line[0])), "crate links or special entries refused");
    const extracted = path.join(plan.scratch, "source"); fs.mkdirSync(extracted);
    command(tar, ["-xzf", archive, "-C", extracted]);
    const cwd = path.join(extracted, "rightkit-fs-0.4.6");
    const child = childEnv(env, { RIGHTKIT_FS_TEST_OTHER_VOLUME: plan.other, CARGO_TARGET_DIR: path.join(plan.scratch, "target"), CARGO_INCREMENTAL: "1" });
    command("cargo", ["generate-lockfile"], { cwd, env: child, timeout: 10 * 60_000 });
    const stdout = command("cargo", ["test", "--locked", "--features", "transaction,test-seams", "--test", "transaction", "a_candidate_on_another_volume_is_copied_before_publication", "--", "--exact", "--nocapture"], { cwd, env: child, timeout: 30 * 60_000, logFile: path.join(out, "cross-volume-test.txt") });
    need(/1 passed; 0 failed/.test(stdout) && !stdout.includes("fixture unavailable"), "cross-volume test did not execute exactly once");
    passed = true;
  } finally {
    write(path.join(out, "qualification.json"), { schema: 1, kind: "fs-native-fixtures", ...context, qualificationRunId: env.GITHUB_RUN_ID, qualificationRunAttempt: env.GITHUB_RUN_ATTEMPT, harnessSha: env.GITHUB_SHA, host: process.platform, arch: process.arch, fixtures: plan, passed, fatCase: plan.fat ? "owner-only private publication must fail closed on FAT32" : "FAT32 coverage belongs to Windows; this host requires APFS separate-volume proof" });
    if (fs.existsSync(path.join(plan.scratch, "native-volume-info.txt"))) fs.copyFileSync(path.join(plan.scratch, "native-volume-info.txt"), path.join(out, "native-volume-info.json"));
  }
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const action = process.argv[2];
    need(["resolve", "setup", "qualify", "cleanup"].includes(action), "expected resolve, setup, qualify or cleanup");
    if (action === "resolve") await resolve(process.env);
    else ({ setup, qualify, cleanup })[action](process.env);
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
