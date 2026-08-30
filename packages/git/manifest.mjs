// Loader and hand-rolled validator for the per-repo `.rightgit.json` manifest.
// Mirrors the shape documented in manifest.schema.json. No ajv dependency —
// validation is plain JS so the package stays dependency-free (node builtins only).

import fs from "node:fs";
import path from "node:path";

export const MANIFEST_FILENAME = ".rightgit.json";

export const KNOWN_LANES = Object.freeze([
  "ci",
  "qualification",
  "package-smoke",
  "release-candidate",
  "publish-npm",
  "publish-package-managers",
]);

// Rendered filename for each lane, installed under <repo>/.github/workflows/.
export const LANE_FILENAMES = Object.freeze({
  ci: "ci.yml",
  qualification: "qualification.yml",
  "package-smoke": "package-smoke.yml",
  "release-candidate": "release-candidate.yml",
  "publish-npm": "publish-npm.yml",
  "publish-package-managers": "publish-package-managers.yml",
});

const PACKAGE_MANAGER_RE = /^(pnpm|npm|yarn)@\d+\.\d+\.\d+$/;
const TEMPLATE_VERSION_RE = /^\d+\.\d+\.\d+$/;

// F15 fix: every one of these fields is manifest (attacker-controlled)
// input that flows verbatim into a rendered workflow file via render.mjs's
// literal `${TOKEN}` substitution — render.mjs does no escaping, by design
// (it is a byte-deterministic template engine, not a YAML/expression-aware
// one). The re-validation proved a hostile `publish.homebrew.formulaName`
// containing GitHub Actions expression syntax (`${{ secrets.APPLE_... }}`)
// survived unmodified into rendered YAML, which GitHub Actions would then
// evaluate as a real expression — a workflow-expression injection, not just
// a cosmetic issue. The fix is at the INPUT boundary, not the render step:
// every manifest field that reaches a template must match a strict
// allowlist charset that cannot express `${{ }}`, backticks, `$()`,
// newlines, or any other shell/YAML/expression metacharacter — validated
// once here, so render.mjs can stay a dumb, deterministic string replacer.
const SAFE_MATRIX_VALUE_RE = /^[A-Za-z0-9][A-Za-z0-9._/*-]*$/;
const GITHUB_OWNER_RE = /^[A-Za-z0-9](?:[A-Za-z0-9-]{0,38})$/;
const GITHUB_REPO_RE = /^[A-Za-z0-9](?:[A-Za-z0-9._-]{0,99})$/;
const HOMEBREW_FORMULA_NAME_RE = /^[A-Za-z0-9][A-Za-z0-9+_.-]*$/;
const WINGET_PACKAGE_IDENTIFIER_RE = /^[A-Za-z0-9][A-Za-z0-9-]*(?:\.[A-Za-z0-9][A-Za-z0-9-]*)+$/;

const TOP_LEVEL_KEYS = new Set([
  "schemaVersion",
  "lanes",
  "matrix",
  "packageManager",
  "publish",
  "templateVersion",
  "profile",
  "gate",
  "candidate",
]);
const GATE_PATH_RE = /^(?!.*\.\.)(?!.*\s)[A-Za-z0-9][A-Za-z0-9._/-]*$/;
const PACKAGE_SCRIPT_RE = /^[A-Za-z0-9][A-Za-z0-9:_-]*$/;
const GITHUB_VARIABLE_RE = /^[A-Z][A-Z0-9_]*$/;
const RELEASE_PLATFORMS = new Set(["windows", "macos", "linux"]);
const RELEASE_ARCHITECTURES = new Set(["x86_64", "arm64"]);
const NATIVE_RUNNER_TARGETS = new Map([
  ["windows-2025", ["windows", "x86_64"]],
  ["windows-11-arm", ["windows", "arm64"]],
  ["macos-15", ["macos", "arm64"]],
  ["macos-15-intel", ["macos", "x86_64"]],
]);

function inferredTarget(os) {
  const platform = /^windows-/i.test(os) ? "windows" : /^macos-/i.test(os) ? "macos" : "linux";
  return { os, platform, architecture: platform === "macos" ? "arm64" : "x86_64" };
}

function candidateTargets(candidate) {
  return Array.isArray(candidate?.targets) ? candidate.targets : (candidate?.os ?? []).map(inferredTarget);
}

/**
 * Validate a parsed manifest object against the right-git manifest contract.
 * Never throws. Returns { valid, errors }.
 */
export function validateManifest(manifest) {
  const errors = [];

  if (manifest === null || typeof manifest !== "object" || Array.isArray(manifest)) {
    return { valid: false, errors: ["manifest must be a JSON object"] };
  }

  for (const key of Object.keys(manifest)) {
    if (!TOP_LEVEL_KEYS.has(key)) errors.push(`unknown top-level field: ${key}`);
  }

  if (manifest.schemaVersion !== 1) {
    errors.push("schemaVersion must be exactly 1");
  }

  let lanes = [];
  if (!Array.isArray(manifest.lanes) || manifest.lanes.length === 0) {
    errors.push("lanes must be a non-empty array");
  } else {
    lanes = manifest.lanes;
    const seen = new Set();
    for (const lane of lanes) {
      if (typeof lane !== "string" || !KNOWN_LANES.includes(lane)) {
        errors.push(`unknown lane: ${JSON.stringify(lane)}`);
      } else if (seen.has(lane)) {
        errors.push(`duplicate lane: ${lane}`);
      } else {
        seen.add(lane);
      }
    }
  }

  if (manifest.matrix === null || typeof manifest.matrix !== "object" || Array.isArray(manifest.matrix)) {
    errors.push("matrix must be an object with os and node arrays");
  } else {
    for (const field of ["os", "node"]) {
      const value = manifest.matrix[field];
      if (!Array.isArray(value) || value.length === 0 || !value.every((v) => typeof v === "string" && SAFE_MATRIX_VALUE_RE.test(v))) {
        errors.push(
          `matrix.${field} must be a non-empty array of safe runner/version labels (letters, digits, '.', '_', '-', '/', '*' only — no expression or shell syntax)`,
        );
      }
    }
    for (const key of Object.keys(manifest.matrix)) {
      if (key !== "os" && key !== "node") errors.push(`unknown matrix field: ${key}`);
    }
  }

  if (typeof manifest.packageManager !== "string" || !PACKAGE_MANAGER_RE.test(manifest.packageManager)) {
    errors.push("packageManager must match '<pnpm|npm|yarn>@X.Y.Z'");
  }

  if (typeof manifest.templateVersion !== "string" || !TEMPLATE_VERSION_RE.test(manifest.templateVersion)) {
    errors.push("templateVersion must be a semver string 'X.Y.Z'");
  }

  const profile = manifest.profile === undefined ? "node" : manifest.profile;
  if (profile !== "node" && profile !== "rust-hybrid") errors.push("profile must be 'node' or 'rust-hybrid'");
  if (profile === "rust-hybrid") {
    if (lanes.some((lane) => lane !== "ci" && lane !== "release-candidate") || !lanes.includes("ci")) {
      errors.push("profile 'rust-hybrid' requires lane 'ci' and supports optional lane 'release-candidate'");
    }
    if (!manifest.gate || typeof manifest.gate !== "object" || Array.isArray(manifest.gate) || Object.keys(manifest.gate).some((key) => key !== "path") || typeof manifest.gate.path !== "string" || !GATE_PATH_RE.test(manifest.gate.path)) {
      errors.push("profile 'rust-hybrid' requires gate.path with a safe relative path");
    }
  } else if (manifest.gate !== undefined) {
    errors.push("gate is only valid for profile 'rust-hybrid'");
  }

  const candidate = manifest.candidate;
  if (candidate !== undefined) {
    if (candidate === null || typeof candidate !== "object" || Array.isArray(candidate)) {
      errors.push("candidate must be an object");
    } else {
      for (const key of Object.keys(candidate)) {
        if (!["buildScript", "checkScript", "artifactRoot", "os", "targets", "signWindows", "signMacos", "releaseChain"].includes(key)) errors.push(`unknown candidate field: ${key}`);
      }
      for (const key of ["buildScript", "checkScript"]) {
        if (typeof candidate[key] !== "string" || !PACKAGE_SCRIPT_RE.test(candidate[key])) {
          errors.push(`candidate.${key} must be one safe package script name`);
        }
      }
      if (typeof candidate.artifactRoot !== "string" || !GATE_PATH_RE.test(candidate.artifactRoot)) {
        errors.push("candidate.artifactRoot must be one safe relative path");
      }
      if (!Array.isArray(candidate.os) || candidate.os.length === 0 || candidate.os.some((value) => typeof value !== "string" || !SAFE_MATRIX_VALUE_RE.test(value))) {
        errors.push("candidate.os must be a non-empty array of safe runner labels");
      } else if (candidate.targets === undefined && candidate.os.some((value) => !manifest.matrix?.os?.includes(value))) {
        errors.push("candidate.os entries must also exist in matrix.os");
      }
      if (candidate.targets !== undefined) {
        if (!Array.isArray(candidate.targets) || candidate.targets.length === 0) {
          errors.push("candidate.targets must be a non-empty array");
        } else {
          const targetKeys = new Set();
          for (const target of candidate.targets) {
            if (target === null || typeof target !== "object" || Array.isArray(target)) {
              errors.push("candidate.targets entries must be objects");
              continue;
            }
            if (Object.keys(target).some((key) => !["os", "platform", "architecture"].includes(key))) errors.push("candidate.targets entries may only contain os, platform, architecture");
            if (typeof target.os !== "string" || !SAFE_MATRIX_VALUE_RE.test(target.os)) errors.push("candidate.targets[].os must be a safe runner label");
            if (!RELEASE_PLATFORMS.has(target.platform)) errors.push("candidate.targets[].platform must be windows, macos, or linux");
            if (!RELEASE_ARCHITECTURES.has(target.architecture)) errors.push("candidate.targets[].architecture must be x86_64 or arm64");
            const nativeTarget = NATIVE_RUNNER_TARGETS.get(target.os);
            if (nativeTarget && (target.platform !== nativeTarget[0] || target.architecture !== nativeTarget[1])) {
              errors.push(`candidate.targets[] must declare ${target.os} as ${nativeTarget[0]}/${nativeTarget[1]}`);
            }
            if ((target.platform === "windows" && !/^windows-/i.test(target.os)) || (target.platform === "macos" && !/^macos-/i.test(target.os))) {
              errors.push("candidate.targets[].os must match its Windows or macOS platform");
            }
            const targetKey = `${target.os}|${target.platform}|${target.architecture}`;
            if (targetKeys.has(targetKey)) errors.push("candidate.targets entries must be unique");
            targetKeys.add(targetKey);
          }
          const targetOs = new Set(candidate.targets.map((target) => target?.os));
          if (candidate.os.some((os) => !targetOs.has(os)) || targetOs.size !== new Set(candidate.os).size || candidate.os.length !== new Set(candidate.os).size) {
            errors.push("candidate.os must contain exactly candidate.targets runner labels");
          }
        }
      }
      const targets = candidateTargets(candidate);
      if (candidate.signWindows !== undefined && typeof candidate.signWindows !== "boolean") {
        errors.push("candidate.signWindows must be a boolean");
      }
      if (candidate.signWindows === true && targets.filter((target) => target?.platform === "windows").length === 0) {
        errors.push("candidate.signWindows requires at least one Windows target");
      }
      if (candidate.signMacos !== undefined && typeof candidate.signMacos !== "boolean") {
        errors.push("candidate.signMacos must be a boolean");
      }
      if (candidate.signMacos === true && targets.filter((target) => target?.platform === "macos").length === 0) {
        errors.push("candidate.signMacos requires at least one macOS target");
      }
      if ((candidate.signWindows === true || candidate.signMacos === true) && manifest.matrix?.node?.length !== 1) {
        errors.push("candidate signing requires exactly one matrix.node version");
      }
      if (candidate.releaseChain !== undefined) {
        const chain = candidate.releaseChain;
        const allowedChainKeys = ["windowsFinalizeScript", "macosFinalizeScript", "installedQualificationScript", "publishScript", "qualificationRunnerVariable", "publishSecrets"];
        if (chain === null || typeof chain !== "object" || Array.isArray(chain)) {
          errors.push("candidate.releaseChain must be an object");
        } else {
          for (const key of Object.keys(chain)) {
            if (!allowedChainKeys.includes(key)) errors.push(`unknown candidate.releaseChain field: ${key}`);
          }
          for (const key of ["windowsFinalizeScript", "installedQualificationScript", "publishScript"]) {
            if (typeof chain[key] !== "string" || !PACKAGE_SCRIPT_RE.test(chain[key])) {
              errors.push(`candidate.releaseChain.${key} must be one safe package script name`);
            }
          }
          if (chain.macosFinalizeScript !== undefined && (typeof chain.macosFinalizeScript !== "string" || !PACKAGE_SCRIPT_RE.test(chain.macosFinalizeScript))) {
            errors.push("candidate.releaseChain.macosFinalizeScript must be one safe package script name");
          }
          if (typeof chain.qualificationRunnerVariable !== "string" || !GITHUB_VARIABLE_RE.test(chain.qualificationRunnerVariable)) {
            errors.push("candidate.releaseChain.qualificationRunnerVariable must be one uppercase GitHub variable name");
          }
          if (chain.publishSecrets !== undefined && (!Array.isArray(chain.publishSecrets) || new Set(chain.publishSecrets).size !== chain.publishSecrets.length || chain.publishSecrets.some((value) => typeof value !== "string" || !GITHUB_VARIABLE_RE.test(value)))) {
            errors.push("candidate.releaseChain.publishSecrets must be unique uppercase GitHub secret names");
          }
          if (candidate.signWindows !== true) errors.push("candidate.releaseChain requires candidate.signWindows=true");
          if (targets.filter((target) => target?.platform === "windows").length !== 1 || targets.filter((target) => target?.platform === "macos").length > 1) {
            errors.push("candidate.releaseChain requires at most one target per signing platform; multi-target aggregation is not supported");
          }
          if (candidate.signMacos === true && !chain.macosFinalizeScript) errors.push("candidate.releaseChain with candidate.signMacos=true requires macosFinalizeScript");
          if (chain.macosFinalizeScript && candidate.signMacos !== true) errors.push("candidate.releaseChain.macosFinalizeScript requires candidate.signMacos=true");
        }
      }
    }
  }
  if (profile === "rust-hybrid" && lanes.includes("release-candidate") && candidate === undefined) {
    errors.push("rust-hybrid release-candidate lane requires candidate { buildScript, checkScript, artifactRoot }");
  }
  if (profile !== "rust-hybrid" && candidate !== undefined) {
    errors.push("candidate is only valid for profile 'rust-hybrid'");
  }

  const publish = manifest.publish;
  if (publish !== undefined) {
    if (publish === null || typeof publish !== "object" || Array.isArray(publish)) {
      errors.push("publish must be an object");
    } else {
      for (const key of Object.keys(publish)) {
        if (!["npm", "homebrew", "winget"].includes(key)) errors.push(`unknown publish field: ${key}`);
      }
      if (publish.npm !== undefined) {
        const npm = publish.npm;
        if (npm === null || typeof npm !== "object") {
          errors.push("publish.npm must be an object");
        } else {
          if (npm.oidc !== true) errors.push("publish.npm.oidc must be true (I2: OIDC is the only publish credential)");
          if (typeof npm.provenance !== "boolean") errors.push("publish.npm.provenance must be a boolean");
        }
      }
      if (publish.homebrew !== undefined) {
        const hb = publish.homebrew;
        if (hb === null || typeof hb !== "object") {
          errors.push("publish.homebrew must be an object");
        } else {
          const tapMatch = typeof hb.tap === "string" ? /^([^/]+)\/([^/]+)$/.exec(hb.tap) : null;
          if (!tapMatch || !GITHUB_OWNER_RE.test(tapMatch[1]) || !GITHUB_REPO_RE.test(tapMatch[2])) {
            errors.push(
              "publish.homebrew.tap must be 'owner/repo' using GitHub-safe characters only (letters, digits, '-', '_', '.' — no expression or shell syntax)",
            );
          }
          if (typeof hb.formulaName !== "string" || !HOMEBREW_FORMULA_NAME_RE.test(hb.formulaName)) {
            errors.push(
              "publish.homebrew.formulaName must be a Homebrew-safe identifier (letters, digits, '+', '_', '.', '-' only — no expression or shell syntax)",
            );
          }
        }
      }
      if (publish.winget !== undefined) {
        const wg = publish.winget;
        if (wg === null || typeof wg !== "object" || typeof wg.packageIdentifier !== "string" || !WINGET_PACKAGE_IDENTIFIER_RE.test(wg.packageIdentifier)) {
          errors.push(
            "publish.winget.packageIdentifier must be a dotted WinGet-safe identifier, e.g. 'Publisher.Package' (letters, digits, '-', '.' only — no expression or shell syntax)",
          );
        }
      }
    }
  }

  if (lanes.includes("publish-npm")) {
    if (!publish || !publish.npm) {
      errors.push("lane 'publish-npm' requires publish.npm { oidc: true, provenance }");
    }
  }

  if (lanes.includes("publish-package-managers")) {
    if (!publish || !publish.homebrew) {
      errors.push("lane 'publish-package-managers' requires publish.homebrew { tap, formulaName }");
    }
    if (!publish || !publish.winget) {
      errors.push("lane 'publish-package-managers' requires publish.winget { packageIdentifier }");
    }
  }

  return { valid: errors.length === 0, errors };
}

/**
 * Read and validate `<repoPath>/.rightgit.json`.
 * Never throws — returns a typed result: { ok, manifest, errors, path }.
 */
export function loadManifest(repoPath) {
  const manifestPath = path.join(repoPath, MANIFEST_FILENAME);
  let raw;
  try {
    raw = fs.readFileSync(manifestPath, "utf8");
  } catch (error) {
    return { ok: false, manifest: null, errors: [`cannot read ${manifestPath}: ${error.message}`], path: manifestPath };
  }

  let manifest;
  try {
    manifest = JSON.parse(raw);
  } catch (error) {
    return { ok: false, manifest: null, errors: [`invalid JSON in ${manifestPath}: ${error.message}`], path: manifestPath };
  }

  const { valid, errors } = validateManifest(manifest);
  if (!valid) {
    return { ok: false, manifest, errors, path: manifestPath };
  }
  return { ok: true, manifest, errors: [], path: manifestPath };
}

function parsePackageManager(spec) {
  const match = /^(pnpm|npm|yarn)@(\d+\.\d+\.\d+)$/.exec(spec);
  if (!match) throw new Error(`invalid packageManager spec: ${spec}`);
  const [, name, version] = match;
  const installCommand = {
    pnpm: "pnpm install --frozen-lockfile",
    npm: "npm ci",
    yarn: "yarn install --frozen-lockfile",
  }[name];
  const runPrefix = { pnpm: "pnpm run", npm: "npm run", yarn: "yarn run" }[name];
  const setup = {
    pnpm: `      - uses: pnpm/action-setup@0977fd99725f1db4007ccb2928dbb4e90d06cc86 # v6.0.10\n        with:\n          version: ${version}`,
    npm: "",
    yarn: `      - run: corepack enable && corepack prepare yarn@${version} --activate`,
  }[name];
  return { name, version, installCommand, runPrefix, setup };
}

function yamlFlowStringList(values) {
  return `[${values.map((v) => JSON.stringify(String(v))).join(", ")}]`;
}

function candidateMatrix(manifest) {
  const entries = [];
  for (const target of candidateTargets(manifest.candidate ?? { os: manifest.matrix.os })) {
    for (const node of manifest.matrix.node) entries.push({ ...target, node, targetTriple: targetTriple(target) });
  }
  return [
    "        include:",
    ...entries.flatMap((entry) => [
      `          - os: ${JSON.stringify(entry.os)}`,
      `            node: ${JSON.stringify(entry.node)}`,
      `            platform: ${JSON.stringify(entry.platform)}`,
      `            architecture: ${JSON.stringify(entry.architecture)}`,
      `            targetTriple: ${JSON.stringify(entry.targetTriple)}`,
    ]),
  ].join("\n");
}

function targetTriple(target) {
  const rustArchitecture = target.architecture === "arm64" ? "aarch64" : target.architecture;
  if (target.platform === "windows") return `${rustArchitecture}-pc-windows-msvc`;
  if (target.platform === "macos") return `${rustArchitecture}-apple-darwin`;
  return `${rustArchitecture}-unknown-linux-gnu`;
}

function signingMatrix(manifest, platform) {
  const node = String(manifest.matrix.node[0]);
  const entries = candidateTargets(manifest.candidate)
    .filter((target) => target.platform === platform)
    .map((target) => ({ ...target, node, targetTriple: targetTriple(target) }));
  return [
    "        include:",
    ...entries.flatMap((entry) => [
      `          - os: ${JSON.stringify(entry.os)}`,
      `            platform: ${JSON.stringify(entry.platform)}`,
      `            architecture: ${JSON.stringify(entry.architecture)}`,
      `            targetTriple: ${JSON.stringify(entry.targetTriple)}`,
      `            node: ${JSON.stringify(entry.node)}`,
    ]),
  ].join("\n");
}

const AZURE_LOGIN_SHA = "7ddb5af1ef8758cf1353cf3b42f940aee27ba21c"; // v3
const DOWNLOAD_ARTIFACT_SHA = "d3f86a106a0bac45b974a628896c90dbdf5c8093"; // v4.3.0
const UPLOAD_ARTIFACT_SHA = "ea165f8d65b6e75b540449e92b4886f43607fa02"; // v4
const ARTIFACT_SIGNING_CLIENT_VERSION = "1.0.128";
const ARTIFACT_SIGNING_CLIENT_SHA256 = "74BD7D27E6CE1051409C38D9B46BC8DF0400ECD643D51FFBF2AC00869061E40B";
const WINDOWS_SDK_BUILD_TOOLS_VERSION = "10.0.26100.4188";
const WINDOWS_SDK_BUILD_TOOLS_SHA256 = "180DEB372659029864C10A0C04787833234D64AACD1D2C0661D2C00295D8E022";

function windowsSigningJob(manifest, pm, artifactPrefix) {
  if (manifest.candidate?.signWindows !== true) return "";
  const nodeVersion = String(manifest.matrix.node[0]);
  const releaseChain = manifest.candidate.releaseChain;
  const finalizeCommand = releaseChain
    ? `${pm.runPrefix} ${releaseChain.windowsFinalizeScript}`
    : "pnpm exec right-release build --platform win --skip-checks";
  const finalizeEnvironment = releaseChain
    ? `          RIGHT_GIT_UNSIGNED_CANDIDATE_ROOT: \${{ runner.temp }}/unsigned-windows-candidate
          RIGHT_GIT_FINALIZED_WINDOWS_ROOT: \${{ runner.temp }}/finalized-windows
          RIGHT_GIT_SOURCE_REVISION: \${{ env.RIGHT_GIT_SOURCE_REVISION }}`
    : `          LEGION_UNSIGNED_CANDIDATE_ROOT: \${{ runner.temp }}/unsigned-windows-candidate
          LEGION_SOURCE_REVISION: \${{ env.RIGHT_GIT_SOURCE_REVISION }}`;
  const uploadPath = releaseChain
    ? "            ${{ runner.temp }}/finalized-windows/**"
    : "            dist/releases/windows/**\n            .right-release/receipts/windows-*.json";
  return `
  windows-sign:
    needs: [candidate]
    if: \${{ startsWith(github.ref, 'refs/tags/v') || (github.event_name == 'workflow_dispatch' && inputs.signed_qualification == true) }}
    environment: release
    strategy:
      fail-fast: false
      matrix:
${signingMatrix(manifest, "windows")}
    runs-on: \${{ matrix.os }}
    permissions:
      contents: read
      id-token: write
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
      - name: Resolve checked-out source revision
        shell: pwsh
        run: '"RIGHT_GIT_SOURCE_REVISION=$(git rev-parse HEAD)" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append'
      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0
        with:
          node-version: ${nodeVersion}
          package-manager-cache: false
${pm.setup}
      - uses: dtolnay/rust-toolchain@f8be11a05b1d4f3fcebe6410cc16743212b999b0
        with:
          toolchain: 1.98.0
          targets: \${{ matrix.targetTriple }}
      - run: ${pm.installCommand}
      - name: Download exact unsigned Windows candidate
        uses: actions/download-artifact@${DOWNLOAD_ARTIFACT_SHA} # v4.3.0
        with:
          name: ${artifactPrefix}-unsigned-\${{ matrix.platform }}-\${{ matrix.architecture }}-${nodeVersion}
          path: \${{ runner.temp }}/unsigned-windows-candidate
      - name: Authenticate to Azure with GitHub OIDC
        uses: azure/login@${AZURE_LOGIN_SHA} # v3
        with:
          client-id: \${{ vars.AZURE_CLIENT_ID }}
          tenant-id: \${{ vars.AZURE_TENANT_ID }}
          subscription-id: \${{ vars.AZURE_SUBSCRIPTION_ID }}
      - name: Install pinned Artifact Signing client
        shell: pwsh
        run: |
          $ErrorActionPreference = 'Stop'
          $tools = Join-Path $env:RUNNER_TEMP 'artifact-signing-tools'
          New-Item -ItemType Directory -Force -Path $tools | Out-Null
          $client = Join-Path $tools 'client.zip'
          $sdk = Join-Path $tools 'sdk.zip'
          Invoke-WebRequest 'https://api.nuget.org/v3-flatcontainer/microsoft.artifactsigning.client/${ARTIFACT_SIGNING_CLIENT_VERSION}/microsoft.artifactsigning.client.${ARTIFACT_SIGNING_CLIENT_VERSION}.nupkg' -OutFile $client
          Invoke-WebRequest 'https://api.nuget.org/v3-flatcontainer/microsoft.windows.sdk.buildtools/${WINDOWS_SDK_BUILD_TOOLS_VERSION}/microsoft.windows.sdk.buildtools.${WINDOWS_SDK_BUILD_TOOLS_VERSION}.nupkg' -OutFile $sdk
          if ((Get-FileHash $client -Algorithm SHA256).Hash -ne '${ARTIFACT_SIGNING_CLIENT_SHA256}') { throw 'Artifact Signing client hash mismatch' }
          if ((Get-FileHash $sdk -Algorithm SHA256).Hash -ne '${WINDOWS_SDK_BUILD_TOOLS_SHA256}') { throw 'Windows SDK Build Tools hash mismatch' }
          Expand-Archive $client (Join-Path $tools 'client')
          Expand-Archive $sdk (Join-Path $tools 'sdk')
          "AZURE_ARTIFACT_SIGNING_DLIB_PATH=$tools\\client\\bin\\x64\\Azure.CodeSigning.Dlib.dll" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append
          "AZURE_SIGNTOOL_PATH=$tools\\sdk\\bin\\10.0.26100.0\\x64\\signtool.exe" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append
      - name: Sign & finalize Windows candidate
        run: ${finalizeCommand}
        env:
${finalizeEnvironment}
          RIGHT_GIT_RELEASE_PLATFORM: \${{ matrix.platform }}
          RIGHT_GIT_RELEASE_ARCHITECTURE: \${{ matrix.architecture }}
          AZURE_ARTIFACT_SIGNING_ENDPOINT: \${{ vars.AZURE_ARTIFACT_SIGNING_ENDPOINT }}
          AZURE_ARTIFACT_SIGNING_ACCOUNT: \${{ vars.AZURE_ARTIFACT_SIGNING_ACCOUNT }}
          AZURE_ARTIFACT_SIGNING_PROFILE: \${{ vars.AZURE_ARTIFACT_SIGNING_PROFILE }}
      - name: Upload signed Windows candidate
        uses: actions/upload-artifact@${UPLOAD_ARTIFACT_SHA} # v4
        with:
          name: ${artifactPrefix}-signed-windows-\${{ matrix.architecture }}-${nodeVersion}
          path: |
${uploadPath}
          if-no-files-found: error
`.trimEnd();
}

function macosSigningJob(manifest, pm, artifactPrefix) {
  if (manifest.candidate?.signMacos !== true) return "";
  const nodeVersion = String(manifest.matrix.node[0]);
  const releaseChain = manifest.candidate.releaseChain;
  const finalizeCommand = releaseChain
    ? `${pm.runPrefix} ${releaseChain.macosFinalizeScript}`
    : "pnpm exec right-release build --platform mac --skip-checks";
  const finalizeEnvironment = releaseChain
    ? `          RIGHT_GIT_UNSIGNED_CANDIDATE_ROOT: \${{ runner.temp }}/unsigned-macos-candidate
          RIGHT_GIT_FINALIZED_MACOS_ROOT: \${{ runner.temp }}/finalized-macos
          RIGHT_GIT_SOURCE_REVISION: \${{ env.RIGHT_GIT_SOURCE_REVISION }}`
    : `          LEGION_UNSIGNED_CANDIDATE_ROOT: \${{ runner.temp }}/unsigned-macos-candidate
          LEGION_SOURCE_REVISION: \${{ env.RIGHT_GIT_SOURCE_REVISION }}`;
  const uploadPath = releaseChain
    ? "            ${{ runner.temp }}/finalized-macos/**"
    : "            dist/releases/mac/**\n            .right-release/receipts/macos-*.json";
  return `
  macos-sign:
    needs: [candidate]
    if: \${{ startsWith(github.ref, 'refs/tags/v') || (github.event_name == 'workflow_dispatch' && inputs.signed_qualification == true) }}
    environment: release
    strategy:
      fail-fast: false
      matrix:
${signingMatrix(manifest, "macos")}
    runs-on: \${{ matrix.os }}
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
      - name: Resolve checked-out source revision
        shell: bash
        run: printf 'RIGHT_GIT_SOURCE_REVISION=%s\\n' "$(git rev-parse HEAD)" >> "$GITHUB_ENV"
      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0
        with:
          node-version: ${nodeVersion}
          package-manager-cache: false
${pm.setup}
      - uses: dtolnay/rust-toolchain@f8be11a05b1d4f3fcebe6410cc16743212b999b0
        with:
          toolchain: 1.98.0
          targets: \${{ matrix.targetTriple }}
      - run: ${pm.installCommand}
      - name: Download exact unsigned macOS candidate
        uses: actions/download-artifact@${DOWNLOAD_ARTIFACT_SHA} # v4.3.0
        with:
          name: ${artifactPrefix}-unsigned-\${{ matrix.platform }}-\${{ matrix.architecture }}-${nodeVersion}
          path: \${{ runner.temp }}/unsigned-macos-candidate
      - name: Install Apple signing identity in temporary keychain
        shell: bash
        env:
          APPLE_CERTIFICATE_BASE64: \${{ secrets.APPLE_CERTIFICATE_BASE64 }}
          APPLE_CERTIFICATE_PASSWORD: \${{ secrets.APPLE_CERTIFICATE_PASSWORD }}
          APPLE_KEYCHAIN_PASSWORD: \${{ secrets.APPLE_KEYCHAIN_PASSWORD }}
          APPLE_API_KEY_BASE64: \${{ secrets.APPLE_API_KEY_BASE64 }}
        run: |
          set -euo pipefail
          certificate_path="$RUNNER_TEMP/apple-signing.p12"
          key_path="$RUNNER_TEMP/AuthKey.p8"
          keychain_path="$RUNNER_TEMP/apple-signing.keychain-db"
          printf '%s' "$APPLE_CERTIFICATE_BASE64" | base64 --decode > "$certificate_path"
          printf '%s' "$APPLE_API_KEY_BASE64" | base64 --decode > "$key_path"
          security create-keychain -p "$APPLE_KEYCHAIN_PASSWORD" "$keychain_path"
          security set-keychain-settings -lut 21600 "$keychain_path"
          security unlock-keychain -p "$APPLE_KEYCHAIN_PASSWORD" "$keychain_path"
          security import "$certificate_path" -P "$APPLE_CERTIFICATE_PASSWORD" -A -t cert -f pkcs12 -k "$keychain_path"
          security set-key-partition-list -S apple-tool:,apple: -k "$APPLE_KEYCHAIN_PASSWORD" "$keychain_path"
          security list-keychain -d user -s "$keychain_path"
          printf 'APPLE_API_KEY_PATH=%s\\n' "$key_path" >> "$GITHUB_ENV"
      - name: Sign, notarize & finalize macOS candidate
        run: ${finalizeCommand}
        env:
${finalizeEnvironment}
          RIGHT_GIT_RELEASE_PLATFORM: \${{ matrix.platform }}
          RIGHT_GIT_RELEASE_ARCHITECTURE: \${{ matrix.architecture }}
          APPLE_DEVELOPER_ID: \${{ vars.APPLE_DEVELOPER_ID }}
          APPLE_API_KEY: \${{ secrets.APPLE_API_KEY }}
          APPLE_API_ISSUER: \${{ secrets.APPLE_API_ISSUER }}
      - name: Upload signed & notarized macOS candidate
        uses: actions/upload-artifact@${UPLOAD_ARTIFACT_SHA} # v4
        with:
          name: ${artifactPrefix}-signed-macos-\${{ matrix.architecture }}-${nodeVersion}
          path: |
${uploadPath}
          if-no-files-found: error
      - name: Remove Apple signing material
        if: \${{ always() }}
        shell: bash
        run: |
          security delete-keychain "$RUNNER_TEMP/apple-signing.keychain-db" 2>/dev/null || true
          rm -f "$RUNNER_TEMP/apple-signing.p12" "$RUNNER_TEMP/AuthKey.p8"
`.trimEnd();
}

function releaseChainJobs(manifest, pm, artifactPrefix) {
  const chain = manifest.candidate?.releaseChain;
  if (!chain) return "";
  const windowsTarget = candidateTargets(manifest.candidate).find((target) => target.platform === "windows");
  const windowsRunner = windowsTarget.os;
  const windowsArchitecture = windowsTarget.architecture;
  const macosTarget = candidateTargets(manifest.candidate).find((target) => target.platform === "macos");
  const nodeVersion = String(manifest.matrix.node[0]);
  const macosDependency = manifest.candidate.signMacos === true ? ", macos-sign" : "";
  const macosSuccess = manifest.candidate.signMacos === true ? " && needs.macos-sign.result == 'success'" : "";
  const macosDownload = manifest.candidate.signMacos === true
    ? `      - name: Download same-run signed & notarized macOS release
        uses: actions/download-artifact@${DOWNLOAD_ARTIFACT_SHA} # v4.3.0
        with:
          name: ${artifactPrefix}-signed-macos-${macosTarget.architecture}-${nodeVersion}
          path: \${{ runner.temp }}/right-git-release/macos`
    : "";
  const publishSecretEnvironment = (chain.publishSecrets ?? [])
    .map((name) => `          ${name}: \${{ secrets.${name} }}`)
    .join("\n");
  return `
  installed-qualification:
    needs: [candidate, windows-sign]
    if: \${{ always() && (startsWith(github.ref, 'refs/tags/v') || (github.event_name == 'workflow_dispatch' && inputs.signed_qualification == true)) && needs.candidate.result == 'success' }}
    environment: release
    runs-on: \${{ vars.${chain.qualificationRunnerVariable} || '${windowsRunner}' }}
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
      - name: Resolve checked-out source revision
        shell: pwsh
        run: '"RIGHT_GIT_SOURCE_REVISION=$(git rev-parse HEAD)" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append'
      - name: Fail closed when protected finalization is absent
        if: \${{ needs.windows-sign.result != 'success' }}
        shell: pwsh
        run: |
          throw 'installed_qualification_failed { reason: prerequisites_absent, detail: "same-run Windows signing/finalization did not succeed" }'
      - name: Download same-run finalized Windows release
        uses: actions/download-artifact@${DOWNLOAD_ARTIFACT_SHA} # v4.3.0
        with:
          name: ${artifactPrefix}-signed-windows-${windowsArchitecture}-${nodeVersion}
          path: \${{ runner.temp }}/right-git-release/windows
      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0
        with:
          node-version: ${nodeVersion}
          package-manager-cache: false
${pm.setup}
      - run: ${pm.installCommand}
      - name: Run installed-artifact qualification
        run: ${pm.runPrefix} ${chain.installedQualificationScript}
        env:
          RIGHT_GIT_FINALIZED_WINDOWS_ROOT: \${{ runner.temp }}/right-git-release/windows
          RIGHT_GIT_QUALIFICATION_EVIDENCE_ROOT: \${{ runner.temp }}/right-git-release/qualification
          RIGHT_GIT_SOURCE_REVISION: \${{ env.RIGHT_GIT_SOURCE_REVISION }}
      - name: Upload installed qualification evidence
        uses: actions/upload-artifact@${UPLOAD_ARTIFACT_SHA} # v4
        with:
          name: ${artifactPrefix}-installed-qualification-${nodeVersion}
          path: \${{ runner.temp }}/right-git-release/qualification/**
          if-no-files-found: error
  publish:
    needs: [installed-qualification${macosDependency}]
    if: \${{ startsWith(github.ref, 'refs/tags/v') && needs.installed-qualification.result == 'success'${macosSuccess} }}
    environment: release
    runs-on: \${{ vars.${chain.qualificationRunnerVariable} || '${windowsRunner}' }}
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
      - name: Resolve checked-out source revision
        shell: pwsh
        run: '"RIGHT_GIT_SOURCE_REVISION=$(git rev-parse HEAD)" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append'
      - name: Download same-run finalized Windows release
        uses: actions/download-artifact@${DOWNLOAD_ARTIFACT_SHA} # v4.3.0
        with:
          name: ${artifactPrefix}-signed-windows-${windowsArchitecture}-${nodeVersion}
          path: \${{ runner.temp }}/right-git-release/windows
${macosDownload}
      - name: Download same-run installed qualification evidence
        uses: actions/download-artifact@${DOWNLOAD_ARTIFACT_SHA} # v4.3.0
        with:
          name: ${artifactPrefix}-installed-qualification-${nodeVersion}
          path: \${{ runner.temp }}/right-git-release/qualification
      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0
        with:
          node-version: ${nodeVersion}
          package-manager-cache: false
${pm.setup}
      - run: ${pm.installCommand}
      - name: Publish qualified signed release
        run: ${pm.runPrefix} ${chain.publishScript}
        env:
          GH_TOKEN: \${{ github.token }}
          RIGHT_GIT_FINALIZED_WINDOWS_ROOT: \${{ runner.temp }}/right-git-release/windows
          RIGHT_GIT_FINALIZED_MACOS_ROOT: \${{ runner.temp }}/right-git-release/macos
          RIGHT_GIT_QUALIFICATION_EVIDENCE_ROOT: \${{ runner.temp }}/right-git-release/qualification
          RIGHT_GIT_SOURCE_REVISION: \${{ env.RIGHT_GIT_SOURCE_REVISION }}
${publishSecretEnvironment}
`.trimEnd();
}

/**
 * Derive one project name from the target repo's package.json, falling back
 * to the repo directory's basename. Never throws.
 */
export function deriveProjectName(repoPath) {
  try {
    const pkgRaw = fs.readFileSync(path.join(repoPath, "package.json"), "utf8");
    const pkg = JSON.parse(pkgRaw);
    if (typeof pkg.name === "string" && pkg.name.length > 0) {
      return pkg.name.replace(/^@[^/]+\//, "");
    }
  } catch {
    // fall through to directory name
  }
  return path.basename(path.resolve(repoPath));
}

/**
 * Build the flat placeholder -> value map used by render.mjs, from a
 * validated manifest plus the target repo's derived project name.
 */
export function buildRenderContext(manifest, repoPath) {
  const pm = parsePackageManager(manifest.packageManager);
  const projectName = deriveProjectName(repoPath);
  const artifactPrefix = projectName.toLowerCase().replace(/[^a-z0-9-]+/g, "-");

  const context = {
    OS_MATRIX: yamlFlowStringList(manifest.matrix.os),
    NODE_MATRIX: yamlFlowStringList(manifest.matrix.node),
    NODE_PUBLISH_VERSION: JSON.stringify(String(manifest.matrix.node[0])),
    PM_NAME: pm.name,
    PM_VERSION: pm.version,
    PM_CACHE: pm.name,
    INSTALL_COMMAND: pm.installCommand,
    PM_RUN: pm.runPrefix,
    PACKAGE_MANAGER_SETUP: pm.setup,
    ARTIFACT_PREFIX: artifactPrefix,
    TEMPLATE_VERSION: manifest.templateVersion,
    GATE_PATH: manifest.gate?.path,
    CANDIDATE_BUILD_SCRIPT: manifest.candidate?.buildScript,
    CANDIDATE_CHECK_SCRIPT: manifest.candidate?.checkScript,
    CANDIDATE_ARTIFACT_ROOT: manifest.candidate?.artifactRoot,
    CANDIDATE_OS_MATRIX: yamlFlowStringList(manifest.candidate?.os ?? manifest.matrix.os),
    CANDIDATE_MATRIX: candidateMatrix(manifest),
    WINDOWS_SIGNING_JOB: windowsSigningJob(manifest, pm, artifactPrefix),
    MACOS_SIGNING_JOB: macosSigningJob(manifest, pm, artifactPrefix),
    RELEASE_CHAIN_JOBS: releaseChainJobs(manifest, pm, artifactPrefix),
  };

  if (manifest.publish?.npm) {
    context.NPM_PROVENANCE_FLAG = manifest.publish.npm.provenance ? " --provenance" : "";
  }
  if (manifest.publish?.homebrew) {
    context.HOMEBREW_TAP = manifest.publish.homebrew.tap;
    context.HOMEBREW_FORMULA_NAME = manifest.publish.homebrew.formulaName;
  }
  if (manifest.publish?.winget) {
    context.WINGET_PACKAGE_IDENTIFIER = manifest.publish.winget.packageIdentifier;
  }

  return context;
}
