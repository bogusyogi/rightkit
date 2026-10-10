import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { renderTemplate, MANAGED_MARKER } from "./render.mjs";
import { buildRenderContext, LANE_FILENAMES, KNOWN_LANES, validateManifest, swiftPrivatePackagesConfigureStep, swiftPrivatePackagesCleanupStep } from "./manifest.mjs";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)));
const templatesDir = path.join(packageRoot, "templates");

const FULL_MANIFEST = {
  schemaVersion: 1,
  compile: "github-actions-only",
  lanes: [...KNOWN_LANES],
  matrix: { os: ["ubuntu-latest", "macos-latest", "windows-latest"], node: ["22.13", "24"] },
  packageManager: "pnpm@11.24.0",
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
    rendered[lane] = renderTemplate(templateText, buildRenderContext(manifest, repoPath, lane));
  }
  return rendered;
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
  assert.match(content, /version: 11\.24\.0/);
  assert.match(content, /components: rustfmt, clippy/);
  assert.doesNotMatch(content, /toolchain:/);
  assert.match(content, /group: \$\{\{ github\.workflow \}\}-\$\{\{ github\.ref \}\}/);
  assert.match(content, /cancel-in-progress: true/);
  assert.match(content, /bash \.\/scripts\/gate\.sh/);
  assert.match(content, /name: Detect Rust-affecting changes\n        id: rust-changes/);
  assert.match(content, /RIGHT_GIT_DIFF_BASE: \$\{\{ github\.event_name == 'pull_request' && github\.event\.pull_request\.base\.sha \|\| github\.event\.before \}\}/);
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

test("ordinary CI templates cancel superseded runs on the same ref", () => {
  const generic = renderAllLanes(FULL_MANIFEST, "/repos/widgets").ci;
  assert.match(generic, /group: \$\{\{ github\.workflow \}\}-\$\{\{ github\.ref \}\}/);
  assert.match(generic, /cancel-in-progress: true/);
});

test("ownership revision expression matches each lane trigger contract", () => {
  const rendered = renderAllLanes(FULL_MANIFEST, "/repos/widgets");
  assert.match(rendered.ci, /git rev-parse HEAD\)" = "\$\{\{ github\.sha \}\}/);
  assert.doesNotMatch(rendered.ci, /inputs\.source_revision/);
  assert.match(rendered["publish-package-managers"], /git rev-parse HEAD\)" = "\$\{\{ github\.sha \}\}/);
  assert.doesNotMatch(rendered["publish-package-managers"], /inputs\.source_revision/);
  assert.match(rendered["release-candidate"], /git rev-parse HEAD\)" = "\$\{\{ inputs\.source_revision \}\}/);
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
  // Every downstream job (admission, candidate, and windows-sign here) checks
  // out the exact admitted revision by SHA, never by resolving HEAD after a
  // ref-triggered checkout.
  assert.match(content, /windows-sign:[\s\S]*?ref: \$\{\{ needs\.admission\.outputs\.source_revision \}\}\n\s+fetch-depth: 0\n\s+persist-credentials: false/);
  assert.match(content, /git rev-parse HEAD.*inputs\.source_revision/);
  assert.ok(content.includes("LEGION_SOURCE_REVISION: ${{ env.RIGHT_GIT_SOURCE_REVISION }}"));
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
  assert.match(content, /macos-sign:[\s\S]*?ref: \$\{\{ needs\.admission\.outputs\.source_revision \}\}\n\s+fetch-depth: 0\n\s+persist-credentials: false/);
  assert.match(content, /git rev-parse HEAD.*inputs\.source_revision/);
  assert.ok(content.includes("LEGION_SOURCE_REVISION: ${{ env.RIGHT_GIT_SOURCE_REVISION }}"));
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
  for (const packageManager of ["pnpm@11.24.0", "yarn@4.6.0"]) {
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
  const pnpm = renderAllLanes({ ...FULL_MANIFEST, packageManager: "pnpm@11.24.0" }, "/repos/widgets");
  for (const content of Object.values(pnpm)) {
    assert.match(content, /uses: pnpm\/action-setup@[a-f0-9]{40}/);
    assert.match(content, /version: 11\.24\.0/);
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
  const checkouts = content
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
    packageManager: "pnpm@11.24.0",
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
  assert.match(unset, /CARGO_PROFILE_TEST_DEBUG: "0"\n$/);

  const set = renderTemplate(template, buildRenderContext({ ...structuredClone(base), qaEvidencePath: "pulse-hub-qa" }, "/repos/widgets"));
  assert.ok(set.indexOf("bash ./scripts/gate.sh") < set.indexOf("- name: Upload QA evidence"), "upload must follow the gate step");
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
      assert.match(out, /run: bash \.\/scripts\/gate\.sh\n/);
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
        concurrency: { mainPushes: "queue" },
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
  assert.doesNotMatch(unset, /^          key: /m);
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
  assert.doesNotMatch(unkeyed, /^          key: /m);

  for (const bad of ["${{ secrets.X }}", "has space", "-leading", "semi;colon", "a".repeat(65), 7]) {
    assert.equal(validateManifest({ ...VALID_RUST_HYBRID, rustCache: { workspaces: ["engine"], keySuffix: bad } }).valid, false, String(bad));
  }
  assert.equal(validateManifest({ ...VALID_RUST_HYBRID, rustCache: { workspaces: ["engine"], keySuffix: "v2" } }).valid, true);
  assert.equal(validateManifest({ ...FULL_MANIFEST, lanes: ["ci"], rustCache: { workspaces: ["engine"], keySuffix: "v2" } }).valid, false, "rustCache stays rust-hybrid only");
});

// concurrency.mainPushes: default and "cancel" keep cancel-in-progress true. "queue" cancels only
// runs that are not default-branch pushes; PR runs keep cancelling superseded runs.
test("concurrency.mainPushes: 'queue' stops default-branch pushes cancelling each other and leaves PR runs cancelling", () => {
  const unset = renderRustCi(INCREMENTAL_BASE);
  assert.match(unset, /^concurrency:\n  group: \$\{\{ github\.workflow \}\}-\$\{\{ github\.ref \}\}\n  cancel-in-progress: true\n/m);
  assert.equal(renderRustCi({ ...INCREMENTAL_BASE, concurrency: { mainPushes: "cancel" } }), unset);
  assert.equal(renderRustCi({ ...INCREMENTAL_BASE, concurrency: {} }), unset);

  const queue = renderRustCi({ ...INCREMENTAL_BASE, concurrency: { mainPushes: "queue" } });
  assert.match(queue, /^concurrency:\n  group: \$\{\{ github\.workflow \}\}-\$\{\{ github\.ref \}\}\n  cancel-in-progress: \$\{\{ !\(github\.event_name == 'push' && github\.ref_name == github\.event\.repository\.default_branch\) \}\}\n/m);
  assert.equal(queue.replace(/  cancel-in-progress: .*\n/, "  cancel-in-progress: true\n"), unset, "the concurrency line is the only difference");

  const generic = renderTemplate(fs.readFileSync(path.join(templatesDir, "ci.yml"), "utf8"), buildRenderContext({ ...FULL_MANIFEST, lanes: ["ci"], concurrency: { mainPushes: "queue" } }, "/repos/widgets", "ci"));
  assert.match(generic, /  cancel-in-progress: \$\{\{ !\(github\.event_name == 'push' && github\.ref_name == github\.event\.repository\.default_branch\) \}\}\n/);
  assert.match(renderTemplate(fs.readFileSync(path.join(templatesDir, "ci.yml"), "utf8"), buildRenderContext({ ...FULL_MANIFEST, lanes: ["ci"] }, "/repos/widgets", "ci")), /  cancel-in-progress: true\n/);

  const regenerate = renderTemplate(
    fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/regenerate.yml"), "utf8"),
    buildRenderContext({ ...INCREMENTAL_BASE, lanes: ["ci", "regenerate"], regenerate: { script: "regenerate" }, concurrency: { mainPushes: "queue" } }, "/repos/pulse", "regenerate"),
  );
  assert.match(regenerate, /  cancel-in-progress: false\n/, "the regenerate lane is never cancelled");

  assert.equal(validateManifest({ ...VALID_RUST_HYBRID, concurrency: { mainPushes: "queue" } }).valid, true);
  assert.equal(validateManifest({ ...VALID_RUST_HYBRID, concurrency: { mainPushes: "bogus" } }).valid, false);
  assert.equal(validateManifest({ ...VALID_RUST_HYBRID, concurrency: { mainPushes: "queue", extra: true } }).valid, false);
  assert.equal(validateManifest({ ...VALID_RUST_HYBRID, concurrency: "queue" }).valid, false);
  assert.equal(validateManifest({ ...FULL_MANIFEST, lanes: ["package-smoke"], regenerate: undefined, concurrency: { mainPushes: "queue" } }).valid, false, "concurrency needs lane ci");
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
  assert.match(devJobs, /\n  dev-artifact-mac:\n    # macOS dev app[^\n]*\n[^\n]*\n    needs: \[private-repo-guard\]\n    if: \$\{\{ github\.event_name == 'push' && github\.ref_name == github\.event\.repository\.default_branch \}\}\n    runs-on: xcode-27\n/);
  assert.match(devJobs, /\n  dev-artifact-windows:\n    # Windows dev app[^\n]*\n[^\n]*\n    needs: \[private-repo-guard\]\n    if: \$\{\{ github\.event_name == 'push' && github\.ref_name == github\.event\.repository\.default_branch \}\}\n    runs-on: windows-2025\n/);
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
const NODE_CI_BASE = { schemaVersion: 1, compile: "github-actions-only", lanes: ["ci"], matrix: { os: ["ubuntu-latest"], node: ["24"] }, packageManager: "pnpm@11.24.0", templateVersion: "1.0.0" };
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
  const gate = out.indexOf("      - run: bash ./scripts/gate.sh\n");
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
  assert.ok(validateManifest({ ...NODE_CI_BASE, lanes: ["publish-npm"], publish: { npm: { oidc: true, provenance: true } }, swift: { privatePackages: SWIFT_ONE } }).errors.some((e) => /requires lane 'ci'/.test(e)));
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
    assert.equal(fs.statSync(netrc).mode & 0o777, 0o600);
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
