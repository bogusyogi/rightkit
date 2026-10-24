// hostedWorkflows: { policy: "declared-unrelated", files: { "<file>.yml": "<reason>" } }
//
// For a repository that ships more than the released app (a monorepo whose web or server pipeline is a hand-written
// GitHub workflow). The released app still builds, signs and uploads only through right-release on its native host;
// every hosted workflow must be declared with the reason it exists, and none may be able to build, sign or upload a
// release. The guarantee is the secrets rule: a workflow that can reach no secret but GITHUB_TOKEN holds no Apple,
// Azure or store credential, so it cannot sign or upload. The command and artifact patterns are a denylist backstop.
export const DECLARED_UNRELATED_POLICY = "declared-unrelated";

const FILE_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]{0,60}\.ya?ml$/;
const ALLOWED_SECRETS = new Set(["GITHUB_TOKEN"]);

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

const normalize = (source) => source.replace(/\r\n?/g, "\n");

/** Shape problems of a declared-unrelated hostedWorkflows value (an empty list when it is well formed). */
export function declaredUnrelatedShapeProblems(value) {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return ["hostedWorkflows must be a policy string or an object"];
  const problems = [];
  const keys = Object.keys(value);
  for (const key of keys) if (key !== "policy" && key !== "files") problems.push(`hostedWorkflows.${key} is not a known field (policy, files)`);
  if (value.policy !== DECLARED_UNRELATED_POLICY) problems.push(`hostedWorkflows.policy must be "${DECLARED_UNRELATED_POLICY}"`);
  const files = value.files;
  if (files === null || typeof files !== "object" || Array.isArray(files) || Object.keys(files).length === 0) {
    problems.push("hostedWorkflows.files must map each workflow file name to the reason it exists");
    return problems;
  }
  for (const [file, reason] of Object.entries(files)) {
    if (!FILE_NAME.test(file)) problems.push(`hostedWorkflows.files has an invalid workflow file name ${JSON.stringify(file)}`);
    if (typeof reason !== "string" || reason.trim().length < 12) problems.push(`hostedWorkflows.files["${file}"] must say why the workflow exists (at least 12 characters)`);
  }
  return problems;
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
    const secrets = [...new Set([...source.matchAll(/\bsecrets\s*(?:\.\s*([A-Za-z0-9_]+)|\[)/g)].map((match) => match[1] ?? "["))];
    const extra = secrets.filter((name) => !ALLOWED_SECRETS.has(name));
    if (extra.length) return `${file} references ${extra.includes("[") ? "the secrets context by index" : `secrets ${extra.join(", ")}`}; a declared-unrelated workflow may use only GITHUB_TOKEN, so it can hold no signing or store credential`;
    if (/\btoJSON\s*\(\s*secrets\s*\)|\$\{\{\s*secrets\s*\}\}/i.test(source)) return `${file} expands the whole secrets context`;
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
