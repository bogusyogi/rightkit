import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { renderTemplate, MANAGED_MARKER } from "./render.mjs";
import { buildRenderContext, LANE_FILENAMES, KNOWN_LANES, validateManifest } from "./manifest.mjs";

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
  templateVersion: "1.0.0",
};

function renderAllLanes(manifest, repoPath) {
  const context = buildRenderContext(manifest, repoPath);
  const rendered = {};
  for (const lane of manifest.lanes) {
    const templatePath = path.join(templatesDir, LANE_FILENAMES[lane]);
    const templateText = fs.readFileSync(templatePath, "utf8");
    rendered[lane] = renderTemplate(templateText, context);
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

test("ordinary CI templates cancel superseded runs on the same ref", () => {
  const generic = renderAllLanes(FULL_MANIFEST, "/repos/widgets").ci;
  assert.match(generic, /group: \$\{\{ github\.workflow \}\}-\$\{\{ github\.ref \}\}/);
  assert.match(generic, /cancel-in-progress: true/);
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
    buildRenderContext(manifest, "/repos/widgets"),
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
  assert.doesNotMatch(content, /git rev-parse HEAD/);
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
    buildRenderContext(manifest, "/repos/widgets"),
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
  assert.doesNotMatch(content, /git rev-parse HEAD/);
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
    buildRenderContext(manifest, "/repos/widgets"),
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
    buildRenderContext(manifest, "/repos/widgets"),
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
  // The old ref-triggered "resolve HEAD after checkout" trick is gone
  // entirely for the releaseChain path — every job instead checks out the
  // exact admitted SHA from the admission job's own output.
  assert.doesNotMatch(content, /git rev-parse HEAD/);
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
    buildRenderContext(manifest, "/repos/widgets"),
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
    buildRenderContext(manifest, "/repos/widgets"),
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
    buildRenderContext(manifest, "/repos/widgets"),
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
    buildRenderContext(manifest, "/repos/widgets"),
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
    buildRenderContext(manifest, "/repos/widgets"),
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
