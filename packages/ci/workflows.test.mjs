import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { validateManifest } from "./manifest.mjs";
import { renderLanes, planSync, applyPlan } from "./sync.mjs";
import { driftCommand } from "./drift.mjs";
import { makeTempRepo, PUBLIC_DEPS } from "./test-support.mjs";
import { swiftToolchainStep } from "./workflows.mjs";

const SWIFT_PIN = "a".repeat(64);
const RENDER = {
  kind: "custom",
  file: "render-windows.yml",
  on: { push: { branches: ["main"], paths: ["render/**", ".github/workflows/render-windows.yml"] }, workflow_dispatch: true },
  runsOn: "windows-2025",
  concurrency: { group: "render-windows", cancelInProgress: false },
  checkout: { sparse: ["render"], longPaths: true },
  timeoutMinutes: 45,
  toolchain: { rust: "1.91.0" },
  steps: [
    { name: "cargo test", shell: "pwsh", workingDirectory: "render", run: "cargo test --release" },
    { name: "Build info", shell: "pwsh", workingDirectory: "render", run: "cargo build --release\n$h = (Get-FileHash target/release/render.dll).Hash\nSet-Content build-info.txt \"sha=${{ github.sha }} dll=$h\"" },
  ],
  artifact: { name: "swiftright-render-{sha}", path: "render/target/release/render.dll", retentionDays: 30, ifNoFilesFound: "error" },
};
const SWIFT_HOST = {
  kind: "swift-host",
  file: "swift-host-tests.yml",
  os: ["windows-2025", "macos-15"],
  packages: ["packages/SwiftCore", "packages/SwiftUIKit"],
  swiftArgs: ["--parallel"],
  toolchain: { swift: { version: "6.4.0", sha256: SWIFT_PIN } },
  checkout: { sparse: ["packages"], longPaths: true },
  artifact: { name: "swift-test-logs-{os}", path: "packages/**/.build/logs", when: "always", ifNoFilesFound: "ignore" },
};
const FACTORY = {
  kind: "dispatch-matrix",
  file: "factory-pairs.yml",
  on: { workflow_dispatch: true },
  inputs: { cases: { description: "Case ids", default: "" }, cases_file: { description: "Case list file" }, runs: { description: "Runs per case", default: "1" } },
  plan: { shell: "bash", run: "node factory/tools/plan.mjs >> \"$GITHUB_OUTPUT\"" },
  jobs: [
    { id: "capture-mac", runsOn: "macos-15", steps: [{ shell: "bash", run: "./capture.sh \"${{ matrix.case }}\"" }], artifact: { name: "capture-mac-{matrix.case}", path: "out/**" } },
    { id: "capture-win", runsOn: "windows-2025", toolchain: { swift: { version: "6.4.0", sha256: SWIFT_PIN } }, steps: [{ shell: "pwsh", run: "./capture.ps1" }], artifact: { name: "capture-win-{matrix.case}", path: "out/**" } },
  ],
  collect: { download: { pattern: "capture-*", path: "captures" }, steps: [{ shell: "bash", run: "node factory/tools/compare.mjs captures" }] },
};
const BASE = {
  schemaVersion: 1,
  compile: "github-actions-only",
  lanes: ["ci"],
  matrix: { os: ["ubuntu-latest"], node: ["22.13"] },
  packageManager: "pnpm@11.25.0",
  templateVersion: "1.0.0",
};
const withWorkflows = (workflows) => ({ ...BASE, workflows });
const errorsFor = (workflows) => validateManifest(withWorkflows(workflows)).errors;
const renderByFile = (manifest) => Object.fromEntries(renderLanes(manifest, "/repos/swiftright").map((entry) => [entry.filename, entry.content]));

test("all three workflow kinds validate and render next to the ci lane", () => {
  const manifest = withWorkflows({ "render-windows": RENDER, "swift-host": SWIFT_HOST, "factory-pairs": FACTORY });
  assert.deepEqual(validateManifest(manifest).errors, []);
  const files = renderByFile(manifest);
  assert.deepEqual(Object.keys(files).sort(), ["ci.yml", "factory-pairs.yml", "render-windows.yml", "swift-host-tests.yml"]);
});

test("every rendered workflow pins actions to a commit sha, reads no secrets, has read-only permissions and no persisted credentials", () => {
  const files = renderByFile(withWorkflows({ "render-windows": RENDER, "swift-host": SWIFT_HOST, "factory-pairs": FACTORY }));
  for (const name of ["render-windows.yml", "swift-host-tests.yml", "factory-pairs.yml"]) {
    const text = files[name];
    assert.ok(text.startsWith("# Managed by right-git"), name);
    const uses = text.split("\n").filter((line) => /^\s*(?:- )?uses:/.test(line));
    assert.ok(uses.length > 0, name);
    for (const line of uses) assert.match(line, /@[0-9a-f]{40}(?:\s|$)/, `${name}: ${line.trim()}`);
    assert.equal((text.match(/persist-credentials: false/g) ?? []).length, uses.filter((line) => /actions\/checkout@/.test(line)).length, name);
    assert.match(text, /^permissions:\n  contents: read$/m, name);
    assert.doesNotMatch(text, /secrets\.|pull_request_target|workflow_run/, name);
    assert.match(text, /RightKit ownership/, `${name} keeps the ownership scan`);
  }
});

test("custom lane renders filename, path triggers, plain dispatch, steps, named artifact, per-lane sparse, concurrency and toolchain pin (#465)", () => {
  const text = renderByFile(withWorkflows({ "render-windows": RENDER }))["render-windows.yml"];
  assert.match(text, /^on:\n  push:\n    branches: \[main\]\n    paths:\n      - 'render\/\*\*'\n      - '\.github\/workflows\/render-windows\.yml'\n  workflow_dispatch:\npermissions:/m);
  assert.match(text, /^concurrency:\n  group: render-windows-\$\{\{ github\.ref \}\}\n  cancel-in-progress: false$/m);
  assert.match(text, /runs-on: windows-2025\n    timeout-minutes: 45/);
  assert.match(text, /uses: dtolnay\/rust-toolchain@[0-9a-f]{40}\n        with:\n          toolchain: 1\.91\.0/);
  assert.match(text, /- name: 'cargo test'\n        shell: pwsh\n        working-directory: render\n        run: \|\n          cargo test --release/);
  assert.match(text, /Set-Content build-info\.txt "sha=\$\{\{ github\.sha \}\} dll=\$h"/);
  assert.match(text, /sparse-checkout: \|\n            render\n          sparse-checkout-cone-mode: true/);
  assert.match(text, /- name: Enable Windows long paths/);
  assert.match(text, /name: swiftright-render-\$\{\{ github\.sha \}\}\n          path: 'render\/target\/release\/render\.dll'\n          retention-days: 30\n          if-no-files-found: error/);
  assert.doesNotMatch(text, /toolchain: stable/);
});

test("swift-host lane runs swift test per package on windows and macOS with the pinned toolchain step (#437, #436)", () => {
  const text = renderByFile(withWorkflows({ "swift-host": SWIFT_HOST }))["swift-host-tests.yml"];
  assert.match(text, /matrix:\n        os: \["windows-2025","macos-15"\]\n    runs-on: \$\{\{ matrix\.os \}\}/);
  assert.match(text, /- name: Install pinned Swift toolchain\n        if: runner\.os == 'Windows'\n        shell: bash\n        run: pnpm exec right-release toolchain swift --version 6\.4\.0 --sha256 a{64} --github-env/);
  assert.match(text, /- name: 'swift test packages\/SwiftCore'\n        shell: bash\n        working-directory: packages\/SwiftCore\n        run: \|\n          swift test --parallel/);
  assert.match(text, /- name: 'swift test packages\/SwiftUIKit'/);
  assert.match(text, /name: swift-test-logs-\$\{\{ matrix\.os \|\| runner\.os \}\}/);
  assert.match(text, /if: \$\{\{ always\(\) \}\}\n        uses: actions\/upload-artifact@[0-9a-f]{40}/);
  assert.match(text, /on:\n  push:\n    branches: \[main\]\n  pull_request:\npermissions:/);
});

test("swiftToolchainStep calls right-release toolchain swift through the manifest's package manager", () => {
  const step = swiftToolchainStep({ packageManager: "npm@10.9.0" }, { version: "6.4.0", sha256: SWIFT_PIN }).join("\n");
  assert.match(step, /run: npm exec -- right-release toolchain swift --version 6\.4\.0 --sha256 a{64} --github-env/);
  assert.match(step, /if: runner\.os == 'Windows'/);
});

test("dispatch-matrix lane renders inputs, a plan job, per-item matrix jobs and a collect job (#437)", () => {
  const text = renderByFile(withWorkflows({ "factory-pairs": FACTORY }))["factory-pairs.yml"];
  assert.match(text, /workflow_dispatch:\n    inputs:\n      cases:\n        description: 'Case ids'\n        type: string\n        required: false\n        default: ''/);
  assert.match(text, /  plan:\n    needs: \[private-repo-guard\]\n    runs-on: ubuntu-24\.04/);
  assert.match(text, /outputs:\n      matrix: \$\{\{ steps\.plan\.outputs\.matrix \}\}/);
  assert.match(text, /RIGHT_GIT_INPUT_CASES_FILE: \$\{\{ inputs\.cases_file \}\}/);
  assert.match(text, /  capture-mac:\n    needs: \[private-repo-guard, plan\]\n    strategy:\n      fail-fast: false\n      matrix: \$\{\{ fromJSON\(needs\.plan\.outputs\.matrix\) \}\}\n    runs-on: macos-15/);
  assert.match(text, /name: capture-mac-\$\{\{ matrix\.case \}\}/);
  assert.match(text, /  collect:\n    needs: \[private-repo-guard, capture-mac, capture-win\]\n    if: \$\{\{ !cancelled\(\) && needs\.private-repo-guard\.result == 'success' \}\}/);
  assert.match(text, /uses: actions\/download-artifact@[0-9a-f]{40} # v4\.3\.0\n        with:\n          pattern: 'capture-\*'\n          path: 'captures'\n          merge-multiple: true/);
  assert.match(text, /- name: RightKit ownership[^\n]*\n        if: \$\{\{ runner\.os != 'Linux' \}\}/);
});

test("workflow validation rejects unsafe values, secrets, untrusted expressions, collisions and unknown fields", () => {
  const bad = (patch, pattern) => {
    const errors = errorsFor({ "render-windows": { ...RENDER, ...patch } });
    assert.ok(errors.some((error) => pattern.test(error)), `${JSON.stringify(patch)} -> ${errors.join(" | ")}`);
  };
  bad({ file: "ci.yml" }, /collides with a built-in lane/);
  bad({ file: "../x.yml" }, /plain workflow filename/);
  bad({ on: { pull_request_target: true } }, /unknown render-windows\.on field|unknown workflows\.render-windows\.on field/);
  bad({ on: { push: { paths: ["../x"] } } }, /paths must be/);
  bad({ on: {} }, /at least one trigger/);
  bad({ steps: [{ shell: "bash", run: "echo ${{ secrets.TOKEN }}" }] }, /must not reference secrets/);
  bad({ steps: [{ shell: "bash", run: "echo ${{ github.head_ref }}" }] }, /untrusted event data/);
  bad({ steps: [{ shell: "bash", run: "echo ${{ github.event.pull_request.title }}" }] }, /untrusted event data/);
  bad({ steps: [{ shell: "zsh", run: "echo" }] }, /shell must be bash or pwsh/);
  bad({ steps: [{ shell: "bash", run: "  indented" }] }, /must not start with indentation/);
  bad({ steps: [] }, /non-empty array/);
  bad({ steps: [{ shell: "bash", run: "x", workingDirectory: "../up" }] }, /workingDirectory/);
  bad({ runsOn: "${{ matrix.os }}" }, /safe runner label/);
  bad({ concurrency: { group: "a b" } }, /concurrency must be/);
  bad({ checkout: { sparse: ["../x"] } }, /checkout\.sparse/);
  bad({ toolchain: { rust: "latest" } }, /toolchain\.rust/);
  bad({ toolchain: { swift: { version: "6.4.0", sha256: "abc" } } }, /toolchain\.swift/);
  bad({ artifact: { name: "x${{ secrets.A }}", path: "out" } }, /artifact\.name may contain only/);
  bad({ artifact: { name: "x", path: "../out" } }, /artifact\.path/);
  bad({ artifact: { name: "x", path: "out", retentionDays: 400 } }, /retentionDays/);
  bad({ artifact: { name: "x", path: "out", ifNoFilesFound: "skip" } }, /ifNoFilesFound/);
  bad({ timeoutMinutes: 0 }, /timeoutMinutes/);
  bad({ bogus: true }, /unknown workflows\.render-windows field: bogus/);
  assert.ok(errorsFor({ a: { ...RENDER }, b: { ...RENDER } }).some((error) => /is also used by workflows\.a/.test(error)));
  assert.ok(errorsFor({ "Bad Name": RENDER }).some((error) => /workflow names are lowercase/.test(error)));
  assert.ok(errorsFor({ x: { kind: "nope" } }).some((error) => /kind must be one of/.test(error)));
  assert.ok(errorsFor({}).some((error) => /1 to 8 named workflows/.test(error)));
});

test("matrix workflows need an artifact name that differs per job; windows swift-host needs the toolchain pin", () => {
  const multi = { ...RENDER, runsOn: ["windows-2025", "macos-15"], artifact: { name: "same", path: "out" } };
  assert.ok(errorsFor({ "render-windows": multi }).some((error) => /differs per job/.test(error)));
  assert.deepEqual(errorsFor({ "render-windows": { ...multi, artifact: { name: "out-{os}", path: "out" } } }), []);
  const noSwift = { ...SWIFT_HOST, toolchain: undefined };
  assert.ok(errorsFor({ "swift-host": noSwift }).some((error) => /toolchain\.swift is required/.test(error)));
  assert.deepEqual(errorsFor({ "swift-host": { ...noSwift, os: ["macos-15"] } }), []);
  const jobNoToken = { ...FACTORY, jobs: [{ ...FACTORY.jobs[0], artifact: { name: "fixed", path: "out" } }] };
  assert.ok(errorsFor({ "factory-pairs": jobNoToken }).some((error) => /differs per job/.test(error)));
  assert.ok(errorsFor({ "factory-pairs": { ...FACTORY, on: { push: true } } }).some((error) => /workflow_dispatch must be true/.test(error)));
  assert.ok(errorsFor({ "factory-pairs": { ...FACTORY, jobs: [{ ...FACTORY.jobs[0], id: "plan" }] } }).some((error) => /job id other than plan/.test(error)));
});

test("sync writes the workflows, drift matches them and a hand edit is detected", async () => {
  const repo = makeTempRepo(withWorkflows({ "render-windows": RENDER, "swift-host": SWIFT_HOST }));
  const plan = await planSync(repo, PUBLIC_DEPS);
  assert.equal(plan.ok, true, JSON.stringify(plan.errors));
  assert.deepEqual(plan.changes.map((change) => change.filename).sort(), ["ci.yml", "render-windows.yml", "swift-host-tests.yml"]);
  assert.equal(applyPlan(repo, plan).ok, true);
  const clean = await driftCommand(repo, PUBLIC_DEPS);
  assert.equal(clean.driftDetected, false);
  fs.appendFileSync(path.join(repo, ".github", "workflows", "render-windows.yml"), "# edit\n");
  const edited = await driftCommand(repo, PUBLIC_DEPS);
  assert.ok(edited.findings.some((finding) => finding.filename === "render-windows.yml" && finding.status === "mismatch"));
});

test("removing a workflow from the manifest removes its managed file on the next sync", async () => {
  const repo = makeTempRepo(withWorkflows({ "render-windows": RENDER }));
  assert.equal(applyPlan(repo, await planSync(repo, PUBLIC_DEPS)).ok, true);
  fs.writeFileSync(path.join(repo, ".rightgit.json"), `${JSON.stringify(BASE)}\n`);
  const plan = await planSync(repo, PUBLIC_DEPS);
  assert.ok(plan.changes.some((change) => change.filename === "render-windows.yml" && change.action === "remove"));
});

test("run scripts allow only static expression forms; everything else must use a quoted env variable", () => {
  const stepErrors = (run) => errorsFor({ "render-windows": { ...RENDER, steps: [{ shell: "bash", run }] } });
  for (const run of ["echo \"${{ github['event']['pull_request']['title'] }}\"", "echo ${{ inputs.cases }}", "echo ${{ github.token }}", "echo ${{ env.X }}", "echo ${{ github.sha"]) {
    assert.ok(stepErrors(run).some((error) => /pass any other value through a quoted env: variable/.test(error)), run);
  }
  for (const run of ["echo ${{ github.sha }} ${{ runner.os }} ${{ matrix.case }} ${{ github.run_id }} ${{ github.run_attempt }} ${{ 'lit-eral' }}", "echo \"$RIGHT_GIT_INPUT_CASES\""]) {
    assert.deepEqual(stepErrors(run), [], run);
  }
  const files = renderByFile(withWorkflows({ "factory-pairs": FACTORY }));
  assert.match(files["factory-pairs.yml"], /RIGHT_GIT_INPUT_CASES: \$\{\{ inputs\.cases \}\}/);
});

test("step names are quoted scalars and swift-host os must be an array", () => {
  const manifest = withWorkflows({ "render-windows": { ...RENDER, steps: [{ name: "Build: Windows", shell: "bash", run: "true" }] } });
  assert.deepEqual(validateManifest(manifest).errors, []);
  assert.match(renderByFile(manifest)["render-windows.yml"], /- name: 'Build: Windows'/);
  const errors = errorsFor({ "swift-host": { ...SWIFT_HOST, os: "macos-15" } });
  assert.ok(errors.some((error) => /swift-host\.os must be an array/.test(error)), errors.join(" | "));
});
