import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { renderTemplate, MANAGED_MARKER } from "./render.mjs";
import { buildRenderContext, LANE_FILENAMES, KNOWN_LANES } from "./manifest.mjs";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)));
const templatesDir = path.join(packageRoot, "templates");

const FULL_MANIFEST = {
  schemaVersion: 1,
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
  assert.match(content, /^name: widgets-ci$/m);
  assert.match(content, /version: 11\.24\.0/);
  assert.match(content, /toolchain: 1\.98\.0/);
  assert.match(content, /bash \.\/scripts\/gate\.sh/);
});

test("rust-hybrid release candidate renders product scripts, artifact handoff, and no signing secret", () => {
  const manifest = {
    ...FULL_MANIFEST,
    lanes: ["ci", "release-candidate"],
    profile: "rust-hybrid",
    gate: { path: "scripts/gate.sh" },
    candidate: { buildScript: "ci:candidate", checkScript: "ci:candidate-check", artifactRoot: "widgets-unsigned", os: ["macos-latest", "windows-latest"] },
    templateVersion: "1.2.0",
  };
  const content = renderTemplate(
    fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/release-candidate.yml"), "utf8"),
    buildRenderContext(manifest, "/repos/widgets"),
  );
  assert.match(content, /pnpm run ci:candidate/);
  assert.match(content, /pnpm run ci:candidate-check/);
  assert.match(content, /RIGHT_GIT_ARTIFACT_ROOT/);
  assert.match(content, /os: \["macos-latest", "windows-latest"\]/);
  assert.match(content, /actions\/upload-artifact@[a-f0-9]{40}/);
  assert.doesNotMatch(content, /secrets\.|AZURE_|APPLE_|certificate|private.key/i);
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
