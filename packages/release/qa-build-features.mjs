// Source-level layer of "no test-automation surface ships": a release or update build must not enable the cargo
// features that compile one in. The binary layer (`qa-control-scan.mjs`) cannot see every surface (a stripped
// binary, compressed installers), so right-release also refuses, before it builds anything:
//   - a build command (prePackage, package, native package hook, finalizer) that enables one of the forbidden
//     features, directly (`--features`, `-F`, `-f`, `--features=`, `--all-features`) or through a package.json
//     script it runs (`pnpm run <script>`, followed up to three levels);
//   - an app Cargo.toml whose DEFAULT feature closure reaches one of them, or that lists `devtools` on `tauri`,
//     (`devtools` and rightkit-shell's `content-devtools` are accepted only for an allow-listed app, see devtools-policy.mjs)
//     `qa-control` on `rightkit-qa-driver`, `dev-inspect` on `rightkit-shell`, or `tauri-plugin-wdio-webdriver` as a non-optional dependency.
// Dev builds are unaffected: `development.mjs` never calls this.
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";

export const FORBIDDEN_FEATURES = Object.freeze(["devtools", "content-devtools", "qa-native", "qa-control", "dev-native", "dev-inspect",
  // Per-app test-automation features (M20 owner gap): HeardRight `qa-shot`/`qa-tools`, ViewRight `dev-control`/`bench-native`.
  "qa-shot", "qa-tools", "dev-control", "bench-native",
  // D4 (M21/M23): rightkit-daemon `test-mode` compiles the override selectors; never in a release build.
  "test-mode"]);
// The only forbidden features an allow-listed app may enable: rightkit-shell's `content-devtools` (and the Tauri
// `devtools` it switches on). See devtools-policy.mjs; every other app is refused them.
const DEVTOOLS_FEATURES = Object.freeze(["devtools", "content-devtools"]);
const FORBIDDEN_DEPENDENCY = "tauri-plugin-wdio-webdriver";
const MANAGERS = new Set(["pnpm", "npm", "yarn", "bun", "npx"]);

// "dep:foo" -> "foo"; "tauri/devtools" and "tauri?/devtools" -> "devtools".
function featureName(item) {
  const bare = String(item).trim().replace(/^dep:/, "");
  return bare.includes("/") ? bare.slice(bare.lastIndexOf("/") + 1) : bare;
}
export function isForbiddenFeature(item, { allowDevtools = false } = {}) {
  const text = String(item).trim();
  const name = featureName(text);
  if (allowDevtools && DEVTOOLS_FEATURES.includes(name)) return false;
  return text === `dep:${FORBIDDEN_DEPENDENCY}` || FORBIDDEN_FEATURES.includes(name);
}

export function tokenize(text) {
  return (String(text).match(/"[^"]*"|'[^']*'|[^\s"']+/g) ?? []).map((t) => t.replace(/^["']|["']$/g, ""));
}

// Feature names a token list enables, plus whether it asks for every feature.
export function featuresInTokens(tokens) {
  const enabled = []; let allFeatures = false;
  const take = (value) => { for (const part of String(value).split(/[,\s]+/)) if (part) enabled.push(part); };
  for (let i = 0; i < tokens.length; i += 1) {
    const t = tokens[i];
    if (t === "--all-features") allFeatures = true;
    else if (t === "--features" || t === "-F" || t === "-f" || t === "--feature") { if (i + 1 < tokens.length) take(tokens[i + 1]); }
    else if (t.startsWith("--features=")) take(t.slice("--features=".length));
    else if (/^-F.+/.test(t)) take(t.slice(2));
  }
  return { enabled, allFeatures };
}

export function scriptTarget(tokens) {
  const first = tokens.findIndex((t) => MANAGERS.has(path.basename(t).replace(/\.(cmd|exe)$/i, "")));
  if (first < 0) return null;
  let i = first + 1;
  while (i < tokens.length && tokens[i].startsWith("-")) i += 1;
  if (["run", "run-script"].includes(tokens[i])) { i += 1; while (i < tokens.length && tokens[i].startsWith("-")) i += 1; }
  return tokens[i] ?? null;
}

// Issues (strings) for one command object { cmd, args, env? } or a shell string.
export function commandFeatureIssues(command, { label = "command", scripts = {}, depth = 0, seen = new Set(), allowDevtools = false } = {}) {
  const parts = typeof command === "string" ? [command] : [command?.cmd, ...(command?.args ?? [])].filter((v) => v !== undefined).map(String);
  // Two views: every argument whole (`--features` followed by "a b qa-native"), and every argument re-tokenised as
  // shell text (`sh -c "cargo build --features qa-native"`).
  const views = [parts, parts.flatMap((p) => tokenize(p))];
  const issues = new Set();
  for (const tokens of views) {
    const { enabled, allFeatures } = featuresInTokens(tokens);
    if (allFeatures) issues.add(`${label}: --all-features enables every feature including ${FORBIDDEN_FEATURES.join(", ")}`);
    for (const feature of enabled) if (isForbiddenFeature(feature, { allowDevtools })) issues.add(`${label}: enables feature \`${feature}\``);
  }
  if (command && typeof command === "object") {
    for (const [key, value] of Object.entries(command.env ?? {})) {
      if (/FEATURES$/i.test(key)) for (const feature of String(value).split(/[,\s]+/)) if (feature && isForbiddenFeature(feature, { allowDevtools })) issues.add(`${label}: env ${key} enables feature \`${feature}\``);
    }
  }
  const script = views.map(scriptTarget).find(Boolean);
  if (script && depth < 3 && Object.hasOwn(scripts, script) && !seen.has(script)) {
    for (const issue of commandFeatureIssues(String(scripts[script]), { label: `${label} -> package script \`${script}\``, scripts, depth: depth + 1, seen: new Set([...seen, script]), allowDevtools })) issues.add(issue);
  }
  return [...issues];
}

// ---- Cargo.toml ---------------------------------------------------------------------------------------------
function stripComments(text) {
  let out = ""; let quote = null;
  for (let i = 0; i < text.length; i += 1) {
    const c = text[i];
    if (quote) { out += c; if (c === quote && text[i - 1] !== "\\") quote = null; continue; }
    if (c === '"' || c === "'") { quote = c; out += c; continue; }
    if (c === "#") { while (i < text.length && text[i] !== "\n") i += 1; out += "\n"; continue; }
    out += c;
  }
  return out;
}

function sectionsOf(text) {
  const sections = []; let current = { name: "", body: "" };
  for (const line of text.split(/\r?\n/)) {
    const header = line.match(/^\s*\[\s*([^\[\]]+?)\s*\]\s*$/);
    if (header) { sections.push(current); current = { name: header[1].replace(/\s+/g, ""), body: "" }; }
    else current.body += `${line}\n`;
  }
  sections.push(current);
  return sections;
}

function balanced(text, from, open, close) {
  let depth = 0; let quote = null;
  for (let i = from; i < text.length; i += 1) {
    const c = text[i];
    if (quote) { if (c === quote && text[i - 1] !== "\\") quote = null; continue; }
    if (c === '"' || c === "'") quote = c;
    else if (c === open) depth += 1;
    else if (c === close) { depth -= 1; if (depth === 0) return text.slice(from, i + 1); }
  }
  return text.slice(from);
}

const strings = (text) => [...String(text).matchAll(/"([^"]*)"|'([^']*)'/g)].map((m) => m[1] ?? m[2]);
const unquote = (key) => key.replace(/^["']|["']$/g, "");

export function parseCargoFeaturesAndDependencies(manifestText) {
  const sections = sectionsOf(stripComments(manifestText));
  const features = new Map(); const dependencies = new Map();
  for (const { name, body } of sections) {
    if (name === "features") {
      for (const m of body.matchAll(/^\s*([A-Za-z0-9_\-"']+)\s*=\s*\[/gm)) {
        features.set(unquote(m[1]), strings(balanced(body, m.index + m[0].length - 1, "[", "]")));
      }
    } else if (/^(?:target\.[^\n]+\.)?dependencies$/.test(name)) {
      for (const m of body.matchAll(/^\s*([A-Za-z0-9_\-"']+)\s*=\s*(\{)?/gm)) {
        const depName = unquote(m[1]);
        const spec = m[2] ? balanced(body, m.index + m[0].length - 1, "{", "}") : "";
        dependencies.set(depName, dependencyInfo(spec));
      }
    } else {
      const table = name.match(/^(?:target\.[^\n]+\.)?dependencies\.([A-Za-z0-9_\-]+)$/);
      if (table) dependencies.set(table[1], dependencyInfo(body));
    }
  }
  return { features, dependencies };
}
function dependencyInfo(spec) {
  const list = spec.match(/features\s*=\s*(\[)/);
  return {
    optional: /optional\s*=\s*true/.test(spec),
    features: list ? strings(balanced(spec, list.index + list[0].length - 1, "[", "]")) : [],
  };
}

export function cargoManifestIssues(manifestText, label = "Cargo.toml", { allowDevtools = false } = {}) {
  const { features, dependencies } = parseCargoFeaturesAndDependencies(manifestText);
  const issues = [];
  const tauri = dependencies.get("tauri");
  if (!allowDevtools && tauri?.features.includes("devtools")) issues.push(`${label}: dependency \`tauri\` lists feature \`devtools\` unconditionally (inspector ships in release builds)`);
  // The crate was renamed from rightkit-control; apps still on the old published name must stay covered.
  for (const name of ["rightkit-qa-driver", "rightkit-control"]) {
    const control = dependencies.get(name);
    if (control && !control.optional && control.features.includes("qa-control")) issues.push(`${label}: dependency \`${name}\` lists \`qa-control\` unconditionally`);
  }
  const shell = dependencies.get("rightkit-shell");
  if (shell && !shell.optional && shell.features.includes("dev-inspect")) issues.push(`${label}: dependency \`rightkit-shell\` lists \`dev-inspect\` unconditionally (developer inspection ships in release builds)`);
  const service = dependencies.get("rightkit-daemon");
  if (service && !service.optional && service.features.includes("test-mode")) issues.push(`${label}: dependency \`rightkit-daemon\` lists \`test-mode\` unconditionally (test-mode capability ships in release builds)`);
  const wdio = dependencies.get(FORBIDDEN_DEPENDENCY);
  if (wdio && !wdio.optional) issues.push(`${label}: dependency \`${FORBIDDEN_DEPENDENCY}\` is not optional`);
  const reached = new Set(); const stack = [...(features.get("default") ?? [])];
  while (stack.length) {
    const item = stack.pop();
    if (reached.has(item)) continue;
    reached.add(item);
    if (isForbiddenFeature(item, { allowDevtools })) issues.push(`${label}: default feature set reaches \`${item}\``);
    if (features.has(item)) stack.push(...features.get(item));
  }
  return issues;
}

// commands: [{ label, command }]. Reads package.json scripts and the app Cargo manifests under `root`.
// `devtools`: the allowance resolved by devtools-policy.mjs (mode "content-only") for an allow-listed app, else undefined.
// Single manifest resolver shared by the closure gate (release-switch-scan.mjs) and the default-feature check below:
// targets.<os>.cargoManifest -> releaseSwitches.manifest -> src-tauri/Cargo.toml -> Cargo.toml. `rel` is the first
// candidate that exists (undefined when none does); `declared` are the configured paths.
export function resolveCargoManifestPath({ root, cargoManifest, manifest } = {}) {
  const declared = [cargoManifest, manifest].filter(Boolean);
  const candidates = [...declared, path.join("src-tauri", "Cargo.toml"), "Cargo.toml"];
  return { rel: candidates.find((c) => existsSync(path.join(root, c))), declared, candidates };
}

export function buildFeatureIssues({ root, commands, devtools, cargoManifest, manifest }) {
  const allowDevtools = devtools?.mode === "content-only";
  let scripts = {};
  try { scripts = JSON.parse(readFileSync(path.join(root, "package.json"), "utf8")).scripts ?? {}; } catch { /* no package.json */ }
  const issues = [];
  for (const { label, command } of commands) if (command) issues.push(...commandFeatureIssues(command, { label, scripts, allowDevtools }));
  const { rel } = resolveCargoManifestPath({ root, cargoManifest, manifest });
  if (rel) issues.push(...cargoManifestIssues(readFileSync(path.join(root, rel), "utf8"), rel, { allowDevtools }));
  return issues;
}

export function assertNoForbiddenBuildFeatures({ root, commands, tier = "release", devtools, cargoManifest, manifest }) {
  const issues = buildFeatureIssues({ root, commands, devtools, cargoManifest, manifest });
  if (issues.length === 0) return;
  throw new Error(
    `${tier} build refused: it enables a test-automation or debugging feature (${FORBIDDEN_FEATURES.join(", ")}); none may ship:\n- ${issues.join("\n- ")}\n`
    + "Remove the feature from the release build command, the app's default features and its tauri dependency; dev builds (`right-release dev`) may keep it. "
    + "No app may ship devtools unless it is on the allow-list in devtools-policy.json and declares `devtools: \"content-only\"` (rightkit-shell DevToolsPolicy::ContentOnly).",
  );
}
