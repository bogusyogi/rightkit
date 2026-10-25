import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";
import { renderTemplate, MANAGED_MARKER } from "./render.mjs";
import { buildRenderContext, LANE_FILENAMES, KNOWN_LANES, CHECK_JOB_LANES, checkJob, parsePackageManager, validateManifest, swiftPrivatePackagesConfigureStep, swiftPrivatePackagesCleanupStep } from "./manifest.mjs";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)));
const templatesDir = path.join(packageRoot, "templates");

const FULL_MANIFEST = {
  schemaVersion: 1,
  compile: "github-actions-only",
  lanes: [...KNOWN_LANES],
  matrix: { os: ["ubuntu-latest", "macos-latest", "windows-latest"], node: ["22.13", "24"] },
  packageManager: "pnpm@11.25.0",
  publish: {
    npm: { oidc: true, provenance: true },
    homebrew: { tap: "acme/homebrew-tap", formulaName: "widgets" },
    winget: { packageIdentifier: "Acme.Widgets" },
  },
  regenerate: { script: "regenerate" },
  templateVersion: "1.0.0",
};

function renderAllLanes(manifest, repoPath) {
  const rendered = {};
  for (const lane of manifest.lanes) {
    const templatePath = path.join(templatesDir, LANE_FILENAMES[lane]);
    const templateText = fs.readFileSync(templatePath, "utf8");
    const configured = lane === "swift-check" ? { ...manifest, swift: { ...manifest.swift, check: { runner: "macos-15", xcodeVersion: "16.4", projectDirectory: "ios", spec: "project.yml", project: "Widgets.xcodeproj", scheme: "Widgets", xcodegenVersion: "2.44.1" } } } : manifest;
    rendered[lane] = renderTemplate(templateText, buildRenderContext(configured, repoPath, lane));
  }
  return rendered;
}

// A render as it was before the RightKit check job (rightkit#833): the job removed, and `check` removed from every `needs` list.
// The hash-pinned renders below are kept against this view of the new render, so the job is provably the whole change.
function withoutCheckJob(rendered) {
  return rendered
    .replace(/^  check:\n(?: {4,}.*\n)+/m, "")
    .replace(/^(    needs: \[[^\]\n]*?), check\b/gm, "$1");
}

test("every known lane has a template file and renders without an unresolved placeholder", () => {
  for (const lane of KNOWN_LANES) {
    const templatePath = path.join(templatesDir, LANE_FILENAMES[lane]);
    assert.ok(fs.existsSync(templatePath), `missing template for lane ${lane}`);
  }
  const rendered = renderAllLanes(FULL_MANIFEST, "/repos/widgets");
  assert.equal(Object.keys(rendered).length, KNOWN_LANES.length);
});

test("rust-hybrid ci renders its pinned package manager and gate", () => {
  const manifest = { ...FULL_MANIFEST, lanes: ["ci"], profile: "rust-hybrid", gate: { path: "scripts/gate.sh" }, templateVersion: "1.1.0" };
  const content = renderTemplate(fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/ci.yml"), "utf8"), buildRenderContext(manifest, "/repos/widgets"));
  assert.match(content, /uses: pnpm\/action-setup@[a-f0-9]{40}/);
  assert.match(content, /fetch-depth: 0/);
  assert.match(content, /^name: widgets-ci$/m);
  assert.match(content, /version: 11\.25\.0/);
  assert.match(content, /components: rustfmt, clippy/);
  assert.doesNotMatch(content, /toolchain:/);
  assert.match(content, /group: \$\{\{ github\.workflow \}\}-\$\{\{ github\.event_name == 'push'[^\n]*github\.run_id \|\| github\.ref \}\}/);
  assert.match(content, /cancel-in-progress: \$\{\{ !\(github\.event_name == 'push' && github\.ref_name == github\.event\.repository\.default_branch\) \}\}/);
  assert.match(content, /pnpm exec rightkit-gate --to compile/);
  assert.doesNotMatch(content, /bash \.\/scripts\/gate\.sh/, "the ci job runs the one door, never the repository's script");
  assert.match(content, /name: Detect Rust-affecting changes\n        id: rust-changes/);
  assert.match(content, /RIGHT_GIT_DIFF_BASE: \$\{\{ github\.event_name == 'pull_request' && github\.event\.pull_request\.base\.sha \|\| '' \}\}/);
  assert.doesNotMatch(content, /github\.event\.before/, "a push never diffs against the previous push: a failed run would be skipped over");
  assert.match(content, /actions\/cache\/restore@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6\.1\.0[\s\S]*restore-keys: \|\n            widgets-last-green-\$\{\{ matrix\.os \}\}-\$\{\{ matrix\.node \}\}-\$\{\{ github\.ref_name \}\}-\n      - name: Detect Rust-affecting changes/);
  assert.match(content, /GITHUB_EVENT_NAME" != "pull_request"[\s\S]*base=""[\s\S]*\.right-git-last-green\/sha[\s\S]*merge-base --is-ancestor[\s\S]*git cat-file -e/, "no marker or a non-ancestor marker leaves changed=true");
  assert.match(content, /pnpm exec rightkit-gate --to compile[\s\S]*name: Record last green commit marker\n        if: github\.event_name != 'pull_request'[\s\S]*actions\/cache\/save@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6\.1\.0[\s\S]*github\.run_attempt \}\}\n/, "the marker is written only after the gate passed, as the job's last steps");
  assert.match(content, /changed=true[\s\S]*\^0\{40\}\$[\s\S]*git cat-file -e[\s\S]*git diff --name-only --diff-filter=ACDMRTUXB/);
  assert.match(content, /--diff-filter=ACDMRTUXB/, "deleted Rust inputs must trigger Rust setup");
  assert.match(content, /\*\.rs\|Cargo\.toml[\s\S]*scripts\/gate\.sh/);
  assert.doesNotMatch(content, /\|build\.rs\||\|\*\/build\.rs\|/);
  assert.match(content, /\.rightgit\.json\|\.github\/workflows\/\*\.yml\|\.github\/workflows\/\*\.yaml/);
  assert.equal((content.match(/if: steps\.rust-changes\.outputs\.required == 'true'/g) ?? []).length, 3);
  assert.match(content, /RIGHT_GIT_RUST_CHANGED: \$\{\{ steps\.rust-changes\.outputs\.changed \}\}/);
  assert.match(content, /RIGHT_GIT_RUST_REQUIRED: \$\{\{ steps\.rust-changes\.outputs\.required \}\}/);
  assert.match(content, /RUSTC_WRAPPER: \$\{\{ steps\.rust-changes\.outputs\.required == 'true' && 'sccache' \|\| '' \}\}/);
  assert.match(content, /CARGO_INCREMENTAL: "0"/);
  assert.match(content, /CARGO_PROFILE_DEV_DEBUG: "0"/);
  assert.match(content, /CARGO_PROFILE_TEST_DEBUG: "0"/);
  assert.doesNotMatch(content, /opt-level|codegen-units|\blto\b|strip\s*=/i);
  assert.match(content, /os: \["windows-latest"\]/);
  assert.doesNotMatch(content, /ubuntu-latest|macos-latest/);
});

test("every generated workflow profile runs ownership scanning independent of optional repository roots", () => {
  const roots = ["engine", "apps/client"];
  const manifest = { ...FULL_MANIFEST, ownership: { roots } };
  const rendered = renderAllLanes(manifest, "C:/repos/coderight");
  for (const [lane, content] of Object.entries(rendered)) {
    assert.match(content, /right-release ownership --app \./, lane);
    assert.match(content, /protected registry roots/, lane);
  }
  const rust = renderTemplate(fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/ci.yml"), "utf8"), buildRenderContext({ ...manifest, profile: "rust-hybrid", gate: { path: "scripts/gate.sh" } }, "C:/repos/coderight"));
  assert.match(rust, /RightKit ownership \(protected registry roots\)/);
  assert.match(rust, /pnpm exec right-release ownership --app \./);
  const withoutRoots = renderAllLanes(FULL_MANIFEST, "C:/repos/widgets");
  for (const [lane, content] of Object.entries(withoutRoots)) assert.match(content, /right-release ownership --app \./, lane);
});

test("ordinary CI templates cancel superseded pull-request runs on the same ref", () => {
  const generic = renderAllLanes(FULL_MANIFEST, "/repos/widgets").ci;
  assert.match(generic, /group: \$\{\{ github\.workflow \}\}-\$\{\{ github\.event_name == 'push'[^\n]*github\.run_id \|\| github\.ref \}\}/);
  assert.match(generic, /cancel-in-progress: \$\{\{ !\(github\.event_name == 'push' && github\.ref_name == github\.event\.repository\.default_branch\) \}\}/);
});

test("ownership revision expression matches each lane trigger contract", () => {
  const rendered = renderAllLanes(FULL_MANIFEST, "/repos/widgets");
  assert.match(rendered.ci, /git rev-parse HEAD\)" = "\$\{\{ github\.sha \}\}/);
  assert.doesNotMatch(rendered.ci, /inputs\.source_revision/);
  assert.match(rendered["publish-package-managers"], /git rev-parse HEAD\)" = "\$\{\{ github\.sha \}\}/);
  assert.doesNotMatch(rendered["publish-package-managers"], /inputs\.source_revision/);
  assert.match(rendered["release-candidate"], /git rev-parse HEAD\)" = "\$\{\{ inputs\.source_revision \|\| github\.sha \}\}/);
});

test("rust-hybrid ci preserves a declared macOS-only lane", () => {
  const manifest = {
    ...FULL_MANIFEST,
    lanes: ["ci"],
    profile: "rust-hybrid",
    gate: { path: "scripts/gate.sh" },
    matrix: { os: ["macos-14"], node: ["26.8.1"] },
  };
  const content = renderTemplate(
    fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/ci.yml"), "utf8"),
    buildRenderContext(manifest, "/repos/widgets"),
  );
  assert.match(content, /os: \["macos-14"\]/);
  assert.doesNotMatch(content, /windows-2025/);
});

test("rust-hybrid ci runs every matrix.os runner when ciAllOs is true, in declared order", () => {
  const base = {
    ...FULL_MANIFEST,
    lanes: ["ci"],
    profile: "rust-hybrid",
    gate: { path: "scripts/gate.sh" },
    qaEvidencePath: "pulse-hub-qa",
    matrix: { os: ["xcode-27", "windows-2025"], node: ["26.8.1"] },
  };
  const render = (manifest) => renderTemplate(
    fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/ci.yml"), "utf8"),
    buildRenderContext(manifest, "/repos/pulse"),
  );
  const all = render({ ...base, ciAllOs: true });
  assert.match(all, /^ {8}os: \["xcode-27", "windows-2025"\]$/m);
  assert.match(all, /runs-on: \$\{\{ matrix\.os \}\}/);
  assert.match(all, /fail-fast: false/);
  assert.match(all, /name: qa-evidence-\$\{\{ matrix\.os \|\| runner\.os \}\}-\$\{\{ github\.run_attempt \}\}/);
  assert.doesNotMatch(all, /Full validation stays/);
  // ciAllOs false and unset render the same single-runner matrix as before.
  for (const manifest of [base, { ...base, ciAllOs: false }]) {
    const single = render(manifest);
    assert.match(single, /# Full validation stays on one declared runner; prefer configured\n {8}# Windows when available, otherwise preserve first configured OS\.\n {8}os: \["windows-2025"\]\n/);
    assert.doesNotMatch(single, /xcode-27/);
  }
  // Unset and false produce byte-identical output.
  assert.equal(render({ ...base, ciAllOs: false }), render(base));
});

test("rust-hybrid ci skips the ownership step on a Linux ciAllOs leg and is otherwise unchanged", () => {
  const base = {
    ...FULL_MANIFEST,
    lanes: ["ci"],
    profile: "rust-hybrid",
    gate: { path: "scripts/gate.sh" },
    matrix: { os: ["macos-15", "windows-2025", "ubuntu-24.04"], node: ["26.8.1"] },
  };
  const render = (manifest) => renderTemplate(
    fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/ci.yml"), "utf8"),
    buildRenderContext(manifest, "/repos/ember"),
  );
  const guarded = "      - name: RightKit ownership (protected registry roots)\n        if: ${{ runner.os != 'Linux' }}\n        shell: bash\n";
  const all = render({ ...base, ciAllOs: true });
  assert.match(all, /^ {8}os: \["macos-15", "windows-2025", "ubuntu-24\.04"\]$/m);
  assert.ok(all.includes(guarded));
  // Not ciAllOs: single runner, step unguarded; the Linux guard never appears.
  for (const manifest of [base, { ...base, ciAllOs: false }]) {
    const single = render(manifest);
    assert.doesNotMatch(single, /runner\.os != 'Linux'/);
    assert.match(single, /RightKit ownership \(protected registry roots\)\n        shell: bash\n/);
  }
  // ciAllOs without a Linux runner renders byte-identically to before.
  const noLinux = { ...base, matrix: { os: ["macos-15", "windows-2025"], node: ["26.8.1"] }, ciAllOs: true };
  assert.doesNotMatch(render(noLinux), /runner\.os != 'Linux'/);
  assert.match(render(noLinux), /RightKit ownership \(protected registry roots\)\n        shell: bash\n/);
  // The guard is the only difference the Linux runner makes to the ownership step.
  assert.equal(all.split(guarded).length, 2);
});

test("rust-hybrid release candidate renders product scripts, artifact handoff, and no signing secret", () => {
  const manifest = {
    ...FULL_MANIFEST,
    lanes: ["ci", "release-candidate"],
    profile: "rust-hybrid",
    gate: { path: "scripts/gate.sh" },
    candidate: {
      buildScript: "ci:candidate",
      checkScript: "ci:candidate-check",
      admissionScript: "release:admit",
      stageSummaryScript: "release:stage-summary",
      artifactRoot: "widgets-unsigned",
      os: ["macos-latest", "windows-latest"],
    },
    templateVersion: "1.2.0",
  };
  const content = renderTemplate(
    fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/release-candidate.yml"), "utf8"),
    buildRenderContext(manifest, "/repos/widgets"),
  );
  assert.match(content, /pnpm run ci:candidate/);
  assert.match(content, /pnpm run ci:candidate-check/);
  assert.match(content, /pnpm run release:admit/);
  assert.match(content, /pnpm run release:stage-summary/);
  // Ownership approval supports only macOS and Windows: the admission job (which runs the ownership step) never renders on Linux.
  const admission = content.match(/^ {2}admission:\n([\s\S]*?)^ {2}\S/m)?.[1] ?? "";
  assert.match(admission, /RightKit ownership/);
  assert.match(admission, /runs-on: macos-15\n/);
  assert.doesNotMatch(admission, /runs-on: ubuntu|runs-on: \$\{\{[^}]*linux/i);
  assert.match(content, /Mozilla-Actions\/sccache-action@fc920bf0ec8de6ee65d409111f7ec508035751ba/);
  assert.match(content, /shared-key: widgets-rust/);
  assert.doesNotMatch(content, /bash \.\/scripts\/gate\.sh/);
  assert.match(content, /RIGHT_GIT_ARTIFACT_ROOT/);
  assert.match(content, /os: "macos-latest"/);
  assert.match(content, /platform: "windows"/);
  assert.match(content, /RIGHT_GIT_RELEASE_ARCHITECTURE: \$\{\{ matrix\.architecture \}\}/);
  assert.match(content, /actions\/upload-artifact@[a-f0-9]{40}/);
  assert.doesNotMatch(content, /secrets\.|AZURE_|APPLE_|certificate|private.key/i);
  assert.doesNotMatch(content, /rust-changes|RIGHT_GIT_RUST_CHANGED/);
  assert.doesNotMatch(content, /if: steps\.rust-changes/);
  assert.match(content, /uses: dtolnay\/rust-toolchain@[a-f0-9]{40}\n        with:/);
  // workflow_dispatch tag-last envelope: no windows-sign/macos-sign/release
  // chain jobs render at all when signing is not configured.
  assert.match(content, /^on:\n  workflow_dispatch:/m);
  assert.doesNotMatch(content, /^ {2}(?:windows-sign|macos-sign|installed-qualification|publish):\s*$/m);
  assert.doesNotMatch(content, /run: (?:pnpm|npm|yarn) run undefined\b/);
  assert.doesNotMatch(content, /\$\{\{\s*github\.output\s*\}\}/);
});

test("rust-hybrid cache contract renders every declared Cargo workspace", () => {
  const manifest = {
    ...FULL_MANIFEST,
    lanes: ["ci"],
    profile: "rust-hybrid",
    gate: { path: "scripts/gate.sh" },
    rustCache: { workspaces: [".", "apps/studio/src-tauri"], requiredForGate: true },
    matrix: { os: ["windows-2025"], node: ["22.23.2"] },
  };
  const content = renderTemplate(
    fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/ci.yml"), "utf8"),
    buildRenderContext(manifest, "/repos/widgets"),
  );
  assert.match(content, /workspaces: \|\n            \. -> target\n            apps\/studio\/src-tauri -> target/);
  assert.match(content, /add-job-id-key: false/);
  assert.match(content, /cache-workspace-crates: false/);
  assert.match(content, /cache-on-failure: true/);
  assert.match(content, /if \[\[ "true" == "true" \]\]; then[\s\S]*required=true/);
  assert.match(content, /RUSTC_WRAPPER: \$\{\{ steps\.rust-changes\.outputs\.required == 'true' && 'sccache' \|\| '' \}\}/);
});

test("release-candidate artifact names all carry the admission artifact_suffix so dry-run artifacts cannot impersonate real ones", () => {
  const manifest = {
    ...FULL_MANIFEST,
    lanes: ["ci", "release-candidate"],
    profile: "rust-hybrid",
    gate: { path: "scripts/gate.sh" },
    candidate: {
      buildScript: "ci:candidate",
      checkScript: "ci:candidate-check",
      admissionScript: "release:admit",
      stageSummaryScript: "release:stage-summary",
      artifactRoot: "widgets-unsigned",
      os: ["windows-2025", "macos-15"],
      targets: [
        { os: "windows-2025", platform: "windows", architecture: "x86_64" },
        { os: "macos-15", platform: "macos", architecture: "arm64" },
      ],
      signWindows: true,
      signMacos: true,
      releaseChain: {
        evidenceVerificationScript: "release:verify-evidence",
        windowsFinalizeScript: "release:finalize-windows",
        macosFinalizeScript: "release:finalize-macos",
        installedQualificationScript: "release:qualify-installed",
        publishScript: "release:publish-qualified",
        qualificationRunnerVariable: "WIDGETS_QUALIFICATION_RUNNER",
      },
    },
    matrix: { os: ["ubuntu-24.04", "windows-2025", "macos-15"], node: ["22.23.2"] },
    templateVersion: "1.8.0",
  };
  const content = renderTemplate(
    fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/release-candidate.yml"), "utf8"),
    buildRenderContext(manifest, "/repos/widgets", "release-candidate"),
  );
  const identityNames = content.split("\n").filter((line) => /^\s*name: .*needs\.admission\.outputs\.source_revision/.test(line));
  assert.ok(identityNames.length >= 8, `expected identity-bound artifact names, got ${identityNames.length}`);
  for (const line of identityNames) {
    assert.ok(line.endsWith("${{ needs.admission.outputs.source_revision }}${{ needs.admission.outputs.artifact_suffix }}"), line.trim());
  }
  assert.match(content, /artifact_suffix: \$\{\{ steps\.admit\.outputs\.artifact_suffix \}\}/);
  // The publish gate text is pinned by @rightkit/release's hosted-workflow policy; dry runs are refused by admission, not by this `if`.
  assert.match(content, /publish:\n    needs: \[[^\n]*\]\n    if: \$\{\{ needs\.admission\.outputs\.publish == 'true' && needs\.installed-qualification\.result == 'success' && needs\.macos-sign\.result == 'success' \}\}/);
});

test("rust-hybrid release candidate can opt into OIDC Windows signing without long-lived secrets", () => {
  const manifest = {
    ...FULL_MANIFEST,
    lanes: ["ci", "release-candidate"],
    profile: "rust-hybrid",
    gate: { path: "scripts/gate.sh" },
    candidate: {
      buildScript: "ci:candidate",
      checkScript: "ci:candidate-check",
      admissionScript: "release:admit",
      stageSummaryScript: "release:stage-summary",
      artifactRoot: "widgets-unsigned",
      os: ["windows-2025"],
      signWindows: true,
    },
    matrix: { os: ["ubuntu-24.04", "windows-2025"], node: ["22.23.2"] },
    templateVersion: "1.3.0",
  };
  const content = renderTemplate(
    fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/release-candidate.yml"), "utf8"),
    buildRenderContext(manifest, "/repos/widgets", "release-candidate"),
  );
  assert.match(content, /environment: release/);
  // Tag-last contract: signing is gated on the admitted, same-run
  // signed_qualification OUTPUT of the `admission` job — never on a ref/tag
  // and never on the raw workflow_dispatch input directly.
  assert.match(content, /windows-sign:\n    needs: \[admission, candidate\]\n    if: \$\{\{ needs\.admission\.outputs\.signed_qualification == 'true' \}\}/);
  assert.doesNotMatch(content, /startsWith\(github\.ref, 'refs\/tags\/v'\)/);
  assert.match(content, /id-token: write/);
  assert.match(content, /azure\/login@[a-f0-9]{40}/);
  assert.match(content, /vars\.AZURE_CLIENT_ID/);
  assert.match(content, /right-release build --platform win --skip-checks/);
  assert.match(content, /Artifact Signing client hash mismatch/);
  // #183: the signing step reads a source QA receipt carried in the downloaded candidate handoff.
  assert.ok(content.includes("RIGHT_RELEASE_QA_RECEIPT: ${{ runner.temp }}/unsigned-windows-candidate/qa"));
  // Every downstream job (admission, candidate, and windows-sign here) checks
  // out the exact admitted revision by SHA, never by resolving HEAD after a
  // ref-triggered checkout.
  assert.match(content, /windows-sign:[\s\S]*?ref: \$\{\{ needs\.admission\.outputs\.source_revision \}\}\n\s+fetch-depth: 0\n\s+persist-credentials: false/);
  assert.match(content, /git rev-parse HEAD.*inputs\.source_revision/);
  assert.ok(content.includes("LEGION_SOURCE_REVISION: ${{ needs.admission.outputs.source_revision }}"));
  assert.ok(content.includes("RIGHT_GIT_SOURCE_REVISION: ${{ needs.admission.outputs.source_revision }}"));
  assert.doesNotMatch(content, /env\.RIGHT_GIT_SOURCE_REVISION/);
  assert.doesNotMatch(content, /LEGION_SOURCE_REVISION: \$\{\{ github\.sha \}\}/);
  assert.doesNotMatch(content, /secrets\.|client-secret|private.key/i);
  assert.doesNotMatch(content, /run: (?:pnpm|npm|yarn) run undefined\b/);
  assert.doesNotMatch(content, /\$\{\{\s*github\.output\s*\}\}/);
});

test("rust-hybrid release candidate can opt into protected macOS signing & notarization", () => {
  const manifest = {
    ...FULL_MANIFEST,
    lanes: ["ci", "release-candidate"],
    profile: "rust-hybrid",
    gate: { path: "scripts/gate.sh" },
    candidate: {
      buildScript: "ci:candidate",
      checkScript: "ci:candidate-check",
      admissionScript: "release:admit",
      stageSummaryScript: "release:stage-summary",
      artifactRoot: "widgets-unsigned",
      os: ["macos-15"],
      signMacos: true,
    },
    matrix: { os: ["ubuntu-24.04", "macos-15"], node: ["22.23.2"] },
    templateVersion: "1.3.0",
  };
  const content = renderTemplate(
    fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/release-candidate.yml"), "utf8"),
    buildRenderContext(manifest, "/repos/widgets", "release-candidate"),
  );
  assert.match(content, /macos-sign:/);
  assert.match(content, /macos-sign:\n    needs: \[admission, candidate\]\n    if: \$\{\{ needs\.admission\.outputs\.signed_qualification == 'true' \}\}/);
  assert.doesNotMatch(content, /startsWith\(github\.ref, 'refs\/tags\/v'\)/);
  assert.match(content, /environment: release/);
  assert.match(content, /secrets\.APPLE_CERTIFICATE_BASE64/);
  assert.match(content, /temporary keychain/i);
  assert.match(content, /right-release build --platform mac --skip-checks/);
  assert.match(content, /secrets\.APPLE_API_KEY_BASE64/);
  assert.ok(content.includes("RIGHT_RELEASE_QA_RECEIPT: ${{ runner.temp }}/unsigned-macos-candidate/qa"));
  assert.match(content, /macos-sign:[\s\S]*?ref: \$\{\{ needs\.admission\.outputs\.source_revision \}\}\n\s+fetch-depth: 0\n\s+persist-credentials: false/);
  assert.match(content, /git rev-parse HEAD.*inputs\.source_revision/);
  assert.ok(content.includes("LEGION_SOURCE_REVISION: ${{ needs.admission.outputs.source_revision }}"));
  assert.ok(content.includes("RIGHT_GIT_SOURCE_REVISION: ${{ needs.admission.outputs.source_revision }}"));
  assert.doesNotMatch(content, /env\.RIGHT_GIT_SOURCE_REVISION/);
  assert.doesNotMatch(content, /LEGION_SOURCE_REVISION: \$\{\{ github\.sha \}\}/);
  assert.match(content, /security delete-keychain/);
  assert.doesNotMatch(content, /pull_request_target/);
  assert.doesNotMatch(content, /run: (?:pnpm|npm|yarn) run undefined\b/);
  assert.doesNotMatch(content, /\$\{\{\s*github\.output\s*\}\}/);
});

test("rust-hybrid multi-architecture candidates bind every native target to exact artifacts and signing", () => {
  const manifest = {
    ...FULL_MANIFEST,
    lanes: ["ci", "release-candidate"],
    profile: "rust-hybrid",
    gate: { path: "scripts/gate.sh" },
    matrix: { os: ["ubuntu-24.04"], node: ["22.23.2"] },
    candidate: {
      buildScript: "ci:candidate",
      checkScript: "ci:candidate-check",
      admissionScript: "release:admit",
      stageSummaryScript: "release:stage-summary",
      artifactRoot: "widgets-unsigned",
      os: ["windows-2025", "windows-11-arm", "macos-15", "macos-15-intel"],
      targets: [
        { os: "windows-2025", platform: "windows", architecture: "x86_64" },
        { os: "windows-11-arm", platform: "windows", architecture: "arm64" },
        { os: "macos-15", platform: "macos", architecture: "arm64" },
        { os: "macos-15-intel", platform: "macos", architecture: "x86_64" },
      ],
      signWindows: true,
      signMacos: true,
    },
    templateVersion: "1.5.0",
  };
  const content = renderTemplate(
    fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/release-candidate.yml"), "utf8"),
    buildRenderContext(manifest, "/repos/widgets", "release-candidate"),
  );
  for (const [runner, triple] of [["windows-2025", "x86_64-pc-windows-msvc"], ["windows-11-arm", "aarch64-pc-windows-msvc"], ["macos-15", "aarch64-apple-darwin"], ["macos-15-intel", "x86_64-apple-darwin"]]) {
    assert.match(content, new RegExp(`os: ${JSON.stringify(runner).replace(/[.*+?^${}()|[\\]\\]/g, "\\$&")}`));
    assert.match(content, new RegExp(`targetTriple: ${JSON.stringify(triple).replace(/[.*+?^${}()|[\\]\\]/g, "\\$&")}`));
  }
  assert.match(content, /targets: \$\{\{ matrix\.targetTriple \}\}/);
  assert.doesNotMatch(content, /bash \.\/scripts\/gate\.sh/);
  assert.match(content, /windows-sign:[\s\S]*matrix:[\s\S]*windows-11-arm/);
  assert.match(content, /macos-sign:[\s\S]*matrix:[\s\S]*macos-15-intel/);
  // Every artifact name is run- and identity-bound: <prefix>-<run_id>-<run_attempt>-<kind>-...-<version>-<source_revision>.
  assert.match(content, /name: widgets-\$\{\{ github\.run_id \}\}-\$\{\{ github\.run_attempt \}\}-candidate-\$\{\{ matrix\.platform \}\}-\$\{\{ matrix\.architecture \}\}-\$\{\{ needs\.admission\.outputs\.version \}\}-\$\{\{ needs\.admission\.outputs\.source_revision \}\}/);
  assert.match(content, /name: widgets-\$\{\{ github\.run_id \}\}-\$\{\{ github\.run_attempt \}\}-signed-windows-\$\{\{ matrix\.architecture \}\}-\$\{\{ needs\.admission\.outputs\.version \}\}-\$\{\{ needs\.admission\.outputs\.source_revision \}\}/);
  assert.match(content, /name: widgets-\$\{\{ github\.run_id \}\}-\$\{\{ github\.run_attempt \}\}-signed-macos-\$\{\{ matrix\.architecture \}\}-\$\{\{ needs\.admission\.outputs\.version \}\}-\$\{\{ needs\.admission\.outputs\.source_revision \}\}/);
  // Exact-count regression guard across the candidate job's build/check/
  // stage-summary steps and the windows-sign/macos-sign stage-summary steps.
  assert.equal((content.match(/RIGHT_GIT_RELEASE_ARCHITECTURE: \$\{\{ matrix\.architecture \}\}/g) ?? []).length, 10);
  assert.doesNotMatch(content, /run: (?:pnpm|npm|yarn) run undefined\b/);
  assert.doesNotMatch(content, /\$\{\{\s*github\.output\s*\}\}/);
});

test("rust-hybrid release chain preserves signed candidate, installed qualification, then publish", () => {
  const manifest = {
    ...FULL_MANIFEST,
    lanes: ["ci", "release-candidate"],
    profile: "rust-hybrid",
    gate: { path: "scripts/gate.sh" },
    candidate: {
      buildScript: "ci:candidate",
      checkScript: "ci:candidate-check",
      admissionScript: "release:admit",
      stageSummaryScript: "release:stage-summary",
      artifactRoot: "widgets-unsigned",
      os: ["windows-2025", "macos-15"],
      signWindows: true,
      signMacos: true,
      releaseChain: {
        evidenceVerificationScript: "release:verify-evidence",
        windowsFinalizeScript: "release:finalize:win",
        macosFinalizeScript: "release:finalize:mac",
        installedQualificationScript: "release:qualify:installed",
        publishScript: "release:publish:qualified",
        qualificationRunnerVariable: "QUALIFICATION_RUNNER_LABEL",
        publishSecrets: ["CLOUDFLARE_API_TOKEN"],
      },
    },
    matrix: { os: ["ubuntu-24.04", "windows-2025", "macos-15"], node: ["22.23.2"] },
    templateVersion: "1.4.0",
  };
  const content = renderTemplate(
    fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/release-candidate.yml"), "utf8"),
    buildRenderContext(manifest, "/repos/widgets", "release-candidate"),
  );
  assert.match(content, /windows-sign:[\s\S]*pnpm run release:finalize:win/);
  assert.match(content, /macos-sign:[\s\S]*pnpm run release:finalize:mac/);
  // Tag-last contract: every gate reads the ADMITTED, same-run outputs of
  // the `admission` job — never the raw workflow_dispatch input directly,
  // and never a ref/tag.
  assert.match(content, /signed_qualification:\n        description: Run protected signing, installed qualification, & evidence verification/);
  assert.doesNotMatch(content, /inputs\.signed_qualification == true/);
  assert.doesNotMatch(content, /startsWith\(github\.ref, 'refs\/tags\/v'\)/);
  assert.match(content, /windows-sign:\n    needs: \[admission, candidate\]\n    if: \$\{\{ needs\.admission\.outputs\.signed_qualification == 'true' \}\}/);
  assert.match(content, /macos-sign:\n    needs: \[admission, candidate\]\n    if: \$\{\{ needs\.admission\.outputs\.signed_qualification == 'true' \}\}/);
  assert.match(content, /installed-qualification:\n    needs: \[admission, candidate, windows-sign\]/);
  assert.match(content, /installed-qualification:[\s\S]*?if: \$\{\{ always\(\) && needs\.admission\.outputs\.signed_qualification == 'true' && needs\.candidate\.result == 'success' \}\}/);
  assert.match(content, /installed_qualification_failed/);
  assert.match(content, /shell: pwsh\n        run: \|\n          throw 'installed_qualification_failed \{ reason: prerequisites_absent, detail: "same-run Windows signing\/finalization did not succeed" \}'/);
  assert.doesNotMatch(content, /^        run: throw 'installed_qualification_failed .*detail: /m);
  assert.match(content, /pnpm run release:qualify:installed/);
  assert.match(content, /installed-qualification:[\s\S]*GH_TOKEN: \$\{\{ github\.token \}\}/);
  assert.match(content, /publish:\n    needs: \[admission, installed-qualification, macos-sign\]/);
  assert.match(content, /publish:[\s\S]*?if: \$\{\{ needs\.admission\.outputs\.publish == 'true' && needs\.installed-qualification\.result == 'success' && needs\.macos-sign\.result == 'success' \}\}/);
  const publishJobIndex = content.search(/^ {2}publish:\n {4}needs:/m);
  assert.ok(publishJobIndex >= 0, "publish job must be present");
  assert.doesNotMatch(content.slice(publishJobIndex), /inputs\.signed_qualification|inputs\.publish/);
  assert.match(content, /contents: write/);
  // Publish is the sole creator of the tag/release: it must not require a
  // pre-existing tag anywhere in its gate.
  assert.doesNotMatch(content.slice(publishJobIndex), /refs\/tags/);
  assert.match(content, /pnpm run release:verify-evidence/);
  assert.match(content, /Verify aggregated release evidence[\s\S]*?pnpm run release:verify-evidence/);
  assert.match(content, /pnpm run release:publish:qualified/);
  assert.match(content, /CLOUDFLARE_API_TOKEN: \$\{\{ secrets\.CLOUDFLARE_API_TOKEN \}\}/);
  // Ownership scan binds to the source revision input before admission emits
  // that same revision for every downstream job.
  assert.match(content, /git rev-parse HEAD.*inputs\.source_revision/);
  assert.doesNotMatch(content, /RIGHT_GIT_SOURCE_REVISION: \$\{\{ github\.sha \}\}/);
  // candidate, windows-sign, macos-sign, installed-qualification and publish all
  // check out the admitted revision with full history; admission checks out
  // ${{ inputs.source_revision }} (it IS the admission job — there is no
  // needs.admission yet). Full history is load-bearing, not cosmetic: see
  // "every job checking out an admitted revision fetches full history".
  assert.equal((content.match(/ref: \$\{\{ needs\.admission\.outputs\.source_revision \}\}\n\s+fetch-depth: 0\n\s+persist-credentials: false/g) ?? []).length, 5, "candidate, windows-sign, macos-sign, installed-qualification & publish should all pin the admitted revision with full history");
  assert.doesNotMatch(content, /ref: \$\{\{ needs\.admission\.outputs\.source_revision \}\}\n\s+persist-credentials: false/, "no admitted-revision checkout may be shallow");
  assert.match(content, /ref: \$\{\{ inputs\.source_revision \}\}\n\s+fetch-depth: 0\n\s+persist-credentials: false/, "admission must check out the exact dispatch-supplied revision");
  assert.doesNotMatch(content.slice(0, content.indexOf("  windows-sign:")), /secrets\.|id-token: write|contents: write/);
  // Downstream download-artifact names must match exactly what windows-sign/
  // macos-sign uploaded (see the multi-architecture test) — this is the
  // regression guard for the artifact-name mismatch bug.
  const windowsSignedArtifact = "widgets-${{ github.run_id }}-${{ github.run_attempt }}-signed-windows-x86_64-${{ needs.admission.outputs.version }}-${{ needs.admission.outputs.source_revision }}";
  const macosSignedArtifact = "widgets-${{ github.run_id }}-${{ github.run_attempt }}-signed-macos-arm64-${{ needs.admission.outputs.version }}-${{ needs.admission.outputs.source_revision }}";
  assert.equal((content.match(new RegExp(windowsSignedArtifact.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "g")) ?? []).length, 2, "windows-sign upload + installed-qualification/publish download must share one artifact name");
  assert.ok(content.includes(macosSignedArtifact), "publish must download the exact macOS artifact macos-sign uploaded");
  assert.doesNotMatch(content, /run: (?:pnpm|npm|yarn) run undefined\b/);
  assert.doesNotMatch(content, /\$\{\{\s*github\.output\s*\}\}/);
});

// Part E regression guard: every `run: <pm> run <script>` line in a
// rendered full release-chain workflow must name a script the manifest
// actually declares. This is the general form of the `pnpm run undefined`
// class of bug — it would also catch a typo'd or stale script reference
// even when it resolves to a real (but wrong) string, which the fail-closed
// undefined/null check in buildRenderContext cannot.
test("every 'run <script>' in a full release-chain workflow names a script the manifest declares", () => {
  const manifest = {
    ...FULL_MANIFEST,
    lanes: ["ci", "release-candidate"],
    profile: "rust-hybrid",
    gate: { path: "scripts/gate.sh" },
    candidate: {
      buildScript: "ci:candidate",
      checkScript: "ci:candidate-check",
      admissionScript: "release:admit",
      stageSummaryScript: "release:stage-summary",
      artifactRoot: "widgets-unsigned",
      os: ["windows-2025", "macos-15"],
      signWindows: true,
      signMacos: true,
      releaseChain: {
        evidenceVerificationScript: "release:verify-evidence",
        windowsFinalizeScript: "release:finalize:win",
        macosFinalizeScript: "release:finalize:mac",
        installedQualificationScript: "release:qualify:installed",
        publishScript: "release:publish:qualified",
        qualificationRunnerVariable: "QUALIFICATION_RUNNER_LABEL",
        publishSecrets: ["CLOUDFLARE_API_TOKEN"],
      },
    },
    matrix: { os: ["ubuntu-24.04", "windows-2025", "macos-15"], node: ["22.23.2"] },
    templateVersion: "1.4.0",
  };
  const content = renderTemplate(
    fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/release-candidate.yml"), "utf8"),
    buildRenderContext(manifest, "/repos/widgets", "release-candidate"),
  );
  const declaredScripts = new Set([
    manifest.candidate.buildScript,
    manifest.candidate.checkScript,
    manifest.candidate.admissionScript,
    manifest.candidate.stageSummaryScript,
    manifest.candidate.releaseChain.evidenceVerificationScript,
    manifest.candidate.releaseChain.windowsFinalizeScript,
    manifest.candidate.releaseChain.macosFinalizeScript,
    manifest.candidate.releaseChain.installedQualificationScript,
    manifest.candidate.releaseChain.publishScript,
  ]);
  const runLines = content.match(/run: (?:pnpm|npm|yarn) run \S+/g) ?? [];
  assert.ok(runLines.length > 0, "expected at least one package-script run line");
  for (const line of runLines) {
    const script = line.split(" run ").pop();
    assert.ok(declaredScripts.has(script), `rendered step runs undeclared script: ${line}`);
  }
});

// Downstream-consumer gap fix: the evidence verifier can only prove a stage
// ran and succeeded if that stage produces a stage summary AND the publish
// job actually downloads it. This test proves all four authoritative
// stages (candidate, windows-sign, macos-sign, installed-qualification)
// produce a summary, that installed-qualification wraps itself in
// init/finalize like every other stage, that publish downloads the
// complete same-run set into distinct subdirectories (no collisions
// between candidate's two platform legs), and that evidence is bound to
// this exact run (not merely version+source_revision) via RIGHT_GIT_RUN_ID/
// RIGHT_GIT_RUN_ATTEMPT on both the verification and publish steps.
test("release chain: installed-qualification has a stage summary and publish downloads the complete same-run evidence set", () => {
  const manifest = {
    ...FULL_MANIFEST,
    lanes: ["ci", "release-candidate"],
    profile: "rust-hybrid",
    gate: { path: "scripts/gate.sh" },
    candidate: {
      buildScript: "ci:candidate",
      checkScript: "ci:candidate-check",
      admissionScript: "release:admit",
      stageSummaryScript: "release:stage-summary",
      artifactRoot: "widgets-unsigned",
      os: ["windows-2025", "macos-15"],
      signWindows: true,
      signMacos: true,
      releaseChain: {
        evidenceVerificationScript: "release:verify-evidence",
        windowsFinalizeScript: "release:finalize:win",
        macosFinalizeScript: "release:finalize:mac",
        installedQualificationScript: "release:qualify:installed",
        publishScript: "release:publish:qualified",
        qualificationRunnerVariable: "QUALIFICATION_RUNNER_LABEL",
        publishSecrets: ["CLOUDFLARE_API_TOKEN"],
      },
    },
    matrix: { os: ["ubuntu-24.04", "windows-2025", "macos-15"], node: ["22.23.2"] },
    templateVersion: "1.4.0",
  };
  const content = renderTemplate(
    fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/release-candidate.yml"), "utf8"),
    buildRenderContext(manifest, "/repos/widgets", "release-candidate"),
  );
  const installedQualificationJob = content.slice(
    content.search(/^ {2}installed-qualification:\n {4}needs:/m),
    content.search(/^ {2}publish:\n {4}needs:/m),
  );

  // Point 1: installed-qualification wraps itself in stage-summary
  // init/finalize like candidate/windows-sign/macos-sign, with the correct
  // authoritative stage identifier, and finalize runs unconditionally.
  assert.match(installedQualificationJob, /name: Initialize installed-qualification stage summary\n\s+run: pnpm run release:stage-summary\n\s+env:\n\s+RIGHT_GIT_STAGE_ACTION: init/);
  assert.match(installedQualificationJob, /RIGHT_GIT_STAGE: installed-qualification/);
  assert.match(installedQualificationJob, /name: Finalize installed-qualification stage summary\n\s+if: \$\{\{ always\(\) \}\}\n\s+run: pnpm run release:stage-summary\n\s+env:\n\s+RIGHT_GIT_STAGE_ACTION: finalize/);
  assert.equal((installedQualificationJob.match(/RIGHT_GIT_STAGE: installed-qualification/g) ?? []).length, 2, "both init and finalize must carry the installed-qualification stage id");
  assert.match(installedQualificationJob, /name: Upload installed-qualification stage summary\n\s+if: \$\{\{ always\(\) \}\}/);

  const publishJob = content.slice(content.search(/^ {2}publish:\n {4}needs:/m));

  // Point 2: publish downloads the complete same-run evidence set — both
  // candidate legs (windows + macos) in distinct subdirectories, plus
  // windows-sign, macos-sign, and installed-qualification summaries.
  assert.match(publishJob, /Download same-run candidate \(Windows\) stage summary[\s\S]*?path: \$\{\{ runner\.temp \}\}\/right-git-release\/stage-summaries\/candidate-windows/);
  assert.match(publishJob, /Download same-run candidate \(macOS\) stage summary[\s\S]*?path: \$\{\{ runner\.temp \}\}\/right-git-release\/stage-summaries\/candidate-macos/);
  assert.match(publishJob, /Download same-run Windows signing stage summary[\s\S]*?path: \$\{\{ runner\.temp \}\}\/right-git-release\/stage-summaries\/windows-sign/);
  assert.match(publishJob, /Download same-run macOS signing stage summary[\s\S]*?path: \$\{\{ runner\.temp \}\}\/right-git-release\/stage-summaries\/macos-sign/);
  assert.match(publishJob, /Download same-run installed-qualification stage summary[\s\S]*?path: \$\{\{ runner\.temp \}\}\/right-git-release\/stage-summaries\/installed-qualification/);
  // Distinct subdirectories: no two downloads share a path.
  const downloadPaths = [...publishJob.matchAll(/path: (\$\{\{ runner\.temp \}\}\/right-git-release\/stage-summaries\/\S+)/g)].map((m) => m[1]);
  assert.equal(downloadPaths.length, 5);
  assert.equal(new Set(downloadPaths).size, 5, "every stage-summary download must use a distinct subdirectory");

  // Point 3: both the evidence-verification step and the publish step carry
  // the full run-identity contract (RIGHT_GIT_STAGE_SUMMARY_ROOT, run id,
  // run attempt) alongside version/source_revision — evidence is bound to
  // THIS run, not merely reusable across runs of the same version+SHA.
  const verifyStep = publishJob.slice(publishJob.indexOf("Verify aggregated release evidence"), publishJob.indexOf("Publish qualified signed release"));
  assert.match(verifyStep, /RIGHT_GIT_STAGE_SUMMARY_ROOT: \$\{\{ runner\.temp \}\}\/right-git-release\/stage-summaries/);
  assert.match(verifyStep, /RIGHT_GIT_RUN_ID: \$\{\{ github\.run_id \}\}/);
  assert.match(verifyStep, /RIGHT_GIT_RUN_ATTEMPT: \$\{\{ github\.run_attempt \}\}/);
  const publishStep = publishJob.slice(publishJob.indexOf("Publish qualified signed release"));
  assert.match(publishStep, /RIGHT_GIT_STAGE_SUMMARY_ROOT: \$\{\{ runner\.temp \}\}\/right-git-release\/stage-summaries/);
  assert.match(publishStep, /RIGHT_GIT_RUN_ID: \$\{\{ github\.run_id \}\}/);
  assert.match(publishStep, /RIGHT_GIT_RUN_ATTEMPT: \$\{\{ github\.run_attempt \}\}/);

  assert.doesNotMatch(content, /run: (?:pnpm|npm|yarn) run undefined\b/);
  assert.doesNotMatch(content, /\$\{\{\s*github\.output\s*\}\}/);
});

test("render determinism: same manifest renders byte-identical output every time", () => {
  const first = renderAllLanes(FULL_MANIFEST, "/repos/widgets");
  const second = renderAllLanes(FULL_MANIFEST, "/repos/widgets");
  for (const lane of KNOWN_LANES) {
    assert.equal(first[lane], second[lane], `lane ${lane} was not deterministic`);
  }
});

test("rendered workflows never request pnpm or yarn setup-node caching before package-manager activation", () => {
  for (const packageManager of ["pnpm@11.25.0", "yarn@4.6.0"]) {
    const rendered = renderAllLanes({ ...FULL_MANIFEST, packageManager }, "/repos/widgets");
    for (const [lane, content] of Object.entries(rendered)) {
      assert.doesNotMatch(
        content,
        /^\s*cache:\s*(?:pnpm|yarn)\s*$/m,
        `lane ${lane} must not ask actions/setup-node for ${packageManager.split("@")[0]} caching before corepack activation`,
      );
    }
  }
});

test("rendered workflows bootstrap each declared package manager safely", () => {
  const pnpm = renderAllLanes({ ...FULL_MANIFEST, packageManager: "pnpm@11.25.0" }, "/repos/widgets");
  for (const content of Object.values(pnpm)) {
    assert.match(content, /uses: pnpm\/action-setup@[a-f0-9]{40}/);
    assert.match(content, /version: 11\.25\.0/);
    assert.doesNotMatch(content, /corepack enable/);
  }

  const npm = renderAllLanes({ ...FULL_MANIFEST, packageManager: "npm@11.0.0" }, "/repos/widgets");
  for (const content of Object.values(npm)) {
    assert.doesNotMatch(content, /pnpm\/action-setup|corepack enable/);
  }

  const yarn = renderAllLanes({ ...FULL_MANIFEST, packageManager: "yarn@4.6.0" }, "/repos/widgets");
  for (const content of Object.values(yarn)) {
    assert.match(content, /corepack prepare yarn@4\.6\.0 --activate/);
    assert.doesNotMatch(content, /pnpm\/action-setup/);
  }
});

// F5 fix: a job-level `if: !private` on the real work job means GitHub
// SKIPS that job entirely for a private repo — the nested "refuse" step
// inside it is then unreachable and never runs, so nothing fails and the
// workflow can read as "nothing to do" rather than a loud failure. The
// fix moves the refusal into its own `private-repo-guard` job that carries
// NO job-level `if` (so it always executes); its single step's own `if`
// fires only when private and exits 1, which fails that job outright. Real
// work jobs `needs: [private-repo-guard]` instead of gating themselves, so
// a failed (not skipped) guard blocks them via GitHub's default
// needs-implies-success semantics. This test asserts the corrected shape;
// see guard-coverage.test.mjs for the full job-graph reachability proof.
test("guard injection: every rendered workflow has an unconditional guard job that refuses a private repository", () => {
  const rendered = renderAllLanes(FULL_MANIFEST, "/repos/widgets");
  for (const lane of KNOWN_LANES) {
    const content = rendered[lane];
    assert.ok(content.startsWith(MANAGED_MARKER), `lane ${lane} is missing the managed marker`);
    assert.match(content, /^ {2}private-repo-guard:\s*$/m, `lane ${lane} missing the private-repo-guard job`);
    assert.match(content, /Refuse private repository/, `lane ${lane} missing the fail-fast guard step`);
    assert.match(content, /if: \$\{\{ github\.event\.repository\.private \}\}/, `lane ${lane} guard step missing its own if condition`);
    assert.match(content, /exit 1/, `lane ${lane} guard step must exit non-zero`);
    // No job anywhere in the file may gate itself on the OLD (skip, not
    // fail) pattern — that is exactly the bug being fixed.
    assert.doesNotMatch(
      content,
      /if: \$\{\{ !github\.event\.repository\.private \}\}/,
      `lane ${lane} still gates a job with the skip-not-fail 'if: !private' pattern`,
    );
  }
});

// F15 regression: the old template shipped HOMEBREW_TAP_TOKEN and
// WINGET_CREATE_GITHUB_TOKEN with no documentation of what they are or why
// they don't violate I2, while the surrounding design text describes a
// strict credential boundary ("no signing credential ... beyond OIDC-to-npm"
// / "never signs in CI") without acknowledging these two long-lived PATs.
// Fails against the pre-fix header comment (no such text existed) and
// passes once the template documents the boundary honestly.
test("F15: publish-package-managers documents its credential boundary honestly", () => {
  const rendered = renderAllLanes(FULL_MANIFEST, "/repos/widgets")["publish-package-managers"];
  assert.match(rendered, /CREDENTIAL BOUNDARY/, "template must call out its credential boundary explicitly");
  assert.match(rendered, /HOMEBREW_TAP_TOKEN:[\s\S]*?PAT/i, "must document what HOMEBREW_TAP_TOKEN is");
  assert.match(rendered, /WINGET_CREATE_GITHUB_TOKEN:[\s\S]*?PAT/i, "must document what WINGET_CREATE_GITHUB_TOKEN is");
  assert.match(rendered, /not signing credentials/i, "must state that these tokens are not signing material (I2 is about signing credentials specifically)");
  assert.match(rendered, /opt-in/i, "must state the lane is opt-in, not rendered by default");
});

// F15 non-regression: the two PAT-style secrets must never leak into any
// OTHER lane's rendered output — they belong solely to the explicitly
// opted-in publish-package-managers lane.
test("F15: HOMEBREW_TAP_TOKEN and WINGET_CREATE_GITHUB_TOKEN never appear outside the publish-package-managers lane", () => {
  const otherLanes = KNOWN_LANES.filter((lane) => lane !== "publish-package-managers");
  const rendered = renderAllLanes({ ...FULL_MANIFEST, lanes: otherLanes }, "/repos/widgets");
  for (const lane of otherLanes) {
    assert.doesNotMatch(rendered[lane], /HOMEBREW_TAP_TOKEN|WINGET_CREATE_GITHUB_TOKEN/, `lane ${lane} must never reference the package-manager PAT secrets`);
  }
});

test("renderTemplate throws on an unresolved placeholder rather than emitting it", () => {
  assert.throws(() => renderTemplate("value: ${MISSING_TOKEN}\n", {}), /no value provided/);
});

test("renderTemplate never substitutes inside GitHub Actions double-brace expressions", () => {
  const out = renderTemplate("run: echo ${{ github.repository }}\nname: ${TEMPLATE_VERSION}\n", { TEMPLATE_VERSION: "1.0.0" });
  assert.equal(out, "run: echo ${{ github.repository }}\nname: 1.0.0\n");
});

test("renderTemplate normalizes an empty final release-chain fragment to one newline", () => {
  const out = renderTemplate("jobs:\n${RELEASE_CHAIN_JOBS}\n", { RELEASE_CHAIN_JOBS: "" });
  assert.equal(out, "jobs:\n");
  assert.equal((out.match(/\n/g) ?? []).length, 1);
});

// Tag-last has no tag to check out, so every stage checks out one exact admitted
// commit SHA and HEAD is always detached. @rightkit/release only exempts a
// detached HEAD when it can prove the commit is reachable from the dispatched
// remote branch — and proving reachability needs history plus the
// remote-tracking ref. A default shallow checkout has neither, so the signing,
// qualification, and publish jobs would be refused. Every job that checks out an
// admitted revision must therefore ask for full history.
test("every job checking out an admitted revision fetches full history", () => {
  const manifest = {
    ...FULL_MANIFEST,
    lanes: ["ci", "release-candidate"],
    profile: "rust-hybrid",
    gate: { path: "scripts/gate.sh" },
    candidate: {
      buildScript: "ci:candidate",
      checkScript: "ci:candidate-check",
      admissionScript: "release:admission",
      stageSummaryScript: "release:stage-summary",
      artifactRoot: "widgets-unsigned",
      os: ["windows-2025", "macos-15"],
      signWindows: true,
      signMacos: true,
      releaseChain: {
        evidenceVerificationScript: "release:verify:evidence",
        windowsFinalizeScript: "release:finalize:win",
        macosFinalizeScript: "release:finalize:mac",
        installedQualificationScript: "release:qualify:installed",
        publishScript: "release:publish:qualified",
        qualificationRunnerVariable: "QUALIFICATION_RUNNER_LABEL",
        publishSecrets: ["CLOUDFLARE_API_TOKEN"],
      },
    },
    matrix: { os: ["ubuntu-24.04", "windows-2025", "macos-15"], node: ["22.23.2"] },
    templateVersion: "1.9.0",
  };
  const content = renderTemplate(
    fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/release-candidate.yml"), "utf8"),
    buildRenderContext(manifest, "/repos/widgets", "release-candidate"),
  );
  // The check job (rightkit#833) is the one exception: it checks out the same revision at depth 1 because it only reads
  // the committed files of HEAD (`rightkit-gate check --commit HEAD`) and never runs right-release, so it has no
  // reachability to prove. Every job that builds, signs, qualifies or publishes the revision still fetches full history.
  const checkJobText = content.slice(content.indexOf("\n  check:\n"), content.indexOf("\n  admission:\n"));
  assert.match(checkJobText, /ref: \$\{\{ inputs\.source_revision \}\}\n {10}fetch-depth: 1\n/, "the check job is the shallow checkout of the dispatched revision");
  assert.doesNotMatch(checkJobText, /right-release/);
  const checkouts = content
    .replace(checkJobText, "")
    .split(/^ {6}- uses: actions\/checkout@/m)
    .slice(1)
    .map((step) => step.split(/^ {6}- /m)[0]);
  assert.ok(checkouts.length >= 6, `expected every stage to check out; found ${checkouts.length}`);
  for (const step of checkouts) {
    if (!/ref: \$\{\{ (needs\.admission\.outputs\.source_revision|inputs\.source_revision) \}\}/.test(step)) continue;
    assert.match(step, /fetch-depth: 0/, `a checkout of an admitted revision must fetch full history:\n${step}`);
  }
  // The four downstream stages specifically — these were the ones shipping shallow.
  for (const job of ["windows-sign", "macos-sign", "installed-qualification", "publish"]) {
    const body = content.split(new RegExp(`^  ${job}:$`, "m"))[1] ?? "";
    const checkout = body.split(/^ {6}- uses: actions\/checkout@/m)[1]?.split(/^ {6}- /m)[0] ?? "";
    assert.match(checkout, /fetch-depth: 0/, `${job} must check out with full history`);
  }
});

// Rule 3 (release workflows have a dry run): a boolean dry_run input, runnable
// from a branch, must thread through admission so packaging/signing/qualification
// run while publication is skipped and artifacts carry a dry-run suffix.
test("release-candidate template exposes a dry_run input threaded through admission", () => {
  const manifest = {
    ...FULL_MANIFEST,
    lanes: ["ci", "release-candidate"],
    profile: "rust-hybrid",
    gate: { path: "scripts/gate.sh" },
    candidate: {
      buildScript: "ci:candidate",
      checkScript: "ci:candidate-check",
      admissionScript: "release:admission",
      stageSummaryScript: "release:stage-summary",
      artifactRoot: "widgets-unsigned",
      os: ["windows-2025", "macos-15"],
      signWindows: true,
      signMacos: true,
    },
    matrix: { os: ["ubuntu-24.04", "windows-2025", "macos-15"], node: ["22.23.2"] },
    templateVersion: "1.9.0",
  };
  const content = renderTemplate(
    fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/release-candidate.yml"), "utf8"),
    buildRenderContext(manifest, "/repos/widgets", "release-candidate"),
  );
  assert.match(content, /^      dry_run:\n        description: .*\n        required: false\n        type: boolean\n        default: false$/m);
  assert.match(content, /dry_run: \$\{\{ steps\.admit\.outputs\.dry_run \}\}/);
  assert.match(content, /artifact_suffix: \$\{\{ steps\.admit\.outputs\.artifact_suffix \}\}/);
  assert.match(content, /RIGHT_GIT_DRY_RUN: \$\{\{ inputs\.dry_run \}\}/);
  // The envelope only pins refs/heads/main and first-attempt for a real dispatch.
  assert.match(content, /if \[\[ "\$\{\{ inputs\.dry_run \}\}" == "true" \]\]; then\n            \[\[ "\$GITHUB_REF" == refs\/heads\/\* \]\]/);
});

// Correction 11: Windows signing, macOS signing, installed qualification and
// publication all shared one `release` environment, so no signing/publication
// grant separation existed. The split is opt-in per product because the secrets
// live in the existing environment until an operator moves them.
function environmentManifest(environments) {
  const manifest = {
    schemaVersion: 1,
    compile: "github-actions-only",
    lanes: ["ci", "release-candidate"],
    packageManager: "pnpm@11.25.0",
    profile: "rust-hybrid",
    gate: { path: "scripts/gate.sh" },
    candidate: {
      buildScript: "ci:candidate",
      checkScript: "ci:candidate-check",
      admissionScript: "release:admit",
      stageSummaryScript: "release:stage-summary",
      artifactRoot: "widgets-unsigned",
      os: ["windows-2025", "macos-15"],
      targets: [
        { os: "windows-2025", platform: "windows", architecture: "x86_64" },
        { os: "macos-15", platform: "macos", architecture: "arm64" },
      ],
      signWindows: true,
      signMacos: true,
      releaseChain: {
        windowsFinalizeScript: "release:finalize-windows",
        macosFinalizeScript: "release:finalize-macos",
        installedQualificationScript: "release:qualify-installed",
        publishScript: "release:publish-qualified",
        qualificationRunnerVariable: "WIDGETS_QUALIFICATION_RUNNER",
      },
    },
    matrix: { os: ["ubuntu-24.04", "windows-2025"], node: ["22.23.2"] },
    templateVersion: "1.8.0",
  };
  if (environments) manifest.candidate.releaseChain.environments = environments;
  return manifest;
}

function renderRelease(manifest) {
  return renderTemplate(
    fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/release-candidate.yml"), "utf8"),
    buildRenderContext(manifest, "/repos/widgets", "release-candidate"),
  );
}

test("release environments default to the single release environment", () => {
  const content = renderRelease(environmentManifest(null));
  assert.ok((content.match(/environment: release$/gm) ?? []).length >= 2);
  assert.doesNotMatch(content, /environment: signing-authorized/);
});

test("signing and publication split into distinct protected environments", () => {
  const content = renderRelease(environmentManifest({
    signing: "signing-authorized",
    publication: "release-authorized",
    qualification: "qualification",
  }));
  assert.equal((content.match(/environment: signing-authorized$/gm) ?? []).length, 2);
  assert.equal((content.match(/environment: release-authorized$/gm) ?? []).length, 1);
  // A job that may sign must never also be the job that publishes.
  assert.doesNotMatch(content, /environment: release$/m);
});

test("an unsafe or unknown release environment is rejected", () => {
  assert.equal(validateManifest(environmentManifest({ signing: "bad name!" })).valid, false);
  assert.equal(validateManifest(environmentManifest({ promotion: "x" })).valid, false);
});

test("ci.yml ownership check cannot be removed by deleting optional repo metadata", () => {
  const manifest = { ...structuredClone(FULL_MANIFEST), ownership: { roots: [".", "engine"] } };
  const template = fs.readFileSync(path.join(templatesDir, "ci.yml"), "utf8");
  const withStep = renderTemplate(template, buildRenderContext(manifest, "/repos/coderight"));
  assert.match(withStep, /RightKit ownership \(protected registry roots\)/);
  const without = renderTemplate(template, buildRenderContext(structuredClone(FULL_MANIFEST), "/repos/widgets"));
  assert.match(without, /RightKit ownership \(protected registry roots\)/);
});

test("rust-hybrid ci uploads QA evidence after the gate only when qaEvidencePath is set", () => {
  const base = { ...FULL_MANIFEST, lanes: ["ci"], profile: "rust-hybrid", gate: { path: "scripts/gate.sh" }, templateVersion: "1.1.0" };
  const template = fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/ci.yml"), "utf8");

  const unset = renderTemplate(template, buildRenderContext(structuredClone(base), "/repos/widgets"));
  assert.doesNotMatch(unset, /upload-artifact|QA evidence|runner\.temp/);
  assert.match(unset, /CARGO_PROFILE_TEST_DEBUG: "0"\n      - name: Record last green commit marker[\s\S]*github\.run_attempt \}\}\n$/);

  const set = renderTemplate(template, buildRenderContext({ ...structuredClone(base), qaEvidencePath: "pulse-hub-qa" }, "/repos/widgets"));
  assert.ok(set.indexOf("pnpm exec rightkit-gate --to compile") < set.indexOf("- name: Upload QA evidence"), "upload must follow the gate step");
  assert.match(
    set,
    /      - name: Upload QA evidence\n        if: always\(\)\n        uses: actions\/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02 # v4\n        with:\n          name: qa-evidence-\$\{\{ matrix\.os \|\| runner\.os \}\}-\$\{\{ github\.run_attempt \}\}\n          path: \$\{\{ runner\.temp \}\}\/pulse-hub-qa\n          retention-days: 7\n          if-no-files-found: ignore\n$/,
  );
  // The upload step is the only difference between the two renders.
  assert.equal(set.replace(/      - name: Upload QA evidence[\s\S]*$/, ""), unset);

  const custom = renderTemplate(template, buildRenderContext({ ...structuredClone(base), qaEvidencePath: "pulse-hub-qa", qaEvidenceRetentionDays: 30 }, "/repos/widgets"));
  assert.match(custom, /retention-days: 30\n/);
});

const INCREMENTAL_BASE = {
  ...FULL_MANIFEST,
  lanes: ["ci"],
  profile: "rust-hybrid",
  gate: { path: "scripts/gate.sh" },
  matrix: { os: ["xcode-27", "windows-2025"], node: ["26.8.1"] },
  ciAllOs: true,
};
const renderRustCi = (manifest) => renderTemplate(
  fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/ci.yml"), "utf8"),
  buildRenderContext(structuredClone(manifest), "/repos/pulse"),
);

test("ciIncremental unset or false renders the legacy cache settings; true opts in", () => {
  const unset = renderRustCi(INCREMENTAL_BASE);
  assert.match(unset, /cache-workspace-crates: false/);
  assert.match(unset, /CARGO_INCREMENTAL: "0"/);
  assert.doesNotMatch(unset, /DerivedData|actions\/cache@|dev-mac|dev-windows/);
  assert.equal(renderRustCi({ ...INCREMENTAL_BASE, ciIncremental: false }), unset);

  const on = renderRustCi({ ...INCREMENTAL_BASE, ciIncremental: true });
  assert.match(on, /cache-workspace-crates: true/);
  assert.match(on, /CARGO_INCREMENTAL: "1"/);
  assert.match(on, /RUSTC_WRAPPER: ""\n/, "sccache refuses incremental builds");
  assert.doesNotMatch(on, /RUSTC_WRAPPER: \$\{\{ steps\.rust-changes/);
  assert.match(on, /Cache Xcode DerivedData\n        if: \$\{\{ runner\.os == 'macOS' \}\}\n        uses: actions\/cache@[0-9a-f]{40} # v4\.2\.4/);
  assert.match(on, /hashFiles\('\*\*\/Cargo\.lock', '\*\*\/Package\.resolved'/);
});

const DEV_MANIFEST = {
  ...INCREMENTAL_BASE,
  devArtifact: {
    mac: "sign",
    windows: "unsigned",
    retentionDays: 5,
    macAppPath: "target/release/bundle/macos/Pulse.app",
    windowsGlob: "target/release/*.exe",
  },
};

test("devArtifact renders push-to-default-branch dev steps and a signing job; unset adds nothing", () => {
  const out = renderRustCi(DEV_MANIFEST);
  assert.match(out, /name: dev-windows-\$\{\{ github\.sha \}\}\n          path: target\/release\/\*\.exe\n          retention-days: 5/);
  assert.match(out, /DEV_APP_PATH: "target\/release\/bundle\/macos\/Pulse\.app"/);
  assert.match(out, /dev-mac-sign:\n[\s\S]*needs: \[ci\]\n    if: \$\{\{ github\.event_name == 'push' && github\.ref_name == github\.event\.repository\.default_branch \}\}\n    environment: release\n    runs-on: xcode-27/);
  assert.match(out, /codesign --force --deep --strict --options runtime --timestamp --sign "\$APPLE_DEVELOPER_ID"/);
  assert.match(out, /name: dev-mac-\$\{\{ github\.sha \}\}/);
  assert.doesNotMatch(out, /notarytool|APPLE_API_KEY|stapler/);
  // Signing material never reaches the PR-reachable ci job.
  assert.doesNotMatch(out.split("  dev-mac-sign:")[0], /secrets\./);
  // mac/windows only render the legs they declare.
  const winOnly = renderRustCi({ ...DEV_MANIFEST, devArtifact: { windows: "unsigned", windowsGlob: "dist/*.exe" } });
  assert.doesNotMatch(winOnly, /dev-mac-sign|dev-unsigned-mac/);
  assert.match(winOnly, /retention-days: 7/);
  assert.equal(renderRustCi({ ...INCREMENTAL_BASE, devArtifact: undefined }), renderRustCi(INCREMENTAL_BASE));
});

test("devArtifact mac 'unsigned' uploads the bare app with no signing job, and dispatch opens unsigned legs only", () => {
  const unsigned = { ...DEV_MANIFEST, devArtifact: { ...DEV_MANIFEST.devArtifact, mac: "unsigned" } };
  const out = renderRustCi(unsigned);
  assert.match(out, /DEV_APP_PATH: "target\/release\/bundle\/macos\/Pulse\.app"/);
  assert.match(out, /name: dev-mac-unsigned-\$\{\{ github\.sha \}\}\n          path: \$\{\{ runner\.temp \}\}\/dev-unsigned\/app\.zip\n          retention-days: 5/);
  assert.doesNotMatch(out, /dev-mac-sign|dev-unsigned-mac-|secrets\.|environment:/);
  assert.doesNotMatch(out, /workflow_dispatch/);
  assert.equal(out, renderRustCi({ ...unsigned, devArtifact: { ...unsigned.devArtifact, dispatch: undefined } }));

  const dispatch = renderRustCi({ ...unsigned, devArtifact: { ...unsigned.devArtifact, dispatch: true } });
  assert.match(dispatch, /^on:\n  pull_request:\n  push:\n    branches: \[main\]\n  workflow_dispatch:\npermissions:/m);
  const cond = "\\(github\\.event_name == 'push' && github\\.ref_name == github\\.event\\.repository\\.default_branch \\|\\| github\\.event_name == 'workflow_dispatch'\\)";
  assert.match(dispatch, new RegExp(`if: \\$\\{\\{ ${cond} && runner\\.os == 'Windows' \\}\\}`));
  assert.match(dispatch, new RegExp(`if: \\$\\{\\{ ${cond} && runner\\.os == 'macOS' \\}\\}`));

  // Dispatch beside a signed mac leg: the signing job and its package step stay push-only.
  const signedDispatch = renderRustCi({ ...DEV_MANIFEST, devArtifact: { ...DEV_MANIFEST.devArtifact, dispatch: true } });
  assert.match(signedDispatch, /workflow_dispatch:/);
  assert.match(signedDispatch, /Package unsigned dev app for signing \(macOS\)|Package unsigned dev app \(macOS\)\n        if: \$\{\{ github\.event_name == 'push' && github\.ref_name == github\.event\.repository\.default_branch && runner\.os == 'macOS' \}\}/);
  assert.match(signedDispatch, /dev-mac-sign:\n[\s\S]*?if: \$\{\{ github\.event_name == 'push' && github\.ref_name == github\.event\.repository\.default_branch \}\}\n    environment: release/);
  assert.doesNotMatch(signedDispatch.split("  dev-mac-sign:")[1], /workflow_dispatch/);
});

test("regenerate lane is dispatch-only, pushes only to right-git/regenerate-<run>, opens a PR, and reaches no secret", () => {
  for (const profile of ["rust-hybrid", "node"]) {
    const manifest = profile === "rust-hybrid"
      ? { ...INCREMENTAL_BASE, lanes: ["ci", "regenerate"], regenerate: { script: "regenerate" } }
      : { ...FULL_MANIFEST, lanes: ["ci", "regenerate"], regenerate: { script: "regenerate" } };
    assert.equal(validateManifest(manifest).valid, true, JSON.stringify(validateManifest(manifest).errors));
    const templatePath = profile === "rust-hybrid" ? "profiles/rust-hybrid/regenerate.yml" : "regenerate.yml";
    const out = renderTemplate(fs.readFileSync(path.join(templatesDir, templatePath), "utf8"), buildRenderContext(manifest, "/repos/widgets", "regenerate"));
    assert.match(out, /^on:\n  workflow_dispatch:\npermissions:\n  contents: read\n/m);
    assert.match(out, /regenerate:\n    needs: \[private-repo-guard\]\n    if: \$\{\{ github\.event_name == 'workflow_dispatch' \}\}\n/);
    assert.match(out, /    permissions:\n      contents: write\n      pull-requests: write\n/);
    assert.match(out, /run: pnpm run regenerate\n/);
    assert.match(out, /branch="right-git\/regenerate-\$\{\{ github\.run_id \}\}-\$\{\{ github\.run_attempt \}\}"\n/);
    assert.match(out, /git push origin "HEAD:refs\/heads\/\$branch"\n/);
    assert.match(out, /gh pr create --base "\$RIGHT_GIT_DEFAULT_BRANCH" --head "\$branch"/);
    assert.doesNotMatch(out, /secrets\.|HEAD:main|HEAD:refs\/heads\/main|pull_request:|push:\n/);
    assert.equal((out.match(/git push/g) ?? []).length, 1);
    if (profile === "rust-hybrid") {
      assert.match(out, /run: pnpm exec rightkit-gate --to compile\n/);
      assert.match(out, /dtolnay\/rust-toolchain@/);
    } else {
      assert.match(out, /run: pnpm run test\n/);
      assert.doesNotMatch(out, /rust-toolchain/);
    }
  }
  assert.equal(validateManifest({ ...FULL_MANIFEST, lanes: ["ci", "regenerate"], regenerate: undefined }).valid, false, "lane needs regenerate.script");
  assert.equal(validateManifest({ ...FULL_MANIFEST, lanes: ["ci"], regenerate: { script: "x" } }).valid, false, "regenerate needs the lane");
  assert.equal(validateManifest({ ...FULL_MANIFEST, lanes: ["ci", "regenerate"], regenerate: { script: "re gen" } }).valid, false, "script name charset");
});

test("devArtifact mac 'notarize' notarizes and staples inside dev-mac-sign only; 'sign' output is unchanged", () => {
  const signed = renderRustCi(DEV_MANIFEST);
  const notarized = renderRustCi({ ...DEV_MANIFEST, devArtifact: { ...DEV_MANIFEST.devArtifact, mac: "notarize" } });
  assert.doesNotMatch(signed, /notarytool|stapler|AuthKey|APPLE_API/);
  const [ciPart, jobPart] = notarized.split("  dev-mac-sign:");
  assert.doesNotMatch(ciPart, /secrets\.|notarytool/);
  assert.match(jobPart, /xcrun notarytool submit "\$zip" --key "\$RUNNER_TEMP\/AuthKey\.p8" --key-id "\$APPLE_API_KEY" --issuer "\$APPLE_API_ISSUER" --wait\n\s+xcrun stapler staple "\$app"\n\s+xcrun stapler validate "\$app"/);
  assert.match(jobPart, /APPLE_API_KEY_BASE64: \$\{\{ secrets\.APPLE_API_KEY_BASE64 \}\}/);
  assert.match(jobPart, /Upload signed and notarized dev app/);
  assert.match(jobPart, /rm -f "\$RUNNER_TEMP\/apple-signing\.p12" "\$RUNNER_TEMP\/AuthKey\.p8"/);
  assert.match(jobPart, /if: \$\{\{ github\.event_name == 'push' && github\.ref_name == github\.event\.repository\.default_branch \}\}\n    environment: release/);
  const notarizeEntitled = validateManifest({ ...DEV_MANIFEST, regenerate: undefined, devArtifact: { ...DEV_MANIFEST.devArtifact, mac: "notarize", macEntitlements: "build/pulse.entitlements" } });
  assert.equal(notarizeEntitled.valid, true, JSON.stringify(notarizeEntitled.errors));
  assert.equal(validateManifest({ ...DEV_MANIFEST, regenerate: undefined, devArtifact: { mac: "notarize" } }).valid, false, "notarize needs macAppPath");
});

test("devArtifact entitlements add a checkout and --entitlements", () => {
  const out = renderRustCi({ ...DEV_MANIFEST, devArtifact: { ...DEV_MANIFEST.devArtifact, macEntitlements: "build/pulse.entitlements" } });
  assert.match(out, /--entitlements "\$GITHUB_WORKSPACE\/build\/pulse\.entitlements"/);
  assert.match(out.split("  dev-mac-sign:")[1], /actions\/checkout@/);
});

// Orthic-Labs/rightkit#257: macSignManifest swaps `codesign --deep` for the per-file sign-dev-artifact command.
const PER_FILE_DEV = { ...DEV_MANIFEST.devArtifact, macSignManifest: "build/dev-sign.json", macPayloadMetadata: "Pulse.app/Contents/Resources/payload.json" };

test("devArtifact macSignManifest signs per file through right-release sign-dev-artifact, not codesign --deep", () => {
  const out = renderRustCi({ ...DEV_MANIFEST, devArtifact: PER_FILE_DEV });
  const sign = out.split("  dev-mac-sign:")[1];
  assert.doesNotMatch(sign, /codesign --force --deep/);
  assert.match(sign, /pnpm exec right-release sign-dev-artifact --artifact "\$work" --manifest "\$GITHUB_WORKSPACE\/build\/dev-sign\.json" --metadata "\$work\/Pulse\.app\/Contents\/Resources\/payload\.json" --evidence "\$RUNNER_TEMP\/dev-signed\/sign-evidence\.json"/);
  assert.match(sign, /actions\/checkout@[\s\S]*actions\/setup-node@[\s\S]*pnpm install --frozen-lockfile[\s\S]*Install Apple signing identity/);
  assert.match(sign, /sign-evidence\.json\n/);
  assert.doesNotMatch(sign, /notarytool/, "the per-file lane never notarizes");
});

test("devArtifact without macSignManifest keeps the codesign --deep lane", () => {
  const out = renderRustCi(DEV_MANIFEST);
  assert.match(out.split("  dev-mac-sign:")[1], /codesign --force --deep --strict --options runtime --timestamp --sign "\$APPLE_DEVELOPER_ID" "\$app"\n          codesign --verify --deep --strict "\$app"/);
  assert.doesNotMatch(out, /sign-dev-artifact/);
});

test("devArtifact macSignManifest validation", () => {
  const check = (devArtifact) => validateManifest({ ...DEV_MANIFEST, regenerate: undefined, devArtifact });
  assert.equal(check(PER_FILE_DEV).valid, true);
  assert.equal(check({ ...PER_FILE_DEV, macPayloadMetadata: undefined }).valid, false, "metadata path required");
  assert.equal(check({ ...PER_FILE_DEV, mac: "notarize" }).valid, false, "no notarize");
  assert.equal(check({ ...PER_FILE_DEV, mac: "unsigned" }).valid, false, "needs sign");
  assert.equal(check({ ...PER_FILE_DEV, macEntitlements: "build/pulse.entitlements" }).valid, false, "no entitlements beside a manifest");
  assert.equal(check({ ...PER_FILE_DEV, macSignManifest: "../x.json" }).valid, false, "safe path");
  assert.equal(check({ ...DEV_MANIFEST.devArtifact, macPayloadMetadata: "a/payload.json" }).valid, false, "metadata alone");
});

// Orthic-Labs/rightkit#257: the sign-existing lane signs an already-qualified artifact of another run; it never builds.
const SIGN_EXISTING_MANIFEST = {
  ...INCREMENTAL_BASE,
  lanes: ["ci", "sign-existing"],
  signExisting: { macSignManifest: "build/dev-sign.json", macPayloadMetadata: "Legion.app/Contents/Resources/payload.json" },
};
const renderSignExisting = (manifest) => renderTemplate(
  fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid", LANE_FILENAMES["sign-existing"]), "utf8"),
  buildRenderContext(structuredClone(manifest), "/repos/legion", "sign-existing"),
);

test("sign-existing renders a dispatch-only, minimal-permission, protected signing job that never builds", () => {
  const out = renderSignExisting(SIGN_EXISTING_MANIFEST);
  assert.match(out, /^name: legion-sign-existing$/m);
  assert.match(out, /on:\n  workflow_dispatch:\n    inputs:\n      run_id:[\s\S]*      artifact:[\s\S]*      source_sha:[\s\S]*\npermissions:\n  contents: read\n  actions: read\n/);
  const job = out.split("\n  sign-existing:\n")[1];
  assert.match(job, /needs: \[private-repo-guard\]\n    if: \$\{\{ github\.event_name == 'workflow_dispatch' && github\.ref_name == github\.event\.repository\.default_branch \}\}\n    environment: release\n    runs-on: xcode-27/);
  assert.match(job, /permissions:\n      contents: read\n      actions: read\n/);
  assert.doesNotMatch(out, /--release|cargo |notarytool|APPLE_API_KEY|buildScript/);
  assert.doesNotMatch(out, /(?:contents|actions|id-token|pull-requests|packages): write/);
  assert.match(job, /pnpm exec right-release sign-dev-artifact --artifact "\$work" --manifest "\$GITHUB_WORKSPACE\/build\/dev-sign\.json" --metadata "\$work\/Legion\.app\/Contents\/Resources\/payload\.json" --evidence "\$RUNNER_TEMP\/dev-signed\/sign-evidence\.json"/);
  assert.match(job, /sign-evidence\.json\n[\s\S]*source-verification\.json\n          retention-days: 7\n          if-no-files-found: error/);
  // Inputs reach the shell only through env, never through inline expressions in run scripts.
  assert.match(job, /INPUT_RUN_ID: \$\{\{ inputs\.run_id \}\}[\s\S]*INPUT_ARTIFACT: \$\{\{ inputs\.artifact \}\}[\s\S]*INPUT_SOURCE_SHA: \$\{\{ inputs\.source_sha \}\}/);
  assert.equal(out.match(/inputs\./g).length, 3);
});

test("sign-existing verifies repository, run, artifact and head sha before it downloads, and signs only after", () => {
  const job = renderSignExisting(SIGN_EXISTING_MANIFEST).split("\n  sign-existing:\n")[1];
  const at = (needle) => { const index = job.indexOf(needle); assert.ok(index >= 0, `missing ${needle}`); return index; };
  assert.ok(at("id: verify") < at("actions/download-artifact@") && at("actions/download-artifact@") < at("Install Apple signing identity") && at("Install Apple signing identity") < at("sign-dev-artifact"));
  assert.match(job, /actions\/runs\/\$INPUT_RUN_ID"\)"/);
  assert.match(job, /run \$INPUT_RUN_ID belongs to \$run_repo, not \$GITHUB_REPOSITORY/);
  assert.match(job, /another repository or a fork/);
  assert.match(job, /case "\$head_sha" in "\$INPUT_SOURCE_SHA"\*\)/);
  assert.match(job, /\.workflow_run\.id[\s\S]*\.workflow_run\.head_sha[\s\S]*\.workflow_run\.head_repository_id/);
  assert.match(job, /git merge-base --is-ancestor "\$head_sha" "\$GITHUB_SHA"/);
  assert.match(job, /uses: actions\/download-artifact@[0-9a-f]{40} # v4\.3\.0\n        with:\n          name: \$\{\{ steps\.verify\.outputs\.artifact_name \}\}\n          run-id: \$\{\{ steps\.verify\.outputs\.run_id \}\}\n          repository: \$\{\{ github\.repository \}\}\n          github-token: \$\{\{ github\.token \}\}/);
  assert.match(job, /uses: actions\/(?:checkout|setup-node|upload-artifact)@[0-9a-f]{40}/);
  assert.doesNotMatch(job, /uses: [^\n]*@v\d/, "third-party actions are pinned to a commit");
});

test("sign-existing mirrors the dev-mac-sign secret and keychain handling and adds no new secret", () => {
  const sign = renderSignExisting(SIGN_EXISTING_MANIFEST);
  assert.deepEqual([...new Set([...sign.matchAll(/secrets\.([A-Z0-9_]+)/g)].map((match) => match[1]))].sort(), ["APPLE_CERTIFICATE_BASE64", "APPLE_CERTIFICATE_PASSWORD", "APPLE_KEYCHAIN_PASSWORD"]);
  const devMac = renderRustCi({ ...DEV_MANIFEST, devArtifact: PER_FILE_DEV }).split("  dev-mac-sign:")[1];
  const block = (text, from, to) => text.slice(text.indexOf(from), text.indexOf(to));
  assert.equal(block(sign, "      - name: Install Apple signing identity", "      - name: Developer ID sign"), block(devMac, "      - name: Install Apple signing identity", "      - name: Developer ID sign"));
  const cleanup = '      - name: Remove Apple signing material\n        if: ${{ always() }}\n        shell: bash\n        run: |\n          security delete-keychain "$RUNNER_TEMP/apple-signing.keychain-db" 2>/dev/null || true\n          rm -f "$RUNNER_TEMP/apple-signing.p12"\n';
  assert.ok(sign.includes(cleanup) && devMac.includes(cleanup), "same keychain removal");
  assert.match(sign, /APPLE_DEVELOPER_ID: \$\{\{ vars\.APPLE_DEVELOPER_ID \}\}/);
});

test("sign-existing environment defaults to the signing environment and can be set", () => {
  assert.match(renderSignExisting(SIGN_EXISTING_MANIFEST), /\n    environment: release\n/);
  assert.match(renderSignExisting({ ...SIGN_EXISTING_MANIFEST, signExisting: { ...SIGN_EXISTING_MANIFEST.signExisting, environment: "legion-sign" } }), /\n    environment: legion-sign\n/);
});

test("signExisting unset leaves the ci workflow byte-identical, and the lane is not rendered unless declared", () => {
  assert.equal(renderRustCi(SIGN_EXISTING_MANIFEST), renderRustCi(INCREMENTAL_BASE));
  assert.doesNotMatch(renderRustCi(SIGN_EXISTING_MANIFEST), /sign-existing|sign-dev-artifact/);
  assert.equal(KNOWN_LANES.includes("sign-existing"), false, "node-profile all-lane checks keep their lane list");
});

// signExisting.macPayloadMetadataSource: the unsigned packaging step zips the .app plus the generated payload metadata file.
const KEEP_PARENT = 'ditto -c -k --keepParent "$DEV_APP_PATH" "$RUNNER_TEMP/dev-unsigned/app.zip"';
const WITH_SOURCE = { macSignManifest: "build/dev-sign.json", macPayloadMetadata: "payload.json", macPayloadMetadataSource: "dist/dev-macos/payload.json" };
const withSource = (devArtifact, signExisting = WITH_SOURCE) => ({ ...INCREMENTAL_BASE, lanes: ["ci", "sign-existing"], devArtifact, signExisting });

test("macPayloadMetadataSource unset keeps the packaging step byte-identical in every macOS leg", () => {
  const legs = [
    DEV_MANIFEST.devArtifact,
    { mac: "unsigned", macAppPath: "target/release/bundle/macos/Pulse.app" },
    { ...DEV_MANIFEST.devArtifact, parallel: true, buildScript: "build:dev" },
  ];
  for (const devArtifact of legs) {
    const plain = renderRustCi({ ...INCREMENTAL_BASE, devArtifact });
    assert.ok(plain.includes(KEEP_PARENT), JSON.stringify(devArtifact));
    const unrelated = renderRustCi(withSource(devArtifact, { macSignManifest: "build/dev-sign.json", macPayloadMetadata: "payload.json" }));
    assert.equal(unrelated, plain, "signExisting without the key changes nothing");
  }
});

test("macPayloadMetadataSource zips the .app and the payload metadata file, keeping the wrapper layout, in every macOS leg", () => {
  const legs = [
    DEV_MANIFEST.devArtifact,
    { mac: "unsigned", macAppPath: "target/release/bundle/macos/Pulse.app" },
    { ...DEV_MANIFEST.devArtifact, parallel: true, buildScript: "build:dev" },
  ];
  for (const devArtifact of legs) {
    const out = renderRustCi(withSource(devArtifact));
    assert.ok(!out.includes(KEEP_PARENT), "the plain keepParent zip is replaced");
    assert.match(out, /test -f "dist\/dev-macos\/payload\.json"\n          stage="\$RUNNER_TEMP\/dev-unsigned-stage"\n          rm -rf "\$stage" && mkdir -p "\$stage"\n          ditto "\$DEV_APP_PATH" "\$stage\/\$\(basename "\$DEV_APP_PATH"\)"\n          cp "dist\/dev-macos\/payload\.json" "\$stage\/payload\.json"\n          ditto -c -k "\$stage" "\$RUNNER_TEMP\/dev-unsigned\/app\.zip"\n/, JSON.stringify(devArtifact));
    assert.match(out, /set -euo pipefail\n          test -d "\$DEV_APP_PATH"\n          mkdir -p "\$RUNNER_TEMP\/dev-unsigned"\n          test -f /, "the step fails on a missing file under set -e, after the app check");
  }
});

test("macPayloadMetadataSource validation", () => {
  const check = (devArtifact, signExisting) => validateManifest({ ...withSource(devArtifact, signExisting), regenerate: undefined });
  const dev = { ...DEV_MANIFEST.devArtifact };
  assert.equal(check(dev, WITH_SOURCE).valid, true);
  assert.equal(check(dev, { ...WITH_SOURCE, macPayloadMetadataSource: "../payload.json" }).valid, false, "safe path");
  assert.equal(check(dev, { ...WITH_SOURCE, macPayloadMetadataSource: "/abs/payload.json" }).valid, false, "repository-relative");
  assert.equal(check(dev, { ...WITH_SOURCE, macPayloadMetadataSource: 7 }).valid, false, "a string");
  assert.equal(check(dev, { ...WITH_SOURCE, macPayloadMetadata: "Legion.app/Contents/Resources/payload.json" }).valid, false, "bare file name at the zip root");
  assert.equal(check({ windows: "unsigned", windowsGlob: "dist/*.exe" }, WITH_SOURCE).valid, false, "needs a macOS dev leg");
  assert.equal(check(dev, { macSignManifest: "build/dev-sign.json", macPayloadMetadata: "Legion.app/Contents/Resources/payload.json" }).valid, true, "unset keeps any metadata path");
});

test("signExisting validation", () => {
  const check = (patch) => validateManifest({ ...SIGN_EXISTING_MANIFEST, regenerate: undefined, ...patch });
  assert.equal(check({}).valid, true);
  assert.equal(check({ signExisting: undefined }).valid, false, "lane needs config");
  assert.equal(check({ lanes: ["ci"] }).valid, false, "config needs lane");
  assert.equal(check({ signExisting: { macSignManifest: "build/dev-sign.json" } }).valid, false, "metadata required");
  assert.equal(check({ signExisting: { macPayloadMetadata: "payload.json" } }).valid, false, "sign manifest required");
  assert.equal(check({ signExisting: { ...SIGN_EXISTING_MANIFEST.signExisting, macSignManifest: "../x.json" } }).valid, false, "safe path");
  assert.equal(check({ signExisting: { ...SIGN_EXISTING_MANIFEST.signExisting, environment: "a b" } }).valid, false, "safe environment");
  assert.equal(check({ signExisting: { ...SIGN_EXISTING_MANIFEST.signExisting, release: true } }).valid, false, "no unknown fields, so no release mode");
  assert.equal(check({ matrix: { os: ["windows-2025"], node: ["26.8.1"] } }).valid, false, "needs a macOS runner");
  assert.equal(validateManifest({ ...SIGN_EXISTING_MANIFEST, profile: undefined, gate: undefined, ciAllOs: undefined, regenerate: undefined }).valid, false, "rust-hybrid only");
});

test("rendered ciIncremental + devArtifact workflows pass actionlint", (t) => {
  const bin = spawnSync("actionlint", ["-version"], { encoding: "utf8" });
  if (bin.error) return t.skip("actionlint not installed");
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "right-git-actionlint-"));
  try {
    fs.mkdirSync(path.join(dir, ".github", "workflows"), { recursive: true });
    fs.writeFileSync(path.join(dir, ".github", "actionlint.yaml"), "self-hosted-runner:\n  labels:\n    - xcode-*\n");
    const samples = {
      "ci-default.yml": INCREMENTAL_BASE,
      "ci-incremental.yml": { ...INCREMENTAL_BASE, ciIncremental: true },
      "ci-dev.yml": { ...DEV_MANIFEST, ciIncremental: true },
      "ci-dev-entitlements.yml": { ...DEV_MANIFEST, devArtifact: { ...DEV_MANIFEST.devArtifact, macEntitlements: "build/pulse.entitlements" } },
      "ci-dev-unsigned-dispatch.yml": { ...DEV_MANIFEST, devArtifact: { ...DEV_MANIFEST.devArtifact, mac: "unsigned", dispatch: true } },
      "ci-parallel-dev.yml": { ...DEV_MANIFEST, devArtifact: { ...DEV_MANIFEST.devArtifact, parallel: true, buildScript: "build:dev" } },
      "ci-parallel-unsigned-dispatch.yml": {
        ...INCREMENTAL_BASE,
        rustCache: { workspaces: ["engine"], keySuffix: "v2" },
        concurrency: { mainPushes: "queue", reason: "actionlint sample of the queue opt-out" },
        devArtifact: { mac: "unsigned", windows: "unsigned", dispatch: true, parallel: true, buildScript: "build:dev", macAppPath: "a/B.app", windowsGlob: "dist/*.exe" },
      },
    };
    const files = [];
    for (const [name, manifest] of Object.entries(samples)) {
      const file = path.join(dir, ".github", "workflows", name);
      fs.writeFileSync(file, renderRustCi(manifest));
      files.push(file);
    }
    for (const [name, template] of [["regenerate-rust.yml", "profiles/rust-hybrid/regenerate.yml"], ["regenerate-node.yml", "regenerate.yml"]]) {
      const manifest = template.startsWith("profiles/")
        ? { ...INCREMENTAL_BASE, lanes: ["ci", "regenerate"], regenerate: { script: "regenerate" } }
        : { ...FULL_MANIFEST, lanes: ["ci", "regenerate"], regenerate: { script: "regenerate" } };
      const file = path.join(dir, ".github", "workflows", name);
      fs.writeFileSync(file, renderTemplate(fs.readFileSync(path.join(templatesDir, template), "utf8"), buildRenderContext(manifest, "/repos/widgets", "regenerate")));
      files.push(file);
    }
    const result = spawnSync("actionlint", ["-shellcheck=", "-pyflakes=", "-config-file", path.join(dir, ".github", "actionlint.yaml"), ...files], { encoding: "utf8" });
    assert.equal(result.status, 0, result.stdout + result.stderr);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

// rustCache.keySuffix: unset renders no key (byte-identical); set renders `<lane>-<suffix>`
// in every rust-cache step, and the dev-artifact jobs get their own key.
const RELEASE_CANDIDATE_MANIFEST = {
  ...FULL_MANIFEST,
  lanes: ["ci", "release-candidate"],
  profile: "rust-hybrid",
  gate: { path: "scripts/gate.sh" },
  regenerate: undefined,
  candidate: {
    buildScript: "ci:candidate",
    checkScript: "ci:candidate-check",
    admissionScript: "release:admit",
    stageSummaryScript: "release:stage-summary",
    artifactRoot: "widgets-unsigned",
    os: ["macos-latest", "windows-latest"],
  },
  templateVersion: "1.2.0",
};
const VALID_RUST_HYBRID = { ...INCREMENTAL_BASE, regenerate: undefined };

test("rustCache.keySuffix: unset adds no key; set renders a lane-scoped key in every rust-cache step", () => {
  const unset = renderRustCi({ ...INCREMENTAL_BASE, rustCache: { workspaces: ["engine"] } });
  assert.doesNotMatch(unset, /add-job-id-key: false\n          key: /);
  assert.equal(renderRustCi({ ...INCREMENTAL_BASE, rustCache: { workspaces: ["engine"] } }), renderRustCi({ ...INCREMENTAL_BASE, rustCache: { workspaces: ["engine"], keySuffix: undefined } }));

  const set = renderRustCi({ ...INCREMENTAL_BASE, rustCache: { workspaces: ["engine"], keySuffix: "v2" } });
  assert.match(set, /          shared-key: pulse-rust\n          add-job-id-key: false\n          key: ci-v2\n          cache-workspace-crates: /);
  assert.equal(set.replace("          key: ci-v2\n", ""), unset, "the key line is the only difference");

  const withDev = renderRustCi({ ...DEV_MANIFEST, rustCache: { workspaces: ["engine"], keySuffix: "v2" }, devArtifact: { ...DEV_MANIFEST.devArtifact, parallel: true, buildScript: "build:dev" } });
  assert.match(withDev, /dev-artifact-mac:[\s\S]*?          key: ci-dev-artifact-mac-v2\n/);
  assert.match(withDev, /dev-artifact-windows:[\s\S]*?          key: ci-dev-artifact-windows-v2\n/);

  const lanes = [
    ["regenerate", { ...INCREMENTAL_BASE, lanes: ["ci", "regenerate"], regenerate: { script: "regenerate" }, rustCache: { workspaces: ["engine"], keySuffix: "v2" } }, "profiles/rust-hybrid/regenerate.yml"],
    ["release-candidate", { ...RELEASE_CANDIDATE_MANIFEST, rustCache: { workspaces: ["engine"], keySuffix: "v2" } }, "profiles/rust-hybrid/release-candidate.yml"],
  ];
  for (const [lane, manifest, template] of lanes) {
    const out = renderTemplate(fs.readFileSync(path.join(templatesDir, template), "utf8"), buildRenderContext(structuredClone(manifest), "/repos/pulse", lane));
    assert.match(out, new RegExp(`          add-job-id-key: false\n          key: ${lane}-v2\n`), lane);
  }
  const unkeyed = renderTemplate(fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/release-candidate.yml"), "utf8"), buildRenderContext(structuredClone(RELEASE_CANDIDATE_MANIFEST), "/repos/pulse", "release-candidate"));
  assert.doesNotMatch(unkeyed, /add-job-id-key: false\n          key: /);

  for (const bad of ["${{ secrets.X }}", "has space", "-leading", "semi;colon", "a".repeat(65), 7]) {
    assert.equal(validateManifest({ ...VALID_RUST_HYBRID, rustCache: { workspaces: ["engine"], keySuffix: bad } }).valid, false, String(bad));
  }
  assert.equal(validateManifest({ ...VALID_RUST_HYBRID, rustCache: { workspaces: ["engine"], keySuffix: "v2" } }).valid, true);
  assert.equal(validateManifest({ ...FULL_MANIFEST, lanes: ["ci"], rustCache: { workspaces: ["engine"], keySuffix: "v2" } }).valid, false, "rustCache stays rust-hybrid only");
});

// One CI run per repo (#338). Default: a `supersede` job cancels older default-branch runs that only just started
// or are failing; an older run past the grace fraction is kept and the new run's ci job waits for it through a
// job-level group with cancel-in-progress false. "queue" (with a required reason) restores the old per-ref group
// and renders no supersede job. Render outputs are compared structurally here, never by baseline hash: no hashed
// baseline (SW_BASELINE) renders a rust-hybrid or node ci lane, so no hash changed.
const MAIN_PUSH_EXPRESSION = "${{ !(github.event_name == 'push' && github.ref_name == github.event.repository.default_branch) }}";
const QUEUE_REASON = "deploy ordering needs every main commit built in turn";
const renderGenericCi = (manifest) => renderTemplate(fs.readFileSync(path.join(templatesDir, "ci.yml"), "utf8"), buildRenderContext({ ...FULL_MANIFEST, lanes: ["ci"], ...manifest }, "/repos/widgets", "ci"));

test("default ci renders the supersede job and a job-level wait group instead of cancelling main pushes", () => {
  const unset = renderRustCi(INCREMENTAL_BASE);
  assert.equal(renderRustCi({ ...INCREMENTAL_BASE, concurrency: { mainPushes: "cancel" } }), unset);
  assert.equal(renderRustCi({ ...INCREMENTAL_BASE, concurrency: {} }), unset);
  for (const content of [unset, renderGenericCi({})]) {
    assert.match(content, /^concurrency:\n  group: \$\{\{ github\.workflow \}\}-\$\{\{ github\.event_name == 'push' && github\.ref_name == github\.event\.repository\.default_branch && github\.run_id \|\| github\.ref \}\}\n/m, "a main push has its own workflow group, so its supersede job always runs");
    assert.ok(content.includes(`  cancel-in-progress: ${MAIN_PUSH_EXPRESSION}\n`), "PR runs still cancel superseded runs, main pushes are never cancelled by the workflow group");
    assert.match(content, /\n  supersede:\n    needs: \[private-repo-guard\]\n    if: \$\{\{ github\.event_name == 'push' && github\.ref_name == github\.event\.repository\.default_branch \}\}\n    runs-on: ubuntu-24\.04\n    timeout-minutes: 5\n    permissions:\n      contents: read\n      actions: write\n    steps:\n/);
    assert.equal((content.match(/actions: write/g) ?? []).length, 1, "actions: write only in supersede");
    assert.match(content, /RIGHT_GIT_GRACE_FRACTION: "0\.7"\n          RIGHT_GIT_MEDIAN_WINDOW: "10"\n/);
    assert.match(content, /cat > "\$RUNNER_TEMP\/supersede\.mjs" <<'RIGHT_GIT_SUPERSEDE_EOF'\n/);
    assert.match(content, /\n          RIGHT_GIT_SUPERSEDE_EOF\n          node "\$RUNNER_TEMP\/supersede\.mjs"\n/);
    assert.match(content, /\n    concurrency:\n      group: \$\{\{ github\.event_name == 'push' && github\.ref_name == github\.event\.repository\.default_branch && format\('\{0\}-main-\{1\}-\{2\}', github\.workflow, matrix\.os, matrix\.node\) \|\| format\('\{0\}-\{1\}-\{2\}-\{3\}-\{4\}', github\.workflow, github\.run_id, github\.run_attempt, matrix\.os, matrix\.node\) \}\}\n      cancel-in-progress: false\n/);
    assert.doesNotMatch(content, /concurrency\.mainPushes queue/);
  }
  // The embedded script is the shipped file, verbatim.
  const script = fs.readFileSync(new URL("./supersede.mjs", import.meta.url), "utf8").replace(/\r\n?/g, "\n").replace(/\n+$/, "");
  assert.ok(unset.includes(script.split("\n").map((line) => (line === "" ? "" : `          ${line}`)).join("\n")));
});

test("concurrency.graceFraction and medianWindow reach the supersede job and are validated", () => {
  const custom = renderRustCi({ ...INCREMENTAL_BASE, concurrency: { graceFraction: 0.5, medianWindow: 5 } });
  assert.match(custom, /RIGHT_GIT_GRACE_FRACTION: "0\.5"\n          RIGHT_GIT_MEDIAN_WINDOW: "5"\n/);
  for (const bad of [{ graceFraction: 0 }, { graceFraction: 1.5 }, { graceFraction: "0.7" }, { medianWindow: 0 }, { medianWindow: 101 }, { medianWindow: 2.5 }]) {
    assert.equal(validateManifest({ ...VALID_RUST_HYBRID, concurrency: bad }).valid, false, JSON.stringify(bad));
  }
  assert.equal(validateManifest({ ...VALID_RUST_HYBRID, concurrency: { graceFraction: 1, medianWindow: 100 } }).valid, true);
  assert.equal(validateManifest({ ...VALID_RUST_HYBRID, concurrency: { mainPushes: "queue", reason: QUEUE_REASON, graceFraction: 0.5 } }).valid, false, "queue renders no supersede job");
});

test("concurrency.mainPushes: 'queue' with a reason keeps the per-ref group, renders no supersede job and records the reason", () => {
  const unset = renderRustCi(INCREMENTAL_BASE);
  const queue = renderRustCi({ ...INCREMENTAL_BASE, concurrency: { mainPushes: "queue", reason: QUEUE_REASON } });
  assert.match(queue, new RegExp(`^# concurrency\\.mainPushes queue, recorded exception to one CI run per repo: ${QUEUE_REASON}\\nconcurrency:\\n  group: \\$\\{\\{ github\\.workflow \\}\\}-\\$\\{\\{ github\\.ref \\}\\}\\n`, "m"));
  assert.ok(queue.includes(`  cancel-in-progress: ${MAIN_PUSH_EXPRESSION}\n`));
  assert.doesNotMatch(queue, /supersede|actions: write|\n    concurrency:/);
  assert.notEqual(queue, unset);

  const generic = renderGenericCi({ concurrency: { mainPushes: "queue", reason: QUEUE_REASON } });
  assert.match(generic, /^concurrency:\n  group: \$\{\{ github\.workflow \}\}-\$\{\{ github\.ref \}\}\n/m);
  assert.doesNotMatch(generic, /supersede|actions: write/);
  assert.match(generic, /^# concurrency\.mainPushes queue, recorded exception/m);

  const regenerate = renderTemplate(
    fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/regenerate.yml"), "utf8"),
    buildRenderContext({ ...INCREMENTAL_BASE, lanes: ["ci", "regenerate"], regenerate: { script: "regenerate" }, concurrency: { mainPushes: "queue", reason: QUEUE_REASON } }, "/repos/pulse", "regenerate"),
  );
  assert.match(regenerate, /  cancel-in-progress: false\n/, "the regenerate lane is never cancelled");

  assert.equal(validateManifest({ ...VALID_RUST_HYBRID, concurrency: { mainPushes: "queue", reason: QUEUE_REASON } }).valid, true);
  assert.equal(validateManifest({ ...VALID_RUST_HYBRID, concurrency: { mainPushes: "queue" } }).valid, false, "queue needs a reason");
  assert.equal(validateManifest({ ...VALID_RUST_HYBRID, concurrency: { mainPushes: "queue", reason: "" } }).valid, false);
  assert.equal(validateManifest({ ...VALID_RUST_HYBRID, concurrency: { mainPushes: "queue", reason: "   " } }).valid, false);
  assert.equal(validateManifest({ ...VALID_RUST_HYBRID, concurrency: { mainPushes: "queue", reason: 5 } }).valid, false);
  assert.equal(validateManifest({ ...VALID_RUST_HYBRID, concurrency: { mainPushes: "queue", reason: "two\nlines" } }).valid, false, "the reason is one YAML comment line");
  assert.equal(validateManifest({ ...VALID_RUST_HYBRID, concurrency: { mainPushes: "cancel", reason: QUEUE_REASON } }).valid, false, "a reason only goes with queue");
  assert.equal(validateManifest({ ...VALID_RUST_HYBRID, concurrency: { reason: QUEUE_REASON } }).valid, false);
  assert.equal(validateManifest({ ...VALID_RUST_HYBRID, concurrency: { mainPushes: "bogus" } }).valid, false);
  assert.equal(validateManifest({ ...VALID_RUST_HYBRID, concurrency: { mainPushes: "queue", reason: QUEUE_REASON, extra: true } }).valid, false);
  assert.equal(validateManifest({ ...VALID_RUST_HYBRID, concurrency: "queue" }).valid, false);
  assert.equal(validateManifest({ ...FULL_MANIFEST, lanes: ["package-smoke"], regenerate: undefined, concurrency: { mainPushes: "queue", reason: QUEUE_REASON } }).valid, false, "concurrency needs lane ci");
});

// devArtifact.parallel: each platform builds and stages the dev app in its own job beside the gate.
// The gate job renders exactly as it does without devArtifact; parallel: false is the in-gate default.
const PARALLEL_DEV = {
  ...VALID_RUST_HYBRID,
  ciAllOs: undefined,
  devArtifact: { ...DEV_MANIFEST.devArtifact, parallel: true, buildScript: "build:dev" },
};

test("devArtifact.parallel renders per-platform dev-artifact jobs beside the gate; the gate job is unchanged", () => {
  const inGate = renderRustCi(DEV_MANIFEST);
  const parallel = renderRustCi(PARALLEL_DEV);

  const gateJobs = parallel.slice(0, parallel.indexOf("\n  dev-artifact-mac:"));
  assert.equal(gateJobs, renderRustCi({ ...PARALLEL_DEV, devArtifact: undefined }), "the gate job matches a manifest with no devArtifact");
  assert.doesNotMatch(gateJobs, /Build dev app|dev-windows-|dev-mac-unsigned|dev-unsigned-mac|Package unsigned dev app|DEV_APP_PATH/);

  const devJobs = parallel.slice(parallel.indexOf("\n  dev-artifact-mac:"), parallel.indexOf("\n  dev-mac-sign:"));
  assert.match(devJobs, /\n  dev-artifact-mac:\n    # macOS dev app[^\n]*\n[^\n]*\n    needs: \[private-repo-guard, check\]\n    if: \$\{\{ github\.event_name == 'push' && github\.ref_name == github\.event\.repository\.default_branch \}\}\n    runs-on: xcode-27\n/);
  assert.match(devJobs, /\n  dev-artifact-windows:\n    # Windows dev app[^\n]*\n[^\n]*\n    needs: \[private-repo-guard, check\]\n    if: \$\{\{ github\.event_name == 'push' && github\.ref_name == github\.event\.repository\.default_branch \}\}\n    runs-on: windows-2025\n/);
  assert.equal((devJobs.match(/        run: pnpm run build:dev\n/g) ?? []).length, 2, "each platform job compiles devArtifact.buildScript");
  assert.doesNotMatch(devJobs, /secrets\./, "the dev-artifact jobs never reach a secret");
  assert.match(devJobs, /name: dev-unsigned-mac-\$\{\{ github\.sha \}\}\n          path: \$\{\{ runner\.temp \}\}\/dev-unsigned\/app\.zip\n          retention-days: 1/);
  assert.match(devJobs, /name: dev-windows-\$\{\{ github\.sha \}\}\n          path: target\/release\/\*\.exe\n          retention-days: 5/);

  assert.match(parallel, /\n  dev-mac-sign:\n    # Push to the default branch only[^\n]*\n[^\n]*\n    needs: \[ci, dev-artifact-mac\]\n/);
  assert.match(inGate, /\n  dev-mac-sign:\n[\s\S]*?    needs: \[ci\]\n/, "the in-gate signing job still waits only for ci");
  assert.equal(renderRustCi({ ...DEV_MANIFEST, devArtifact: { ...DEV_MANIFEST.devArtifact, parallel: false } }), inGate, "parallel: false is the in-gate default");
  assert.doesNotMatch(inGate, /dev-artifact-mac:|dev-artifact-windows:/);
  assert.doesNotMatch(parallel.slice(0, parallel.indexOf("\n  dev-artifact-mac:")), /dev-artifact/);
});

test("devArtifact.parallel: unsigned legs keep manual dispatch and the signing job stays push-only", () => {
  const out = renderRustCi({
    ...VALID_RUST_HYBRID,
    devArtifact: { mac: "unsigned", windows: "unsigned", dispatch: true, parallel: true, buildScript: "build:dev", macAppPath: "a/B.app", windowsGlob: "*.exe" },
  });
  const cond = "\\(github\\.event_name == 'push' && github\\.ref_name == github\\.event\\.repository\\.default_branch \\|\\| github\\.event_name == 'workflow_dispatch'\\)";
  assert.match(out, /^on:\n  pull_request:\n  push:\n    branches: \[main\]\n  workflow_dispatch:\npermissions:/m);
  assert.match(out, new RegExp(`dev-artifact-mac:\\n[\\s\\S]*?    if: \\$\\{\\{ ${cond} \\}\\}\\n    runs-on: xcode-27`));
  assert.match(out, new RegExp(`dev-artifact-windows:\\n[\\s\\S]*?    if: \\$\\{\\{ ${cond} \\}\\}\\n    runs-on: windows-2025`));
  assert.doesNotMatch(out, /dev-mac-sign/);
});

test("devArtifact.parallel validates its own runners, buildScript and flag; ciAllOs is not required", () => {
  assert.equal(validateManifest(PARALLEL_DEV).valid, true, JSON.stringify(validateManifest(PARALLEL_DEV).errors));
  assert.equal(validateManifest({ ...PARALLEL_DEV, ciAllOs: true }).valid, true);
  const dev = PARALLEL_DEV.devArtifact;
  assert.equal(validateManifest({ ...PARALLEL_DEV, devArtifact: { ...dev, buildScript: undefined } }).valid, false, "parallel needs buildScript");
  assert.equal(validateManifest({ ...PARALLEL_DEV, devArtifact: { ...dev, buildScript: "build dev" } }).valid, false, "buildScript charset");
  assert.equal(validateManifest({ ...PARALLEL_DEV, devArtifact: { ...dev, parallel: undefined } }).valid, false, "buildScript requires parallel");
  assert.equal(validateManifest({ ...PARALLEL_DEV, devArtifact: { ...dev, parallel: "yes" } }).valid, false, "parallel is a boolean");
  assert.equal(validateManifest({ ...PARALLEL_DEV, matrix: { os: ["windows-2025"], node: ["26.8.1"] } }).valid, false, "mac sign needs a macOS runner in matrix.os");
  assert.equal(validateManifest({ ...PARALLEL_DEV, matrix: { os: ["xcode-27"], node: ["26.8.1"] }, devArtifact: { mac: "unsigned", windows: "unsigned", parallel: true, buildScript: "build:dev", macAppPath: "a/B.app", windowsGlob: "*.exe" } }).valid, false, "windows unsigned needs a windows runner");
});

// swift.privatePackages: read access to a private Swift package mirror from the ci job.
// The configure step sits right before the first build or test step; the cleanup step
// runs always, after the gate. The secret is referenced only from step env, never a run script.
const SWIFT_ONE = [{ host: "github.com", secret: "RIGHTKIT_SWIFT_TOKEN" }];
const NODE_CI_BASE = { schemaVersion: 1, compile: "github-actions-only", lanes: ["ci"], matrix: { os: ["ubuntu-latest"], node: ["24"] }, packageManager: "pnpm@11.25.0", templateVersion: "1.0.0" };
const renderNodeCi = (manifest) => renderTemplate(
  fs.readFileSync(path.join(templatesDir, "ci.yml"), "utf8"),
  buildRenderContext(structuredClone(manifest), "/repos/widgets"),
);
const CONFIGURE_HEAD = "      - name: Configure private Swift package access\n";
const CLEANUP_HEAD = "      - name: Remove private Swift package access\n";
// The rendered run body of a step, with the 10-space YAML block indent removed.
const stepScript = (step) => step.slice(step.indexOf("        run: |\n") + "        run: |\n".length).split("\n").map((line) => line.slice(10)).join("\n");

test("swift.privatePackages unset renders nothing new in either ci template", () => {
  for (const [render, base] of [[renderRustCi, INCREMENTAL_BASE], [renderNodeCi, NODE_CI_BASE]]) {
    const unset = render(base);
    assert.doesNotMatch(unset, /private Swift package|\.netrc|insteadOf|RIGHT_GIT_PRIVATE_TOKEN/);
    assert.equal(render({ ...base, swift: undefined }), unset);
  }
});

test("rust-hybrid ci: configure follows the toolchain and precedes the gate; cleanup runs always after it", () => {
  const out = renderRustCi({ ...INCREMENTAL_BASE, swift: { privatePackages: SWIFT_ONE } });
  const configure = out.indexOf(CONFIGURE_HEAD);
  const gate = out.indexOf("      - run: pnpm exec rightkit-gate --to compile\n");
  const cleanup = out.indexOf(CLEANUP_HEAD);
  assert.ok(out.indexOf("Swatinem/rust-cache") < configure && configure < gate && gate < cleanup, "order: toolchain, configure, gate, cleanup");
  assert.match(out.slice(cleanup), /        if: \$\{\{ always\(\) \}\}\n/);
  assert.equal(out.slice(0, configure).includes("RIGHT_GIT_PRIVATE_TOKEN"), false);
  assert.equal(out.match(/\$\{\{ secrets\.RIGHTKIT_SWIFT_TOKEN \}\}/g)?.length, 2, "one env reference per step");
  assert.equal(out.match(/^          RIGHT_GIT_PRIVATE_TOKEN_0: \$\{\{ secrets\.RIGHTKIT_SWIFT_TOKEN \}\}$/gm)?.length, 2);
  assert.doesNotMatch(stepScript(out.slice(configure, gate)), /secrets\./);
});

test("node ci: configure follows install and precedes the tests; cleanup is the last step", () => {
  const out = renderNodeCi({ ...NODE_CI_BASE, swift: { privatePackages: SWIFT_ONE } });
  const install = out.indexOf("      - run: pnpm install");
  const configure = out.indexOf(CONFIGURE_HEAD);
  const firstTest = out.search(/      - run: pnpm (?:run )?test\n/);
  const cleanup = out.indexOf(CLEANUP_HEAD);
  assert.ok(install > 0 && install < configure && configure < firstTest && firstTest < cleanup, "order: install, configure, tests, cleanup");
  assert.ok(out.indexOf("check-network-boundary") < cleanup);
  assert.match(out, /          fi\n$/);
});

test("swift.privatePackages renders one machine entry and one job-scoped rewrite per host", () => {
  const out = renderRustCi({ ...INCREMENTAL_BASE, swift: { privatePackages: [...SWIFT_ONE, { host: "git.example.com", secret: "SECOND_SWIFT_TOKEN" }] } });
  assert.ok(out.includes(`'git.example.com' "$RIGHT_GIT_PRIVATE_TOKEN_1"`));
  assert.ok(out.includes(`git config --global "url.https://x-access-token:$RIGHT_GIT_PRIVATE_TOKEN_1@git.example.com/.insteadOf" "https://git.example.com/"`));
  assert.ok(out.includes(`git config --global --remove-section "url.https://x-access-token:$RIGHT_GIT_PRIVATE_TOKEN_1@git.example.com/" || true`));
  assert.equal(out.match(/printf 'machine/g)?.length, 2);
});

test("swift.privatePackages validation refuses unsafe names, hosts and duplicates, and placements it cannot serve", () => {
  const withSwift = (swift, base = VALID_RUST_HYBRID) => validateManifest({ ...base, swift }).errors;
  assert.deepEqual(validateManifest({ ...VALID_RUST_HYBRID, swift: { privatePackages: SWIFT_ONE } }).errors, []);
  assert.deepEqual(validateManifest({ ...NODE_CI_BASE, swift: { privatePackages: SWIFT_ONE } }).errors, []);
  for (const secret of ["GITHUB_TOKEN", "github_token", "1BAD", "A B", "A}}", "${{ secrets.X }}", ""]) {
    assert.ok(withSwift({ privatePackages: [{ host: "github.com", secret }] }).some((e) => /GitHub Actions secret name/.test(e)), `secret ${secret}`);
  }
  for (const host of ["github.com/evil", "-bad.example", "https://github.com", "git hub.com"]) {
    assert.ok(withSwift({ privatePackages: [{ host, secret: "A_B" }] }).some((e) => /bare host name/.test(e)), `host ${host}`);
  }
  assert.ok(withSwift({ privatePackages: [SWIFT_ONE[0], SWIFT_ONE[0]] }).some((e) => /must be unique/.test(e)));
  assert.ok(withSwift({ privatePackages: [] }).some((e) => /1 to 4/.test(e)));
  assert.ok(withSwift({ privatePackages: Array.from({ length: 5 }, (_, i) => ({ host: `h${i}.example`, secret: `S${i}` })) }).some((e) => /1 to 4/.test(e)));
  assert.ok(withSwift({ privatePackages: [{ host: "github.com", secret: "A", extra: 1 }] }).some((e) => /only host and secret/.test(e)));
  assert.ok(withSwift({ privatePackages: SWIFT_ONE, other: 1 }).some((e) => /unknown swift field: other/.test(e)));
  assert.ok(validateManifest({ ...NODE_CI_BASE, lanes: ["publish-npm"], publish: { npm: { oidc: true, provenance: true } }, swift: { privatePackages: SWIFT_ONE } }).errors.some((e) => /requires one of the lanes 'ci', 'qualification', 'release-candidate', 'regenerate'/.test(e)));
  assert.ok(validateManifest({ ...VALID_RUST_HYBRID, devArtifact: { mac: "unsigned", dispatch: true, macAppPath: "a/B.app" }, swift: { privatePackages: SWIFT_ONE } }).errors.some((e) => /cannot be combined with devArtifact\.dispatch/.test(e)));
});

test("swift.privatePackages configure and cleanup scripts write, refuse and remove only the credentials they own", () => {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "right-git-swift-"));
  try {
    const home = path.join(scratch, "home");
    const runnerTemp = path.join(scratch, "runner-temp");
    fs.mkdirSync(home);
    fs.mkdirSync(runnerTemp);
    const manifest = { ...VALID_RUST_HYBRID, swift: { privatePackages: SWIFT_ONE } };
    const configure = stepScript(swiftPrivatePackagesConfigureStep(manifest));
    const cleanup = stepScript(swiftPrivatePackagesCleanupStep(manifest));
    const run = (script, token) => spawnSync("bash", ["-c", script], {
      encoding: "utf8",
      windowsHide: true,
      env: { PATH: process.env.PATH, HOME: home, XDG_CONFIG_HOME: path.join(home, ".config"), RUNNER_TEMP: runnerTemp, RIGHT_GIT_PRIVATE_TOKEN_0: token },
    });
    const netrc = path.join(home, ".netrc");
    const gitconfig = path.join(home, ".gitconfig");

    // A missing secret fails closed before anything is written.
    assert.notEqual(run(configure, "").status, 0);
    assert.equal(fs.existsSync(netrc), false);

    const configured = run(configure, "placeholder-value");
    assert.equal(configured.status, 0, configured.stderr);
    assert.equal(fs.readFileSync(netrc, "utf8"), "machine github.com\nlogin x-access-token\npassword placeholder-value\n");
    // NTFS has no permission bits for chmod to set; the mode is checked where the workflow runs.
    if (process.platform !== "win32") assert.equal(fs.statSync(netrc).mode & 0o777, 0o600);
    assert.match(fs.readFileSync(gitconfig, "utf8"), /\[url "https:\/\/x-access-token:placeholder-value@github\.com\/"\]\n\tinsteadOf = https:\/\/github\.com\//);

    // A second configure refuses to replace the file it already owns.
    assert.notEqual(run(configure, "placeholder-value").status, 0);

    const cleaned = run(cleanup, "placeholder-value");
    assert.equal(cleaned.status, 0, cleaned.stderr);
    assert.equal(fs.existsSync(netrc), false);
    assert.doesNotMatch(fs.existsSync(gitconfig) ? fs.readFileSync(gitconfig, "utf8") : "", /x-access-token/);

    // Cleanup on a run that never configured leaves a pre-existing netrc alone.
    fs.writeFileSync(netrc, "machine other.example\nlogin a\npassword b\n", { mode: 0o600 });
    assert.equal(run(cleanup, "placeholder-value").status, 0);
    assert.equal(fs.readFileSync(netrc, "utf8"), "machine other.example\nlogin a\npassword b\n");
  } finally {
    fs.rmSync(scratch, { recursive: true, force: true });
  }
});

test("rendered swift.privatePackages workflows pass actionlint", (t) => {
  const bin = spawnSync("actionlint", ["-version"], { encoding: "utf8" });
  if (bin.error) return t.skip("actionlint not installed");
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "right-git-swift-actionlint-"));
  try {
    fs.mkdirSync(path.join(dir, ".github", "workflows"), { recursive: true });
    fs.writeFileSync(path.join(dir, ".github", "actionlint.yaml"), "self-hosted-runner:\n  labels:\n    - xcode-*\n");
    const files = [
      ["swift-rust.yml", renderRustCi({ ...INCREMENTAL_BASE, swift: { privatePackages: SWIFT_ONE } })],
      ["swift-rust-two-hosts.yml", renderRustCi({ ...INCREMENTAL_BASE, swift: { privatePackages: [...SWIFT_ONE, { host: "git.example.com", secret: "SECOND_SWIFT_TOKEN" }] } })],
      ["swift-node.yml", renderNodeCi({ ...NODE_CI_BASE, swift: { privatePackages: SWIFT_ONE } })],
    ].map(([name, text]) => {
      const file = path.join(dir, ".github", "workflows", name);
      fs.writeFileSync(file, text);
      return file;
    });
    const result = spawnSync("actionlint", ["-shellcheck=", "-pyflakes=", "-config-file", path.join(dir, ".github", "actionlint.yaml"), ...files], { encoding: "utf8" });
    assert.equal(result.status, 0, result.stdout + result.stderr);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

// Lane fixtures for the swift.privatePackages tests: one manifest per profile/shape and the
// template each lane renders from. SW_BASELINE pins the unset output to the 0.2.41 renders.
const SW_NODE = {
  schemaVersion: 1,
  compile: "github-actions-only",
  lanes: ["ci", "qualification", "package-smoke", "release-candidate", "regenerate", "publish-npm", "publish-package-managers"],
  matrix: { os: ["ubuntu-latest", "macos-latest"], node: ["24"] },
  packageManager: "pnpm@11.24.0",
  publish: {
    npm: { oidc: true, provenance: true },
    homebrew: { tap: "acme/homebrew-tap", formulaName: "widgets" },
    winget: { packageIdentifier: "Acme.Widgets" },
  },
  regenerate: { script: "regenerate" },
  templateVersion: "1.0.0",
};
const SW_RUST = {
  schemaVersion: 1,
  compile: "github-actions-only",
  profile: "rust-hybrid",
  lanes: ["ci", "release-candidate", "regenerate"],
  gate: { path: "scripts/gate.sh" },
  matrix: { os: ["xcode-27", "windows-2025"], node: ["26.8.1"] },
  packageManager: "pnpm@11.24.0",
  regenerate: { script: "regenerate" },
  candidate: {
    buildScript: "ci:candidate",
    checkScript: "ci:candidate-check",
    admissionScript: "release:admit",
    stageSummaryScript: "release:stage-summary",
    artifactRoot: "widgets-unsigned",
    os: ["xcode-27", "windows-2025"],
  },
  devArtifact: {
    mac: "sign",
    windows: "unsigned",
    parallel: true,
    buildScript: "build:dev",
    macAppPath: "target/release/bundle/macos/Pulse.app",
    windowsGlob: "target/release/*.exe",
  },
  templateVersion: "1.2.0",
};
// Every signing, qualification and publish job the release chain can render.
const SW_RUST_CHAIN = {
  ...SW_RUST,
  lanes: ["ci", "release-candidate"],
  regenerate: undefined,
  devArtifact: { ...SW_RUST.devArtifact, mac: "notarize" },
  matrix: { os: ["ubuntu-24.04", "windows-2025", "macos-15"], node: ["22.23.2"] },
  candidate: {
    buildScript: "ci:candidate",
    checkScript: "ci:candidate-check",
    admissionScript: "release:admission",
    stageSummaryScript: "release:stage-summary",
    artifactRoot: "widgets-unsigned",
    os: ["windows-2025", "macos-15"],
    signWindows: true,
    signMacos: true,
    releaseChain: {
      evidenceVerificationScript: "release:verify:evidence",
      windowsFinalizeScript: "release:finalize:win",
      macosFinalizeScript: "release:finalize:mac",
      installedQualificationScript: "release:qualify:installed",
      publishScript: "release:publish:qualified",
      qualificationRunnerVariable: "QUALIFICATION_RUNNER_LABEL",
      publishSecrets: ["CLOUDFLARE_API_TOKEN"],
    },
  },
};
const SW_CASES = [
  ["node/ci", SW_NODE, "ci"],
  ["node/qualification", SW_NODE, "qualification"],
  ["node/package-smoke", SW_NODE, "package-smoke"],
  ["node/release-candidate", SW_NODE, "release-candidate"],
  ["node/regenerate", SW_NODE, "regenerate"],
  ["node/publish-npm", SW_NODE, "publish-npm"],
  ["node/publish-package-managers", SW_NODE, "publish-package-managers"],
  ["rust-hybrid/ci", SW_RUST, "ci"],
  ["rust-hybrid/release-candidate", SW_RUST, "release-candidate"],
  ["rust-hybrid/regenerate", SW_RUST, "regenerate"],
  ["rust-hybrid-chain/ci", SW_RUST_CHAIN, "ci"],
  ["rust-hybrid-chain/release-candidate", SW_RUST_CHAIN, "release-candidate"],
];
const swRender = (manifest, lane, swift) => {
  const clone = structuredClone(manifest);
  if (swift !== undefined) clone.swift = swift;
  const dir = clone.profile === "rust-hybrid" ? path.join(templatesDir, "profiles", "rust-hybrid") : templatesDir;
  return renderTemplate(fs.readFileSync(path.join(dir, LANE_FILENAMES[lane]), "utf8"), buildRenderContext(clone, "/repos/widgets", lane));
};

// 0.3.1 (rightkit#833, A-11): the lanes that compile or test now render the RightKit check job, so the hashes of those nine
// lanes (publish-npm included: it rebuilds the tarball from the release tag and runs check-release.mjs) were taken again from
// this render, deliberately (a breaking render change; the version bump covers it). The three lanes without a check job
// (regenerate x2, publish-package-managers) keep their hashes. SW_PRE_CHECK_BASELINE keeps the previous nine: the new render
// without the check job still hashes to them.
const SW_BASELINE = {
  "node/ci": "6f541c73a509bc8a16e4d01596f3597de5055a424cb370cf69cde5b683491a4e",
  "node/qualification": "2ffaf14dd3d83d6f8325f2afbeeccb157134fbe0215c86c74d0d081c02cd38b4",
  "node/package-smoke": "f4280607ac420c4d8cc1e09f74304cc5ab7b1b94c879f079c1fdb2b846ab0342",
  "node/release-candidate": "df34b26d2e8f30944fc99bf51dfd1418bf10fac8abaec38a70a85212aa8940e4",
  "node/regenerate": "82bfa3063ae0bc04ba52fecc14d562b418037716283648a887c206683a4e1c14",
  "node/publish-npm": "90bc2bed5f1ad72236623e69ca29f0e5e023c285ab84319e1640cd75d59058bd",
  "node/publish-package-managers": "106b8d28ed76d916afe68808388cd5dc0de0ddd719b566832a6e8280410f9d7f",
  "rust-hybrid/ci": "933f2ae78420cf069adf53df01538134f34f7d22390b97d676cdb21ed8390a7d",
  "rust-hybrid/release-candidate": "92087ffdcb025a6e90b29979fa5fc0e0349b0545838edecbb35d67fd86c6c80c",
  "rust-hybrid/regenerate": "1f25412e0ad5f1d378c76e6f54b615bc4ec13e2bbf85133247557458e1cac966",
  "rust-hybrid-chain/ci": "537e5063dd0bf8ecd9ed95fd2940561655aba954f593d03e05de0b5c6fc36f80",
  "rust-hybrid-chain/release-candidate": "ca8b90de1e52a81ca298953a239233cca449995d3555e315325028b2eb877f53",
};
const SW_PRE_CHECK_BASELINE = {
  // 0.4 (one door): the rust-hybrid ci jobs run `rightkit-gate --to compile` instead of the repository gate script, so their
  // pre-check hashes were retaken; everything else in those renders is unchanged.
  // 0.2.49 (#338): the three ci lanes render the supersede job and job-level concurrency (hashes from the Dell render; the other nine lanes match CI byte for byte).
  "node/ci": "1e44d86e2d9c46f21992ca236da797a7f8751e8622902157fbaa87e28e9b3431",
  "node/qualification": "730cc94fde3400079c2a4e66effed0e8957de2b75576d13f02b9398059ccc921",
  "node/package-smoke": "bc981ccb7248cc022cb8157c1fbdb7895c48f6a3fe4eeaf5a76eb5bba4206517",
  "node/release-candidate": "6ff48f01cadc95e5e92cc225432aa280a7baac46017ae30e54a165e3db2d1539",
  // The 0.2.41 publish-npm render, before the check job (the lane was exempt for one revision; it builds, so it is not).
  "node/publish-npm": "50882207659dfb6e0207fc0d9b33d86b7c10a2d2e3e6c65b2db05d5261982834",
  "rust-hybrid/ci": "e0d89b0ae0ac2f982c94e7b894892c6f22941ef57a3cd3f7feab2d8975b3cde1",
  // 0.2.44/0.2.45: the 0.2.41 render with the admission and regenerate jobs on macos-15 (ownership approval does not run on Linux).
  // 0.2.46 (#168): plus RIGHTKIT_DEV_SIGNING: "ci-adhoc" on the candidate job. Hash taken from CI 38978333900 (xcode and windows agree).
  "rust-hybrid/release-candidate": "8e108084b910f3edd35b197b920b1f5beb7da339f87a1ac90e258fda5d9ea467",
  "rust-hybrid-chain/ci": "5ad16f4a557579c02cf5e4be3684d54ed1a580904b601bc39e6b91edb38133f6",
  // 0.2.46 (#168): the chain candidate job also sets RIGHTKIT_DEV_SIGNING: "ci-adhoc". Hash taken from CI 38986615056 (xcode; the excerpt has no windows).
  "rust-hybrid-chain/release-candidate": "c118e1c555e1de603f61acfde0b4749d519b97ec3815fc53b024b22d1e5cb3b9",
};

// Each lane that builds Swift renders the shared configure and cleanup steps into exactly these jobs.
const SW_EXPECTED_JOBS = {
  "node/ci": ["test"],
  "node/qualification": ["qualification"],
  "node/package-smoke": [],
  "node/release-candidate": ["candidate"],
  "node/regenerate": ["regenerate"],
  "node/publish-npm": [],
  "node/publish-package-managers": [],
  "rust-hybrid/ci": ["ci", "dev-artifact-mac"],
  "rust-hybrid/release-candidate": ["candidate"],
  "rust-hybrid/regenerate": ["regenerate"],
  "rust-hybrid-chain/ci": ["ci", "dev-artifact-mac"],
  "rust-hybrid-chain/release-candidate": ["candidate"],
};
const SW_SECRET_ENV_LINE = /^ {10}RIGHT_GIT_PRIVATE_TOKEN_0: \$\{\{ secrets\.RIGHTKIT_SWIFT_TOKEN \}\}$/;

// jobs: { id -> text } for a rendered workflow.
const swJobs = (workflow) => {
  const body = workflow.slice(workflow.indexOf("\njobs:\n") + "\njobs:\n".length);
  const parts = body.split(/^  ([A-Za-z0-9_-]+):$/m);
  const jobs = {};
  for (let i = 1; i < parts.length; i += 2) jobs[parts[i]] = parts[i + 1];
  return jobs;
};
const swStepNames = (job) => [...job.matchAll(/^ {6}- (?:name: (.+)|(uses|run): (.+))$/gm)].map((m) => m[1] ?? `${m[2]}: ${m[3]}`);
const swSwiftJobs = (workflow) => Object.entries(swJobs(workflow)).filter(([, job]) => job.includes(CONFIGURE_HEAD)).map(([id]) => id);
const SW_SET = { privatePackages: SWIFT_ONE };

test("swift.privatePackages unset: every lane's output is pinned, and without the check job it is the 0.2.41 render", () => {
  for (const [name, manifest, lane] of SW_CASES) {
    const unset = swRender(manifest, lane);
    assert.equal(crypto.createHash("sha256").update(unset).digest("hex"), SW_BASELINE[name], `${name} differs from its pinned render`);
    assert.equal(crypto.createHash("sha256").update(withoutCheckJob(unset)).digest("hex"), SW_PRE_CHECK_BASELINE[name] ?? SW_BASELINE[name], `${name} without the check job differs from the 0.2.41 render`);
    assert.equal(swRender({ ...manifest, swift: undefined }, lane), unset, `${name}: swift: undefined`);
    assert.doesNotMatch(unset, /private Swift package|\.netrc|insteadOf|RIGHT_GIT_PRIVATE_TOKEN/);
  }
  for (const manifest of [SW_NODE, SW_RUST, SW_RUST_CHAIN]) assert.deepEqual(validateManifest(manifest).errors, []);
});

test("swift.privatePackages renders only into the jobs that build Swift; refused lanes and jobs stay byte-identical", () => {
  for (const [name, manifest, lane] of SW_CASES) {
    const out = swRender(manifest, lane, SW_SET);
    assert.deepEqual(swSwiftJobs(out), SW_EXPECTED_JOBS[name], `${name}: jobs carrying the steps`);
    if (SW_EXPECTED_JOBS[name].length === 0) {
      assert.equal(out, swRender(manifest, lane), `${name}: no placeholder, so no change`);
    }
    // The secret is named only by the two env lines per carrying job, never anywhere else.
    const refs = out.split("\n").filter((line) => line.includes("secrets.RIGHTKIT_SWIFT_TOKEN"));
    assert.equal(refs.length, SW_EXPECTED_JOBS[name].length * 2, `${name}: two env references per carrying job`);
    for (const line of refs) assert.match(line, SW_SECRET_ENV_LINE, `${name}: secret outside an env line: ${line}`);
    // Signing, qualification and publish jobs and the Windows dev job never see it.
    for (const [id, job] of Object.entries(swJobs(out))) {
      if (SW_EXPECTED_JOBS[name].includes(id)) continue;
      assert.doesNotMatch(job, /RIGHTKIT_SWIFT_TOKEN|RIGHT_GIT_PRIVATE_TOKEN|Swift package access/, `${name}/${id} must stay secret-free`);
    }
  }
  // The release chain really renders the jobs the argument above excludes.
  const chain = swJobs(swRender(SW_RUST_CHAIN, "release-candidate", SW_SET));
  for (const id of ["admission", "windows-sign", "macos-sign", "installed-qualification", "publish"]) assert.ok(id in chain, `fixture renders ${id}`);
  const chainCi = swJobs(swRender(SW_RUST_CHAIN, "ci", SW_SET));
  for (const id of ["dev-artifact-windows", "dev-mac-sign"]) assert.ok(id in chainCi, `fixture renders ${id}`);
});

test("swift.privatePackages steps: position, always-run cleanup, and the secret only in env, per lane", () => {
  const stepOrder = (out, jobId) => swStepNames(swJobs(out)[jobId]);
  const ORDER = [
    // [case, job, steps in required order]
    ["node/ci", "test", ["Configure private Swift package access", "run: pnpm run test", "run: node scripts/ci/check-network-boundary.mjs", "Remove private Swift package access"]],
    ["node/qualification", "qualification", ["RightKit ownership (protected registry roots)", "Configure private Swift package access", "run: pnpm run test:all", "Remove private Swift package access"]],
    ["node/release-candidate", "candidate", ["RightKit ownership (protected registry roots)", "Configure private Swift package access", "run: node --test tests/release-candidate.test.mjs", "run: node scripts/release/build-candidate.mjs --platform current --out \"${{ runner.temp }}/widgets-rc\"", "run: node scripts/release/check-release.mjs \"${{ runner.temp }}/widgets-rc\"", "Remove private Swift package access", "uses: actions/upload-artifact@v4"]],
    ["node/regenerate", "regenerate", ["RightKit ownership (protected registry roots)", "Configure private Swift package access", "Regenerate derived artifacts", "Test the regenerated tree", "Remove private Swift package access", "Open a pull request with the regenerated files"]],
    ["rust-hybrid/ci", "ci", ["uses: Mozilla-Actions/sccache-action@fc920bf0ec8de6ee65d409111f7ec508035751ba # v0.0.11", "Configure private Swift package access", "run: pnpm exec rightkit-gate --to compile", "Remove private Swift package access", "Record last green commit marker"]],
    ["rust-hybrid/ci", "dev-artifact-mac", ["uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2", "Configure private Swift package access", "Build dev app", "Remove private Swift package access", "Package unsigned dev app (macOS)", "Upload unsigned dev app for signing (macOS)"]],
    ["rust-hybrid/release-candidate", "candidate", ["Initialize candidate stage summary", "Configure private Swift package access", "Build unsigned release candidate", "Check unsigned release candidate", "Remove private Swift package access", "Finalize candidate stage summary", "Upload unsigned candidate handoff"]],
    ["rust-hybrid/regenerate", "regenerate", ["uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2", "Configure private Swift package access", "Regenerate derived artifacts", "Gate the regenerated tree", "Remove private Swift package access", "Open a pull request with the regenerated files"]],
  ];
  for (const [name, jobId, expected] of ORDER) {
    const [, manifest, lane] = SW_CASES.find(([n]) => n === name);
    const out = swRender(manifest, lane, SW_SET);
    const actual = stepOrder(out, jobId);
    let cursor = -1;
    for (const step of expected) {
      const at = actual.findIndex((candidate, index) => index > cursor && (candidate === step || candidate.startsWith(step)));
      assert.ok(at > cursor, `${name}/${jobId}: expected "${step}" after position ${cursor} in ${JSON.stringify(actual)}`);
      cursor = at;
    }
    const job = swJobs(out)[jobId];
    const configure = job.slice(job.indexOf(CONFIGURE_HEAD), job.indexOf(CLEANUP_HEAD));
    const cleanup = job.slice(job.indexOf(CLEANUP_HEAD)).split(/^ {6}- /m)[1];
    assert.match(`      - ${cleanup}`, /^ {6}- name: Remove private Swift package access\n        if: \$\{\{ always\(\) \}\}\n/, `${name}/${jobId}: cleanup always runs`);
    assert.doesNotMatch(configure.split(/^ {6}- /m)[1].split("        run: |\n")[1], /secrets\./, `${name}/${jobId}: no secret expression in the configure run script`);
    // Nothing before the configure step touches the credential.
    assert.equal(job.slice(0, job.indexOf(CONFIGURE_HEAD)).includes("RIGHT_GIT_PRIVATE_TOKEN"), false);
    // Every secret expression in the job is an env value; no run script line contains one.
    let inRun = false;
    for (const line of job.split("\n")) {
      if (/^ {8}run: \|$/.test(line)) { inRun = true; continue; }
      if (inRun && /^ {0,8}\S/.test(line)) inRun = false;
      if (inRun) assert.doesNotMatch(line, /secrets\./, `${name}/${jobId}: secret in a run script`);
      if (line.includes("secrets.")) assert.match(line, SW_SECRET_ENV_LINE);
    }
  }
});

test("regenerate lanes remove the credential before the branch is pushed", () => {
  for (const name of ["node/regenerate", "rust-hybrid/regenerate"]) {
    const [, manifest, lane] = SW_CASES.find(([n]) => n === name);
    const out = swRender(manifest, lane, SW_SET);
    assert.ok(out.indexOf(CLEANUP_HEAD) < out.indexOf("git push origin"), `${name}: cleanup precedes the push`);
    assert.ok(out.indexOf(CONFIGURE_HEAD) < out.indexOf("Regenerate derived artifacts"), `${name}: configure precedes the regenerate script`);
    assert.match(out, /^on:\n  workflow_dispatch:\npermissions:/m);
  }
});

test("rust-hybrid dev-artifact: the Windows job never gets the steps; a Windows-only devArtifact renders nothing new", () => {
  const out = swRender(SW_RUST, "ci", SW_SET);
  const jobs = swJobs(out);
  assert.match(jobs["dev-artifact-mac"], /# QA gate job, not after it, and reaches only the private Swift package secret\./);
  assert.match(jobs["dev-artifact-windows"], /# QA gate job, not after it, and never reaches a secret\./);
  assert.doesNotMatch(jobs["dev-artifact-windows"], /Swift package|RIGHT_GIT_PRIVATE_TOKEN/);
  assert.match(jobs["dev-artifact-mac"], /persist-credentials: false/);
  assert.match(jobs["dev-artifact-mac"], /if: \$\{\{ github\.event_name == 'push' && github\.ref_name == github\.event\.repository\.default_branch \}\}/);
  const windowsOnly = { ...SW_RUST, devArtifact: { windows: "unsigned", parallel: true, buildScript: "build:dev", windowsGlob: "target/release/*.exe" } };
  assert.equal(
    swRender(windowsOnly, "ci", SW_SET).slice(swRender(windowsOnly, "ci", SW_SET).indexOf("\n  dev-artifact-windows:")),
    swRender(windowsOnly, "ci").slice(swRender(windowsOnly, "ci").indexOf("\n  dev-artifact-windows:")),
  );
});

test("every template's triggers are safe for the swift credential: none is pull_request_target or workflow_run", () => {
  const templates = [
    ...fs.readdirSync(templatesDir).filter((f) => f.endsWith(".yml")).map((f) => path.join(templatesDir, f)),
    ...fs.readdirSync(path.join(templatesDir, "profiles", "rust-hybrid")).filter((f) => f.endsWith(".yml")).map((f) => path.join(templatesDir, "profiles", "rust-hybrid", f)),
  ];
  assert.equal(templates.length, 13);
  for (const file of templates) {
    const onBlock = fs.readFileSync(file, "utf8").split("\njobs:")[0].split(/^on:/m)[1] ?? "";
    assert.doesNotMatch(onBlock.split(/^permissions:/m)[0], /pull_request_target|workflow_run/, path.relative(templatesDir, file));
  }
});

test("rendering the credential steps under pull_request_target or workflow_run is refused", () => {
  const step = swiftPrivatePackagesConfigureStep({ swift: SW_SET });
  for (const trigger of ["pull_request_target:", "workflow_run:\n    workflows: [ci]\n    types: [completed]"]) {
    const template = `name: x\non:\n  ${trigger}\njobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo hi\${SWIFT_PRIVATE_CONFIGURE_STEP}\n`;
    assert.throws(() => renderTemplate(template, { SWIFT_PRIVATE_CONFIGURE_STEP: step }), /refusing to render the private Swift package credential steps/);
    // The same trigger without the credential step is not this guard's business.
    assert.doesNotThrow(() => renderTemplate(template, { SWIFT_PRIVATE_CONFIGURE_STEP: "" }));
  }
  const flow = "name: x\non: [pull_request_target]\njobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo hi${SWIFT_PRIVATE_CONFIGURE_STEP}\n";
  assert.throws(() => renderTemplate(flow, { SWIFT_PRIVATE_CONFIGURE_STEP: step }), /refusing/);
  const safe = "name: x\non:\n  pull_request:\n  workflow_dispatch:\njobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo hi${SWIFT_PRIVATE_CONFIGURE_STEP}\n";
  assert.doesNotThrow(() => renderTemplate(safe, { SWIFT_PRIVATE_CONFIGURE_STEP: step }));
});

test("swift.privatePackages lane validation: needs ci, qualification, release-candidate or regenerate; refuses lanes that never render it", () => {
  const errorsFor = (lanes) => validateManifest({ ...SW_NODE, lanes, regenerate: lanes.includes("regenerate") ? SW_NODE.regenerate : undefined, swift: SW_SET }).errors;
  for (const lane of ["ci", "qualification", "release-candidate", "regenerate"]) {
    assert.deepEqual(errorsFor([lane]), [], lane);
  }
  for (const lanes of [["package-smoke"], ["publish-npm"], ["publish-package-managers"]]) {
    assert.ok(errorsFor(lanes).some((e) => /requires one of the lanes/.test(e)), lanes.join());
  }
});

test("rendered swift.privatePackages workflows for every lane pass actionlint", (t) => {
  const bin = spawnSync("actionlint", ["-version"], { encoding: "utf8" });
  if (bin.error) return t.skip("actionlint not installed");
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "right-git-swift-lanes-"));
  try {
    fs.mkdirSync(path.join(dir, ".github", "workflows"), { recursive: true });
    fs.writeFileSync(path.join(dir, ".github", "actionlint.yaml"), "self-hosted-runner:\n  labels:\n    - xcode-*\n");
    const files = SW_CASES.flatMap(([name, manifest, lane]) => {
      const file = path.join(dir, ".github", "workflows", `${name.replace("/", "-")}.yml`);
      fs.writeFileSync(file, swRender(manifest, lane, { privatePackages: [...SWIFT_ONE, { host: "git.example.com", secret: "SECOND_SWIFT_TOKEN" }] }));
      return [file];
    });
    const result = spawnSync("actionlint", ["-shellcheck=", "-pyflakes=", "-config-file", path.join(dir, ".github", "actionlint.yaml"), ...files], { encoding: "utf8" });
    assert.equal(result.status, 0, result.stdout + result.stderr);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

// node release-candidate: the ownership step proves the checked-out commit is the revision the
// candidate was requested for. A dispatch names it through the source_revision input; a v* tag
// push has no input and compares against the pushed commit.
test("node release-candidate declares source_revision, checks it out, and binds the ownership step to it", () => {
  const out = swRender(SW_NODE, "release-candidate");
  assert.match(out, /^on:\n  workflow_dispatch:\n    inputs:\n      source_revision:\n        description: [^\n]+\n        required: true\n        type: string\n  push:\n    tags: \['v\*'\]\npermissions:/m);
  assert.match(out, /- uses: actions\/checkout@v5\n        with:\n          ref: \$\{\{ inputs\.source_revision \|\| github\.sha \}\}\n/);
  assert.match(out, /run: test "\$\(git rev-parse HEAD\)" = "\$\{\{ inputs\.source_revision \|\| github\.sha \}\}" && /);
  assert.doesNotMatch(out, /= "\$\{\{ inputs\.source_revision \}\}"/);
  // The rust-hybrid dispatch-only lane keeps its input-only comparison.
  assert.match(swRender(SW_RUST, "release-candidate"), /run: test "\$\(git rev-parse HEAD\)" = "\$\{\{ inputs\.source_revision \}\}" && /);
  // Other node lanes still compare with the run's own commit.
  assert.match(swRender(SW_NODE, "qualification"), /= "\$\{\{ github\.sha \}\}" && /);
});

test("node release-candidate ownership step passes on both triggers and fails on a revision mismatch", () => {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "right-git-rc-revision-"));
  try {
    const git = (...args) => spawnSync("git", ["-C", scratch, ...args], { encoding: "utf8" });
    git("init", "-q");
    git("-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "--allow-empty", "-m", "one");
    git("-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "--allow-empty", "-m", "two");
    const head = git("rev-parse", "HEAD").stdout.trim();
    const other = git("rev-parse", "HEAD~1").stdout.trim();
    const line = swRender(SW_NODE, "release-candidate").split("\n").find((l) => l.includes("git rev-parse HEAD"));
    const script = line.replace(/^\s*run: /, "").replace(/ && .*$/, "").replace("${{ inputs.source_revision || github.sha }}", "$RC_EXPECTED");
    const check = (inputRevision, sha) => spawnSync("bash", ["-c", script], {
      cwd: scratch,
      encoding: "utf8",
      env: { PATH: process.env.PATH, RC_EXPECTED: inputRevision || sha },
    }).status;
    // workflow_dispatch: the input names the commit that was checked out.
    assert.equal(check(head, other), 0, "dispatch input equals HEAD");
    assert.notEqual(check(other, head), 0, "dispatch input differs from HEAD");
    // v* tag push: no input, so the pushed commit is the expectation.
    assert.equal(check("", head), 0, "tag push at HEAD");
    assert.notEqual(check("", other), 0, "tag push whose commit is not HEAD");
    assert.match(script, /^test "\$\(git rev-parse HEAD\)" = "\$RC_EXPECTED"$/);
  } finally {
    fs.rmSync(scratch, { recursive: true, force: true });
  }
});

test("rust-hybrid: no job that runs the ownership step is hard-coded to a Linux runner", () => {
  const rendered = [];
  const candidate = { ...FULL_MANIFEST, lanes: ["ci", "release-candidate", "regenerate"], profile: "rust-hybrid", gate: { path: "scripts/gate.sh" }, regenerate: { script: "regenerate" }, candidate: { buildScript: "ci:candidate", checkScript: "ci:candidate-check", admissionScript: "release:admit", stageSummaryScript: "release:stage-summary", artifactRoot: "widgets-unsigned", os: ["macos-latest", "windows-latest"] }, templateVersion: "1.2.0" };
  for (const [lane, template] of [["ci", "ci.yml"], ["release-candidate", "release-candidate.yml"], ["regenerate", "regenerate.yml"]]) {
    const content = renderTemplate(fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid", template), "utf8"), buildRenderContext(candidate, "/repos/widgets", lane));
    const jobs = content.split(/^jobs:\n/m)[1].split(/^ {2}(?=[a-z][\w-]*:\n)/m).filter(Boolean);
    for (const job of jobs.filter((j) => /RightKit ownership/.test(j))) {
      rendered.push(`${lane}/${job.split(":")[0]}`);
      assert.doesNotMatch(job, /runs-on: ubuntu|runs-on: \$\{\{[^}]*(ubuntu|linux)/i, `${lane}: a job with the ownership step must not run on Linux`);
    }
  }
  assert.deepEqual(rendered.sort(), ["ci/ci", "regenerate/regenerate", "release-candidate/admission"]);
});

// Orthic-Labs/rightkit#107: optional dev-qualification lane beside a QA-free candidate.
const DEV_QUAL_TEMPLATE = fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/release-candidate.yml"), "utf8");

function devQualManifest(devQualification) {
  return {
    ...FULL_MANIFEST,
    lanes: ["ci", "release-candidate"],
    profile: "rust-hybrid",
    gate: { path: "scripts/gate.sh" },
    candidate: {
      buildScript: "ci:candidate",
      checkScript: "ci:candidate-check",
      admissionScript: "release:admit",
      stageSummaryScript: "release:stage-summary",
      artifactRoot: "widgets-unsigned",
      os: ["windows-2025", "macos-15"],
      targets: [
        { os: "windows-2025", platform: "windows", architecture: "x86_64" },
        { os: "macos-15", platform: "macos", architecture: "arm64" },
      ],
      signWindows: true,
      signMacos: true,
      releaseChain: {
        evidenceVerificationScript: "release:verify-evidence",
        windowsFinalizeScript: "release:finalize-windows",
        macosFinalizeScript: "release:finalize-macos",
        installedQualificationScript: "release:qualify-installed",
        publishScript: "release:publish-qualified",
        qualificationRunnerVariable: "WIDGETS_QUALIFICATION_RUNNER",
      },
      ...(devQualification ? { devQualification } : {}),
    },
    matrix: { os: ["ubuntu-24.04", "windows-2025", "macos-15"], node: ["22.23.2"] },
    templateVersion: "1.8.0",
  };
}

function renderDevQual(manifest) {
  return renderTemplate(DEV_QUAL_TEMPLATE, buildRenderContext(manifest, "/repos/widgets", "release-candidate"));
}

test("dev-qualification unset: release-candidate workflow is byte-identical to the template without the placeholder", () => {
  const manifest = devQualManifest();
  const before = renderTemplate(
    DEV_QUAL_TEMPLATE.replace("${DEV_QUALIFICATION_JOB}${WINDOWS_SIGNING_JOB}", "${WINDOWS_SIGNING_JOB}"),
    buildRenderContext(manifest, "/repos/widgets", "release-candidate"),
  );
  const content = renderDevQual(manifest);
  assert.equal(content, before);
  assert.doesNotMatch(content, /dev-qualification|devqual/);
  assert.match(content, /windows-sign:\n    needs: \[admission, candidate\]\n/);
  assert.match(content, /macos-sign:\n    needs: \[admission, candidate\]\n/);
  // Unsigned manifests (no signing jobs, no chain) stay free of the job too.
  const unsigned = devQualManifest();
  delete unsigned.candidate.releaseChain;
  unsigned.candidate.signWindows = false;
  unsigned.candidate.signMacos = false;
  assert.doesNotMatch(renderDevQual(unsigned), /dev-qualification/);
});

test("dev-qualification set: job needs admission, builds the dev build, uploads devqual evidence and gates both signing jobs", () => {
  const manifest = devQualManifest({ buildScript: "build:dev-qualification", journeyScript: "qa:journeys" });
  const content = renderDevQual(manifest);
  const job = content.slice(content.indexOf("\n  dev-qualification:\n"), content.indexOf("\n  windows-sign:\n"));
  assert.ok(job.length > 0, "dev-qualification job renders before windows-sign");
  assert.match(job, /^\n {2}dev-qualification:\n {4}needs: \[admission\]\n/);
  assert.match(job, /matrix:\n {8}include:\n {10}- os: "windows-2025"[\s\S]*- os: "macos-15"/);
  assert.match(job, /ref: \$\{\{ needs\.admission\.outputs\.source_revision \}\}\n\s+fetch-depth: 0\n\s+persist-credentials: false/);
  assert.match(job, /run: pnpm run build:dev-qualification\n/);
  assert.match(job, /run: pnpm run qa:journeys\n/);
  assert.doesNotMatch(job, /ci:candidate|release:finalize|secrets\.|id-token|environment:/);
  assert.match(job, /RIGHT_GIT_STAGE_ACTION: init[\s\S]*RIGHT_GIT_STAGE: dev-qualification/);
  assert.match(job, /RIGHT_GIT_STAGE_ACTION: finalize[\s\S]*RIGHT_GIT_STAGE: dev-qualification/);
  assert.match(job, /name: widgets-\$\{\{ github\.run_id \}\}-\$\{\{ github\.run_attempt \}\}-devqual-\$\{\{ matrix\.platform \}\}-\$\{\{ matrix\.architecture \}\}-\$\{\{ needs\.admission\.outputs\.version \}\}-\$\{\{ needs\.admission\.outputs\.source_revision \}\}\$\{\{ needs\.admission\.outputs\.artifact_suffix \}\}/);
  assert.match(job, /widgets-unsigned-devqual\/\*\*/);
  // Pinned action SHAs match the candidate job's.
  const candidateJob = content.slice(content.indexOf("\n  candidate:\n"), content.indexOf("\n  dev-qualification:\n"));
  for (const action of ["actions/checkout", "actions/setup-node", "dtolnay/rust-toolchain", "Swatinem/rust-cache", "Mozilla-Actions/sccache-action", "actions/upload-artifact"]) {
    const pins = (text) => [...new Set([...text.matchAll(new RegExp(`${action.replace("/", "\\/")}@([a-f0-9]{40})`, "g"))].map((m) => m[1]))];
    assert.deepEqual(pins(job), pins(candidateJob), action);
  }
  // The candidate build stays the QA-free one and the candidate never runs journeys.
  assert.match(candidateJob, /run: pnpm run ci:candidate\n/);
  assert.doesNotMatch(candidateJob, /build:dev-qualification|qa:journeys/);
  // Both signing jobs need it; a failed dev qualification skips signing (no always()).
  assert.match(content, /windows-sign:\n    needs: \[admission, candidate, dev-qualification\]\n    if: \$\{\{ needs\.admission\.outputs\.signed_qualification == 'true' \}\}/);
  assert.match(content, /macos-sign:\n    needs: \[admission, candidate, dev-qualification\]\n    if: \$\{\{ needs\.admission\.outputs\.signed_qualification == 'true' \}\}/);
  // Installed qualification still reaches the signed bytes only through windows-sign.
  assert.match(content, /installed-qualification:\n    needs: \[admission, candidate, windows-sign\]/);
});

test("dev-qualification gates a Windows-only signing job and renders without a release chain", () => {
  const manifest = devQualManifest({ buildScript: "build:dev-qualification", journeyScript: "qa:journeys" });
  delete manifest.candidate.releaseChain;
  delete manifest.candidate.signMacos;
  manifest.candidate.os = ["windows-2025"];
  manifest.candidate.targets = [{ os: "windows-2025", platform: "windows", architecture: "x86_64" }];
  const content = renderDevQual(manifest);
  assert.match(content, /windows-sign:\n    needs: \[admission, candidate, dev-qualification\]\n/);
  assert.doesNotMatch(content, /macos-sign:/);
});

// ---- gate.shards / gate.quick (rightkit#950) -------------------------------------------------------------
const SHARD_BASE = { schemaVersion: 1, compile: "github-actions-only", lanes: ["ci"], matrix: { os: ["windows-2025", "macos-15"], node: ["24"] }, packageManager: "pnpm@11.25.0", templateVersion: "1.8.0", profile: "rust-hybrid", gate: { path: "scripts/gate.sh" } };
const SHARD_TEMPLATE = () => fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/ci.yml"), "utf8");
const shardRender = (manifest) => renderTemplate(SHARD_TEMPLATE(), buildRenderContext(manifest, "/repos/widgets"));

test("gate.shards and gate.quick are gone: the ci job runs the one door once per runner", () => {
  const errors = (gate) => validateManifest({ ...SHARD_BASE, gate }).errors.join("\n");
  assert.match(errors({ path: "scripts/gate.sh", shards: [{ id: "a" }, { id: "b" }] }), /gate\.shards and gate\.quick are gone/);
  assert.match(errors({ path: "scripts/gate.sh", quick: true }), /gate\.shards and gate\.quick are gone/);
  assert.deepEqual(validateManifest(SHARD_BASE).errors, []);
  const out = shardRender(SHARD_BASE);
  assert.doesNotMatch(out, /matrix\.shard|ci-summary|^  quick:|bash \.\/scripts\/gate\.sh/m);
  assert.equal(out.match(/^      - run: pnpm exec rightkit-gate --to compile$/gm)?.length, 1);
});

// ---- RightKit check job (rightkit#833, A-11) -------------------------------------------------------------
// The first job of every lane that compiles or tests: needs only the private-repo guard, runs `rightkit-gate check --commit HEAD`
// from the installed @rightkit/release, and every other job of the workflow needs it (directly, or through a job that does).

// The jobs of a rendered workflow in file order: id -> { text, needs }. `needs` is the inline list form every template uses.
function checkJobsOf(workflow) {
  const body = workflow.slice(workflow.indexOf("\njobs:\n") + "\njobs:\n".length);
  const parts = body.split(/^  ([A-Za-z0-9_-]+):$/m);
  const jobs = new Map();
  for (let i = 1; i < parts.length; i += 2) {
    const needs = /^ {4}needs: \[(.*)\]$/m.exec(parts[i + 1])?.[1];
    jobs.set(parts[i], { text: parts[i + 1], needs: needs === undefined ? [] : needs.split(",").map((name) => name.trim()) });
  }
  return jobs;
}
const checkStepsOf = (job) => [...job.text.matchAll(/^ {6}- (?:uses: (\S[^\n]*)|run: ([^\n]*)|name: ([^\n]*))$/gm)].map((m) => m[1] ?? m[2] ?? m[3]);
const checkUses = (job, action) => [...job.text.matchAll(/^ {6}- uses: (\S[^\n]*)$/gm)].map((m) => m[1]).filter((use) => use.startsWith(`${action}@`));
const checkRunsOn = (job) => /^ {4}runs-on: (\S+)$/m.exec(job.text)?.[1];

// Each case: a name, the rendered workflow, and the job that is the lane's own first job after the check.
const checkNode = renderAllLanes(FULL_MANIFEST, "/repos/widgets");
const CHECK_RUST_SHARDED = { ...SW_RUST_CHAIN, devArtifact: { ...SW_RUST_CHAIN.devArtifact, parallel: true, buildScript: "ci:dev" } };
const CHECK_CASES = [
  ["node/ci", checkNode.ci, "test"],
  ["node/qualification", checkNode.qualification, "qualification"],
  ["node/package-smoke", checkNode["package-smoke"], "clean-tarball"],
  ["node/release-candidate", checkNode["release-candidate"], "candidate"],
  ["node/swift-check", checkNode["swift-check"], "swift-check"],
  ["node/publish-npm", checkNode["publish-npm"], "publish-npm"],
  ["rust-hybrid/ci", swRender(SW_RUST, "ci"), "ci"],
  ["rust-hybrid/ci with parallel dev artifacts", swRender(CHECK_RUST_SHARDED, "ci"), "ci"],
  ["rust-hybrid/release-candidate", swRender(SW_RUST_CHAIN, "release-candidate"), "admission"],
];

test("check job: every lane that compiles or tests renders it right after the guard, needing only the guard", () => {
  for (const [name, workflow, first] of CHECK_CASES) {
    const jobs = checkJobsOf(workflow);
    const ids = [...jobs.keys()];
    assert.equal(ids[0], "private-repo-guard", name);
    assert.equal(ids[1], "check", `${name}: the check job follows the guard`);
    assert.equal(ids.filter((id) => id === "check").length, 1, name);
    assert.deepEqual(jobs.get("check").needs, ["private-repo-guard"], `${name}: no dependency except the guard`);
    assert.doesNotMatch(jobs.get("check").text, /^ {4}if:/m, `${name}: the check job always runs`);
    assert.ok(jobs.get(first).needs.includes("check"), `${name}: ${first} needs check`);
  }
});

test("check job: ci, test and quick list it in needs, and so does every other job that compiles or tests", () => {
  const node = (lane) => checkJobsOf(checkNode[lane]);
  assert.deepEqual(node("ci").get("test").needs, ["private-repo-guard", "check"]);
  assert.deepEqual(node("qualification").get("qualification").needs, ["private-repo-guard", "check"]);
  assert.deepEqual(node("package-smoke").get("clean-tarball").needs, ["private-repo-guard", "check"]);
  assert.deepEqual(node("release-candidate").get("candidate").needs, ["private-repo-guard", "check"]);
  assert.deepEqual(node("swift-check").get("swift-check").needs, ["private-repo-guard", "check"]);
  assert.deepEqual(node("publish-npm").get("publish-npm").needs, ["private-repo-guard", "check"]);
  // The qualification jobs that follow need `qualification`, so they sit behind the check through it.
  for (const job of ["schema", "security", "hardening", "conformance"]) assert.deepEqual(node("qualification").get(job).needs, ["qualification"]);

  const plain = checkJobsOf(swRender(SW_RUST, "ci"));
  assert.deepEqual(plain.get("ci").needs, ["private-repo-guard", "check"]);
  const sharded = checkJobsOf(swRender(CHECK_RUST_SHARDED, "ci"));
  assert.deepEqual(sharded.get("ci").needs, ["private-repo-guard", "check"]);
  // devArtifact.parallel compiles in jobs of their own; they wait for the check too. Signing still waits for ci.
  for (const job of ["dev-artifact-mac", "dev-artifact-windows"]) assert.deepEqual(sharded.get(job).needs, ["private-repo-guard", "check"], job);
  assert.deepEqual(sharded.get("dev-mac-sign").needs, ["ci", "dev-artifact-mac"]);
  // The release chain starts at admission; every later job needs admission, directly or through candidate.
  const chain = checkJobsOf(swRender(SW_RUST_CHAIN, "release-candidate"));
  assert.deepEqual(chain.get("admission").needs, ["private-repo-guard", "check"]);
  for (const [id, job] of chain) {
    if (["private-repo-guard", "check", "admission"].includes(id)) continue;
    assert.ok(job.needs.includes("admission") || job.needs.some((need) => need !== "check" && chain.get(need).needs.includes("admission")), `${id} sits behind admission`);
  }
});

test("check job: installs without lifecycle scripts, then runs rightkit-gate check on HEAD; no secrets, no registry fetch", () => {
  for (const [name, workflow] of CHECK_CASES) {
    const jobs = checkJobsOf(workflow);
    const job = jobs.get("check");
    assert.match(job.text, /\n {6}- uses: actions\/checkout@[^\n]*\n {8}with:\n(?: {10}ref: [^\n]*\n)? {10}fetch-depth: 1\n {10}persist-credentials: false\n/, `${name}: depth 1 and no persisted credentials`);
    const steps = checkStepsOf(job);
    assert.equal(steps.at(-1), "pnpm exec rightkit-gate check --commit HEAD", `${name}: the check is the last step`);
    assert.ok(steps.includes("pnpm install --frozen-lockfile --ignore-scripts"), `${name}: frozen install without scripts`);
    assert.ok(steps.findIndex((step) => step.startsWith("pnpm install")) < steps.length - 1, `${name}: install runs before the check`);
    assert.doesNotMatch(job.text, /--filter|secrets\.|right-release|\bnpm exec|\bnpx\b/, `${name}: no filter (it matches no project in a consumer), secret or registry fetch`);
    assert.match(job.text, /^ {4}timeout-minutes: \d+$/m, `${name}: bounded`);
    assert.equal(checkRunsOn(job), checkRunsOn(jobs.get("private-repo-guard")), `${name}: runs where the guard runs`);
    assert.match(job.text, /^ {4}# RightKit check: class A over committed files only/m, name);
    assert.ok(steps.includes("Install the pinned class A scan tools") && steps.includes("Install the pinned workflow linter"), `${name}: class A tools are provisioned, never skipped`);
  }
});

test("check job: pins its actions the way the other jobs of the same workflow pin them", () => {
  for (const [name, workflow] of CHECK_CASES) {
    const jobs = checkJobsOf(workflow);
    const others = [...jobs].filter(([id]) => id !== "check").map(([, job]) => job);
    const check = jobs.get("check");
    for (const action of ["actions/checkout", "actions/setup-node", "pnpm/action-setup"]) {
      const used = checkUses(check, action);
      assert.equal(used.length, 1, `${name}: the check job uses ${action} once`);
      assert.ok(others.some((job) => checkUses(job, action).includes(used[0])), `${name}: ${action} pin ${used[0]} is one the other jobs use`);
    }
    const pinned = checkUses(check, "actions/checkout")[0].includes("@v5") ? "tag" : "sha";
    assert.equal(pinned, name.startsWith("node/") && name !== "node/swift-check" ? "tag" : "sha", `${name}: pin style`);
  }
});

test("check job: node version is the first matrix node, quoted", () => {
  assert.match(checkJobsOf(checkNode.ci).get("check").text, /\n {10}node-version: "22\.13"\n/);
  assert.match(checkJobsOf(swRender(SW_RUST, "ci")).get("check").text, /\n {10}node-version: "26\.8\.1"\n/);
});

test("check job: a release-candidate checks the revision it builds, publish-npm the release tag it publishes; every other lane checks the event's commit", () => {
  assert.match(checkJobsOf(checkNode["release-candidate"]).get("check").text, /\n {10}ref: \$\{\{ inputs\.source_revision \|\| github\.sha \}\}\n {10}fetch-depth: 1\n/);
  assert.match(checkJobsOf(swRender(SW_RUST_CHAIN, "release-candidate")).get("check").text, /\n {10}ref: \$\{\{ inputs\.source_revision \}\}\n {10}fetch-depth: 1\n/);
  for (const [name, workflow] of CHECK_CASES) {
    if (name.endsWith("release-candidate") || name === "node/publish-npm") continue;
    assert.doesNotMatch(checkJobsOf(workflow).get("check").text, /\bref:/, name);
  }
  // publish-npm checks out github.event.release.tag_name, rebuilds the tarball from it and publishes it: the check must read that commit.
  const jobs = checkJobsOf(checkNode["publish-npm"]);
  assert.match(jobs.get("check").text, /\n {10}ref: \$\{\{ github\.event\.release\.tag_name \}\}\n {10}fetch-depth: 1\n/);
  assert.match(jobs.get("publish-npm").text, /\n {10}ref: \$\{\{ github\.event\.release\.tag_name \}\}\n/);
});

// publish-npm holds the OIDC publish credential and rebuilds the tarball from source: the check runs first, on the tag's commit,
// and holds no part of that credential.
test("check job: publish-npm publishes only after the check passed on the release tag; the check holds no credential", () => {
  const jobs = checkJobsOf(checkNode["publish-npm"]);
  assert.deepEqual([...jobs.keys()].filter((id) => id !== "release"), ["private-repo-guard", "check", "publish-npm"]);
  const publish = jobs.get("publish-npm");
  assert.deepEqual(publish.needs, ["private-repo-guard", "check"]);
  assert.match(publish.text, /build-candidate\.mjs/);
  assert.match(publish.text, /check-release\.mjs/);
  assert.match(publish.text, /^ {6}id-token: write$/m);
  const check = jobs.get("check").text;
  assert.doesNotMatch(check, /id-token|environment:|permissions:|secrets\.|registry-url|build-candidate|check-release|npm publish/, "the check job inherits contents: read and nothing else");
  assert.ok(checkNode["publish-npm"].indexOf("\n  check:\n") < checkNode["publish-npm"].indexOf("\n  publish-npm:\n"), "the check precedes the publish job in the file");
});

// The check job installs with the manager's own frozen install (parsePackageManager), plus --ignore-scripts, so it cannot drift
// from the install every build job runs. The publish-npm lane installs with the same command without the flag.
test("check job: its install is the manager's frozen install without lifecycle scripts, derived from the same source as the build's", () => {
  for (const packageManager of ["pnpm@11.25.0", "npm@10.9.2", "yarn@1.22.22"]) {
    const { installCommand } = parsePackageManager(packageManager);
    const manifest = { ...FULL_MANIFEST, packageManager };
    const rendered = renderAllLanes(manifest, "/repos/widgets");
    for (const lane of CHECK_JOB_LANES.filter((name) => name !== "release")) { // release is rust-hybrid only (its own test)
      const steps = checkStepsOf(checkJobsOf(rendered[lane]).get("check"));
      assert.ok(steps.includes(`${installCommand} --ignore-scripts`), `${packageManager} ${lane}`);
      assert.equal(steps.filter((step) => step.startsWith(installCommand)).length, 1, `${packageManager} ${lane}: one install`);
    }
    const publishSteps = checkStepsOf(checkJobsOf(rendered["publish-npm"]).get("publish-npm"));
    assert.ok(publishSteps.includes(installCommand), `${packageManager}: the publish job installs with the same command, with scripts`);
  }
});

test("check job: the ci lane prints the stale-lockfile hint after its install, since the ci job no longer runs when check fails", () => {
  for (const [name, workflow] of CHECK_CASES) {
    const text = checkJobsOf(workflow).get("check").text;
    const hint = text.includes("Print pnpm-lock.yaml patch when the lockfile is stale");
    assert.equal(hint, name === "node/ci" || name.startsWith("rust-hybrid/ci"), `${name}: hint only in the ci lane`);
    if (hint) assert.ok(text.indexOf("pnpm install --frozen-lockfile --ignore-scripts") < text.indexOf("Print pnpm-lock.yaml patch"), name);
  }
});

test("check job: npm and yarn call the installed bin by path, and install without scripts", () => {
  const expected = {
    "pnpm@11.25.0": ["pnpm install --frozen-lockfile --ignore-scripts", "pnpm exec rightkit-gate check --commit HEAD"],
    "npm@10.9.2": ["npm ci --ignore-scripts", "./node_modules/.bin/rightkit-gate check --commit HEAD"],
    "yarn@1.22.22": ["yarn install --frozen-lockfile --ignore-scripts", "./node_modules/.bin/rightkit-gate check --commit HEAD"],
  };
  for (const [packageManager, [install, gate]] of Object.entries(expected)) {
    const rendered = renderAllLanes({ ...FULL_MANIFEST, packageManager }, "/repos/widgets");
    for (const lane of CHECK_JOB_LANES.filter((name) => name !== "release")) {
      const steps = checkStepsOf(checkJobsOf(rendered[lane]).get("check"));
      assert.equal(steps.at(-1), gate, `${packageManager} ${lane}`);
      assert.ok(steps.includes(install), `${packageManager} ${lane}`);
    }
  }
  // npm has no setup action; yarn enables corepack; neither prints the pnpm lockfile hint.
  const npm = checkJobsOf(renderAllLanes({ ...FULL_MANIFEST, packageManager: "npm@10.9.2" }, "/repos/widgets").ci).get("check").text;
  assert.doesNotMatch(npm, /pnpm|corepack/);
  const yarn = checkJobsOf(renderAllLanes({ ...FULL_MANIFEST, packageManager: "yarn@1.22.22" }, "/repos/widgets").ci).get("check").text;
  assert.match(yarn, /corepack enable && corepack prepare yarn@1\.22\.22 --activate/);
  assert.doesNotMatch(yarn, /pnpm/);
});

// Lanes without a check job, each for a reason that is also a constraint: `regenerate` repairs what the check flags, and
// sign-existing and supersede are admitted by @rightkit/release only in their exact rendered shape (release.mjs
// SIGN_EXISTING_*, SUPERSEDE_JOB_HEAD, protectedRegenerate: `needs: [private-repo-guard]`); publish-package-managers builds
// and tests nothing. publish-npm is not here: it checks out the release tag, rebuilds the tarball (build-candidate.mjs) and
// runs check-release.mjs, so it sits behind the check like every lane that builds.
const CHECK_EXEMPT_LANES = {
  regenerate: "it repairs derived files and lockfiles, which the check flags",
  "sign-existing": "exact managed shape: jobs private-repo-guard and sign-existing only",
  "publish-package-managers": "it builds and tests nothing: it syntax-checks a committed formula and publishes metadata of a released version",
};

test("check job: every lane is behind it or exempt for a stated reason, and an exempt lane renders unchanged", () => {
  for (const lane of Object.keys(LANE_FILENAMES)) {
    assert.notEqual(CHECK_JOB_LANES.includes(lane), lane in CHECK_EXEMPT_LANES, `${lane} must be in CHECK_JOB_LANES or CHECK_EXEMPT_LANES, not both and not neither`);
  }
  for (const lane of CHECK_JOB_LANES) assert.ok(checkJob(FULL_MANIFEST, lane).startsWith("  check:\n"), lane);
  for (const lane of Object.keys(CHECK_EXEMPT_LANES)) assert.equal(checkJob(FULL_MANIFEST, lane), "", lane);
  const exempt = [
    ["node/regenerate", checkNode.regenerate],
    ["node/publish-package-managers", checkNode["publish-package-managers"]],
    ["rust-hybrid/regenerate", swRender(SW_RUST, "regenerate")],
    ["rust-hybrid/sign-existing", renderSignExisting(SIGN_EXISTING_MANIFEST)],
  ];
  for (const [name, workflow] of exempt) {
    assert.equal(withoutCheckJob(workflow), workflow, `${name} renders no check job and no check dependency`);
    assert.doesNotMatch(workflow, /^ {2}check:$|rightkit-gate check|, check\]/m, name);
  }
  // An exempt lane builds nothing: the steps that make a lane behind-the-check (the build and the release check) are absent.
  for (const [name, workflow] of exempt) assert.doesNotMatch(workflow, /build-candidate\.mjs|check-release\.mjs|cargo build|pnpm (?:run )?build/, `${name} must not build`);
  assert.deepEqual([...checkJobsOf(renderSignExisting(SIGN_EXISTING_MANIFEST)).keys()], ["private-repo-guard", "sign-existing"]);
  assert.deepEqual(checkJobsOf(swRender(SW_RUST, "regenerate")).get("regenerate").needs, ["private-repo-guard"]);
  // supersede only cancels older runs: it stays in the exact shape @rightkit/release admits.
  for (const workflow of [checkNode.ci, swRender(SW_RUST, "ci")]) {
    assert.match(workflow, /\n {2}supersede:\n {4}needs: \[private-repo-guard\]\n {4}if: \$\{\{ github\.event_name == 'push' && github\.ref_name == github\.event\.repository\.default_branch \}\}\n {4}runs-on: ubuntu-[0-9.]+\n {4}timeout-minutes: \d+\n {4}permissions:\n {6}contents: read\n {6}actions: write\n {4}steps:\n/);
  }
});

test("check job: a queue-mode render without devArtifact still gives ci the check dependency", () => {
  const queued = { ...SW_RUST, concurrency: { mainPushes: "queue", reason: "warm caches" }, devArtifact: undefined };
  const jobs = checkJobsOf(swRender(queued, "ci"));
  assert.deepEqual([...jobs.keys()], ["private-repo-guard", "check", "ci"]);
  assert.deepEqual(jobs.get("ci").needs, ["private-repo-guard", "check"]);
});

test("release lane: the trusted publisher holds R2 only in its publication job, re-checks class A on the staged commit and publishes through the door", () => {
  const manifest = { schemaVersion: 1, compile: "github-actions-only", lanes: ["ci", "release"], matrix: { os: ["windows-2025"], node: ["24"] }, packageManager: "pnpm@11.25.0", templateVersion: "1.8.0", profile: "rust-hybrid", gate: { path: "scripts/gate.sh" } };
  assert.deepEqual(validateManifest(manifest).errors, []);
  assert.match(validateManifest({ ...manifest, profile: undefined, gate: undefined }).errors.join("\n"), /'release' are only valid for profile 'rust-hybrid'/);
  const out = renderTemplate(fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/release.yml"), "utf8"), buildRenderContext(manifest, "/repos/widgets", "release"));
  const jobs = checkJobsOf(out);
  assert.deepEqual([...jobs.keys()], ["private-repo-guard", "check", "publish"]);
  assert.match(jobs.get("check").text, /ref: \$\{\{ inputs\.source_revision \}\}/, "class A runs on the staged commit");
  assert.deepEqual(jobs.get("publish").needs, ["private-repo-guard", "check"]);
  assert.match(jobs.get("publish").text, /^ {4}environment: release$/m);
  assert.match(jobs.get("publish").text, /pnpm exec rightkit-gate --to release --platform "\$platform" --tier "\$TIER" --receipt-in "\$staged\/receipt-\$platform\.json"/);
  assert.match(jobs.get("publish").text, /RIGHTKIT_PUBLISHER: github-environment/);
  for (const [id, job] of jobs) if (id !== "publish") assert.doesNotMatch(job.text, /secrets\./, `${id} holds no secret`);
  assert.equal((out.match(/secrets\.CLOUDFLARE_API_TOKEN/g) ?? []).length, 1, "the R2 credential appears once, in the publish step");
  assert.doesNotMatch(out, /right-release (upload|build|publish)/, "nothing reaches right-release around the door");
});
