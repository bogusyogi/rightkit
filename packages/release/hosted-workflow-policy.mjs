// hostedWorkflows: { policy: "declared-unrelated", files: { "<file>.yml": "<reason>" | { reason, idToken } } }
//
// For a repository that ships more than the released app (a monorepo whose web or server pipeline is a hand-written
// GitHub workflow). The released app still builds, signs and uploads only through right-release on its native host;
// every hosted workflow must be declared with the reason it exists, and none may be able to build, sign or upload a
// release. What makes that hold is credentials: an admitted workflow reaches no secret but GITHUB_TOKEN (no secrets
// context, no `secrets: inherit`, no reusable workflow, no escape that could spell one), mints no OIDC token unless the
// file declares why (`idToken`), and runs only commit-pinned actions. Without an Apple, Azure or store credential it
// cannot sign or upload as Orthic Labs. The command and artifact patterns are a denylist backstop, not the guarantee:
// a script file the workflow runs is not read.
export const DECLARED_UNRELATED_POLICY = "declared-unrelated";

const FILE_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]{0,60}\.ya?ml$/;
const MIN_REASON = 12;

// Release machinery a declared-unrelated workflow must never run. Matched on the whole file, comments included: a comment cannot hide a command.
const RELEASE_MACHINERY = [
  [/\bright-release\b/, "right-release"],
  [/\bxcodebuild\b[^\n]*(?:\barchive\b|-exportArchive\b|-exportOptionsPlist\b)/, "xcodebuild archive/export"],
  [/\bcodesign\b/, "codesign"],
  [/\bnotarytool\b|\bstapler\b/, "notarization"],
  [/\baltool\b|\biTMSTransporter\b|\bupload-testflight\b|\bapp-store-connect\b/i, "App Store upload"],
  [/\bfastlane\b/, "fastlane"],
  [/\bsecurity\s+import\b|\bapple-actions\//, "Apple signing keychain import"],
  [/\bproductbuild\b|\bpkgbuild\b/, "installer package build"],
  [/\bsigntool\b|\bAzureSignTool\b|\bazure\/(?:trusted|artifact)-signing/i, "Windows code signing"],
  [/\btauri(?:-apps\/tauri-action|\s+build\b)/, "Tauri app build"],
];

// A `uses:` must be a local action (./...), a docker image by digest, or owner/repo[/path]@<40-hex commit>.
const PINNED_USES = /^(?:\.\/[^\s]*|docker:\/\/[^\s@]+@sha256:[0-9a-f]{64}|[A-Za-z0-9_.-]+\/[A-Za-z0-9_./-]+@[0-9a-f]{40})$/;

const normalize = (source) => source.replace(/\r\n?/g, "\n");
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const entryReason = (entry) => (isObject(entry) ? entry.reason : entry);

/** Shape problems of a declared-unrelated hostedWorkflows value (an empty list when it is well formed). */
export function declaredUnrelatedShapeProblems(value) {
  if (!isObject(value)) return ["hostedWorkflows must be a policy string or an object"];
  const problems = [];
  for (const key of Object.keys(value)) if (key !== "policy" && key !== "files") problems.push(`hostedWorkflows.${key} is not a known field (policy, files)`);
  if (value.policy !== DECLARED_UNRELATED_POLICY) problems.push(`hostedWorkflows.policy must be "${DECLARED_UNRELATED_POLICY}"`);
  const files = value.files;
  if (!isObject(files) || Object.keys(files).length === 0) {
    problems.push("hostedWorkflows.files must map each workflow file name to the reason it exists");
    return problems;
  }
  for (const [file, entry] of Object.entries(files)) {
    if (!FILE_NAME.test(file)) problems.push(`hostedWorkflows.files has an invalid workflow file name ${JSON.stringify(file)}`);
    if (isObject(entry)) {
      for (const key of Object.keys(entry)) if (key !== "reason" && key !== "idToken") problems.push(`hostedWorkflows.files["${file}"].${key} is not a known field (reason, idToken)`);
      if (entry.idToken !== undefined && (typeof entry.idToken !== "string" || entry.idToken.trim().length < MIN_REASON)) problems.push(`hostedWorkflows.files["${file}"].idToken must say why the workflow mints an OIDC token (at least ${MIN_REASON} characters)`);
    }
    const reason = entryReason(entry);
    if (typeof reason !== "string" || reason.trim().length < MIN_REASON) problems.push(`hostedWorkflows.files["${file}"] must say why the workflow exists (at least ${MIN_REASON} characters)`);
  }
  return problems;
}

function credentialRefusal(file, source, entry) {
  // GitHub decodes YAML escapes before it evaluates expressions, so an escape could spell `secrets` or a command.
  if (/\\[xuU]/.test(source)) return `${file} contains a \\x, \\u or \\U escape; a declared-unrelated workflow must spell every name literally`;
  // The only secret is GITHUB_TOKEN; any other use of the word (secrets.X, secrets['X'], secrets: inherit, toJSON(secrets),
  // join(secrets)) is refused. A path segment such as /run/secrets/ is not the context.
  const rest = source.replace(/\bsecrets\.GITHUB_TOKEN\b/g, "");
  if (/(?<![\w/-])secrets\b(?![\w/-])/i.test(rest)) return `${file} references secrets other than GITHUB_TOKEN (or passes the secrets context on); a declared-unrelated workflow can hold no signing or store credential`;
  for (const [, ref] of source.matchAll(/^\s*(?:-\s+)?uses:\s*["']?([^\s"'#]+)/gm)) {
    if (/\.github\/workflows\//.test(ref)) return `${file} calls the reusable workflow ${ref}; a called workflow is not checked`;
    if (!PINNED_USES.test(ref)) return `${file} uses ${ref}, which is not pinned to a full commit sha (or a local action, or a docker image by digest)`;
  }
  if (/^\s*id-token\s*:\s*write\b/m.test(source) && !(isObject(entry) && entry.idToken)) return `${file} mints an OIDC token (id-token: write) without declaring why; set hostedWorkflows.files["${file}"] to { reason, idToken: "<why>" }`;
  return null;
}

/**
 * Why a declared-unrelated workflow set is refused, or null when it is admitted.
 * workflows: the file names present in .github/workflows; read(file): its text; artifacts: release artifact paths
 * from the config's targets (a workflow that names one could produce or ship it).
 */
export function declaredUnrelatedRefusal({ declaration, workflows, read, artifacts = [] }) {
  const shape = declaredUnrelatedShapeProblems(declaration);
  if (shape.length) return shape.join("; ");
  const declared = Object.keys(declaration.files).sort();
  const present = [...workflows].sort();
  const undeclared = present.filter((file) => !declared.includes(file));
  if (undeclared.length) return `undeclared hosted workflow file(s): ${undeclared.join(", ")} (declare each in hostedWorkflows.files with its reason, or remove it)`;
  const missing = declared.filter((file) => !present.includes(file));
  if (missing.length) return `hostedWorkflows.files declares workflow file(s) that do not exist: ${missing.join(", ")}`;
  for (const file of present) {
    const source = normalize(read(file));
    const credential = credentialRefusal(file, source, declaration.files[file]);
    if (credential) return credential;
    for (const [pattern, label] of RELEASE_MACHINERY) {
      if (pattern.test(source)) return `${file} runs release machinery (${label}); the released app builds, signs and uploads only through right-release on its native host`;
    }
    for (const artifact of artifacts) {
      if (artifact && source.includes(artifact)) return `${file} names the release artifact ${artifact}`;
    }
  }
  return null;
}

/** Release artifact paths a config's targets declare (artifacts, iosPackage archive and ipa). */
export function releaseArtifactPaths(config) {
  const paths = new Set();
  for (const target of Object.values(config?.targets ?? {})) {
    for (const artifact of target?.artifacts ?? []) if (typeof artifact === "string") paths.add(artifact);
    for (const key of ["archive", "ipa"]) if (typeof target?.iosPackage?.[key] === "string") paths.add(target.iosPackage[key]);
  }
  return [...paths];
}
