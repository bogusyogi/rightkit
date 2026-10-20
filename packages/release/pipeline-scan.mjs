// M24 `--enforce` inventory: find every way an app can own, or indirectly start, a build, sign, notarize, package or upload step.
//
// Sources read (R2 framework hooks, R3 tool inventory, R1 alias traversal):
//   - the release config (`right-release.config.json`: every command admitted by R1; a legacy `.mjs` config is a finding and is never opened),
//   - package.json scripts and npm lifecycle hooks, with aliases followed to the helper files they reach,
//   - helper and pipeline scripts under scripts/, tools/, ci/, build/, bin/, packaging/ and the app root,
//   - GitHub workflows (a `# Managed by right-git` workflow is RightKit-owned and only inventoried),
//   - Tauri before-dev / before-build / before-bundle hooks and signing keys in tauri.conf.json,
//   - Cargo `build.rs` (signing or upload is forbidden; spawning an interpreter or RightKit is a generator that must be admitted),
//   - Xcode script phases (`project.pbxproj`, XcodeGen `project.yml`).
// A finding names its file, line, stage and ownership (`app-owned` when the app wrote the pipeline step, `unknown` when ownership could
// not be established, which blocks exactly like an app-owned step). Every finding blocks `--enforce`. This is a static scan: it cannot
// prove native behaviour, and ordinary product runtime code (src/, crates) is never read except build.rs.
import { existsSync, lstatSync, readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { admitHookString, admitTauriHook } from "./pipeline-admission.mjs";
import { loadGeneratorRegistry, makeGeneratorAdmitter } from "./pipeline-generators.mjs";
import { ownershipOfCode, ruleOfCode, validateConfigPipeline } from "./pipeline-config.mjs";
import { classifyAction, dispatchesRightKit, scanTextForPipelineTools, segmentTokens, splitSegments } from "./pipeline-tools.mjs";
import { LEGACY_RELEASE_CONFIG_FILE, RELEASE_CONFIG_FILE, loadReleaseConfigSync } from "./release-config.mjs";
import { PIPELINE_DISPATCH_SUBCOMMANDS } from "./pipeline-admission.mjs";

export const PIPELINE_SCAN_SCHEMA = 1;
const MAX_FILE_BYTES = 2 * 1024 * 1024;
const MAX_FILES = 5000;
const SKIP_DIRS = new Set(["node_modules", "target", "dist", ".git", ".cache", ".right-release", ".pnpm-store", "coverage", "Pods", "DerivedData", ".build", ".swiftpm"]);
const SCRIPT_DIRS = ["scripts", "script", "tools", "tool", "ci", ".ci", "build", "bin", "packaging", "installer", "installers", "release", "deploy", ".husky", ".githooks"];
const SHELL_EXT = /\.(?:sh|ps1|bat|cmd|command|bash|zsh)$/i;
const CODE_EXT = /\.(?:mjs|cjs|js|ts|mts|py|rb)$/i;
const HELPER_REF = /\.(?:sh|ps1|bat|cmd|command|bash|zsh|mjs|cjs|js|py)$/i;
const MANAGED_MARKER = /#\s*Managed by right-git/i;
const AUTO_LIFECYCLE = new Set(["preinstall", "install", "postinstall", "prepublish", "preprepare", "prepare", "postprepare", "prepack", "postpack", "prepublishOnly", "publish", "postpublish"]);
const posix = (value) => value.split(path.sep).join("/");

const defaultFs = {
  readText: (file) => readFileSync(file, "utf8"),
  exists: (file) => existsSync(file),
  readdir: (dir) => readdirSync(dir, { withFileTypes: true }).map((entry) => ({ name: entry.name, isDirectory: entry.isDirectory(), isSymlink: entry.isSymbolicLink(), isFile: entry.isFile() })),
  size: (file) => lstatSync(file).size,
};

const lineAt = (text, index) => { let line = 1; for (let i = 0; i < index && i < text.length; i += 1) if (text[i] === "\n") line += 1; return line; };

/** Run the inventory for one app. `fs` is injectable. Returns `{ schema, app, root, findings, inventory, scanned }`. */
export function scanApp({ appRoot, registry, fs = defaultFs } = {}) {
  const root = path.resolve(appRoot);
  const findings = [];
  const inventory = [];
  const visited = new Set();
  let budget = MAX_FILES;
  const rel = (file) => posix(path.relative(root, file));
  const add = (finding) => findings.push({ severity: "block", ownership: ownershipOfCode(finding.code), rule: ruleOfCode(finding.code), line: 1, ...finding });
  const read = (file, label = rel(file)) => {
    if (budget <= 0) { add({ code: "R0-too-many-files", file: label, stage: "scan", message: `more than ${MAX_FILES} files reached; the inventory is incomplete and unknown ownership blocks` }); return null; }
    budget -= 1;
    try {
      if (fs.size && fs.size(file) > MAX_FILE_BYTES) { add({ code: "R0-unreadable", file: label, stage: "scan", message: `${label} is larger than ${MAX_FILE_BYTES} bytes and cannot be inspected` }); return null; }
      return fs.readText(file);
    } catch (error) {
      add({ code: "R0-unreadable", file: label, stage: "scan", message: `${label} cannot be read (${error.message})` });
      return null;
    }
  };

  // ---- package.json ------------------------------------------------------------------------------------------------------------
  let scripts;
  let packageText = "";
  const packageFile = path.join(root, "package.json");
  if (fs.exists(packageFile)) {
    packageText = read(packageFile) ?? "";
    try {
      const parsed = JSON.parse(packageText || "{}");
      if (parsed.scripts && typeof parsed.scripts === "object" && !Array.isArray(parsed.scripts)) scripts = parsed.scripts;
    } catch (error) { add({ code: "R0-package-json", file: "package.json", stage: "scan", message: `package.json is not valid JSON (${error.message}); script ownership is unknown` }); }
  }

  // ---- release config (R1) -----------------------------------------------------------------------------------------------------
  const configFile = path.join(root, RELEASE_CONFIG_FILE);
  const legacyFile = path.join(root, LEGACY_RELEASE_CONFIG_FILE);
  let config;
  if (fs.exists(legacyFile)) {
    add({ code: "R0-legacy-config", file: LEGACY_RELEASE_CONFIG_FILE, stage: "config", ownership: "app-owned", message: `${LEGACY_RELEASE_CONFIG_FILE} is an executable release config; it is never imported, evaluated or converted. Replace it with ${RELEASE_CONFIG_FILE}.` });
    inventory.push({ kind: "config", file: LEGACY_RELEASE_CONFIG_FILE, ownership: "app-owned-executable" });
  }
  let admitGenerator;
  const useRegistry = registry ?? (() => { try { return loadGeneratorRegistry(); } catch { return undefined; } })();
  if (fs.exists(configFile)) {
    inventory.push({ kind: "config", file: RELEASE_CONFIG_FILE, ownership: "rightkit-schema" });
    try {
      const loaded = loadReleaseConfigSync(configFile, { validatePipeline: false, readText: (target) => fs.readText(target) });
      config = loaded.config;
      for (const finding of validateConfigPipeline(config, { scripts, registry: useRegistry, appRoot: root, lineOf: loaded.lineOf })) {
        add({ code: finding.code, file: RELEASE_CONFIG_FILE, line: finding.line, stage: finding.stage, ownership: finding.ownership, message: `${finding.pointer}: ${finding.message}` });
      }
    } catch (error) {
      add({ code: `R0-${String(error.code ?? "config-invalid").toLowerCase().replace(/^release_config_/, "config-").replaceAll("_", "-")}`, file: RELEASE_CONFIG_FILE, line: error.line ?? 1, stage: "config", ownership: "unknown", message: error.message });
    }
  } else if (!fs.exists(legacyFile)) {
    add({ code: "R0-config-missing", file: RELEASE_CONFIG_FILE, stage: "config", ownership: "unknown", message: `${RELEASE_CONFIG_FILE} is missing: the app's pipeline ownership cannot be established` });
  }
  if (useRegistry) admitGenerator = makeGeneratorAdmitter({ registry: useRegistry, app: config?.app ?? path.basename(root), appRoot: root, readBytes: (file) => Buffer.from(fs.readText(file), "utf8") });
  const aliasOptions = { scripts, admitGenerator };

  // ---- helper queue (scripts reachable from pipeline entries, scripts directories) ----------------------------------------------
  const queue = [];
  const enqueue = (file, via) => { const key = path.resolve(file); if (!visited.has(key)) { visited.add(key); queue.push({ file: key, via }); } };
  const refsIn = (text) => {
    const refs = new Set();
    for (const line of String(text).split(/\r?\n/)) {
      for (const segment of splitSegments(line)) {
        for (const token of segmentTokens(segment)) {
          if (!HELPER_REF.test(token) || token.startsWith("-") || /^[a-z]+:\/\//i.test(token)) continue;
          const normalized = token.replaceAll("\\", "/").replace(/^(?:\.\/)+/, "");
          if (path.posix.isAbsolute(normalized) || /^[A-Za-z]:/.test(normalized) || normalized.split("/").includes("..") || normalized.includes("$") || normalized.includes("*")) continue;
          refs.add(normalized);
        }
      }
    }
    return [...refs];
  };

  if (scripts) {
    const scriptNames = Object.keys(scripts);
    for (const name of scriptNames) {
      const body = scripts[name];
      const lineOfScript = () => { const at = packageText.search(new RegExp(`"${name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}"\\s*:`)); return at >= 0 ? lineAt(packageText, at) : 1; };
      if (typeof body !== "string") { add({ code: "R1-alias-unknown", file: "package.json", line: lineOfScript(), stage: "script", message: `package script "${name}" is not a string` }); continue; }
      inventory.push({ kind: "package-script", file: "package.json", name, ownership: "app-script" });
      for (const hit of scanTextForPipelineTools(body, { kind: "shell" })) {
        add({ code: `R3-${hit.id}`, file: "package.json", line: lineOfScript(), stage: hit.stage, ownership: "app-owned", tool: hit.tool, message: `script "${name}" runs ${hit.tool} (${hit.stage}); build, sign, notarize, package and upload run only inside RightKit (right-release)` });
      }
      const lifecycle = AUTO_LIFECYCLE.has(name) || (/^(?:pre|post)(.+)$/.test(name) && scriptNames.includes(name.replace(/^(?:pre|post)/, "")));
      if (lifecycle) {
        const admitted = admitHookString(body, aliasOptions, [name]);
        if (!admitted.admitted) add({ code: admitted.code.replace(/^R\d/, "R2"), file: "package.json", line: lineOfScript(), stage: "hook", message: `npm lifecycle hook "${name}": ${admitted.reason}` });
      }
      for (const ref of refsIn(body)) enqueue(path.join(root, ref), `package.json scripts.${name}`);
    }
  }

  // ---- script directories and root-level scripts -------------------------------------------------------------------------------
  const walkDir = (dir, depth) => {
    let entries;
    try { entries = fs.readdir(dir); } catch { return; }
    for (const entry of entries) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory) { if (depth > 0 && !SKIP_DIRS.has(entry.name)) walkDir(full, depth - 1); continue; }
      if (!(SHELL_EXT.test(entry.name) || CODE_EXT.test(entry.name) || /^(?:Makefile|justfile)$/i.test(entry.name))) continue;
      if (entry.isSymlink) { add({ code: "R0-symlink", file: rel(full), stage: "scan", message: `${rel(full)} is a symlink; a linked script's owner cannot be established` }); continue; }
      enqueue(full, `pipeline directory ${rel(dir) || "."}`);
    }
  };
  for (const dir of SCRIPT_DIRS) if (fs.exists(path.join(root, dir))) walkDir(path.join(root, dir), 4);
  try {
    for (const entry of fs.readdir(root)) {
      if (entry.isDirectory || entry.isSymlink) continue;
      if (SHELL_EXT.test(entry.name) || /^(?:Makefile|justfile)$/i.test(entry.name) || /^(?:package|build|release|sign|notarize|install|deploy)[-_.a-z0-9]*\.(?:mjs|cjs|js|py)$/i.test(entry.name)) enqueue(path.join(root, entry.name), "app root");
    }
  } catch { /* an unreadable root is reported by the reads above */ }

  // The queue grows while it drains: a helper's own references join it.
  for (let index = 0; index < queue.length; index += 1) {
    const { file, via } = queue[index];
    const label = rel(file);
    if (!fs.exists(file)) { add({ code: "R1-helper-missing", file: label, stage: "script", ownership: "unknown", message: `${label} is reached from ${via} but does not exist; an unknown reachable script blocks` }); continue; }
    const text = read(file, label);
    if (text === null) continue;
    const isShell = SHELL_EXT.test(file) || /(?:Makefile|justfile)$/i.test(file);
    inventory.push({ kind: "helper-script", file: label, via, ownership: "app-owned" });
    for (const hit of scanTextForPipelineTools(text, { kind: isShell ? "shell" : "code" })) {
      add({ code: `R3-${hit.id}`, file: label, line: hit.line, stage: hit.stage, ownership: "app-owned", tool: hit.tool, message: `${label} (from ${via}) runs ${hit.tool} (${hit.stage}); this is an app-owned pipeline script` });
    }
    if (isShell) for (const ref of refsIn(text)) enqueue(path.resolve(path.dirname(file), ref).startsWith(root) && fs.exists(path.resolve(path.dirname(file), ref)) ? path.resolve(path.dirname(file), ref) : path.join(root, ref), `${label}`);
  }

  // ---- GitHub workflows --------------------------------------------------------------------------------------------------------
  const workflowDir = path.join(root, ".github", "workflows");
  if (fs.exists(workflowDir)) {
    for (const entry of fs.readdir(workflowDir)) {
      if (entry.isDirectory || !/\.ya?ml$/i.test(entry.name)) continue;
      const file = path.join(workflowDir, entry.name);
      const label = rel(file);
      const text = read(file, label);
      if (text === null) continue;
      const managed = MANAGED_MARKER.test(text.split(/\r?\n/).slice(0, 10).join("\n"));
      inventory.push({ kind: "workflow", file: label, ownership: managed ? "rightkit-managed" : "app-owned" });
      if (managed) continue;
      for (const hit of scanTextForPipelineTools(text, { kind: "shell" })) {
        add({ code: `R3-${hit.id}`, file: label, line: hit.line, stage: hit.stage, ownership: "app-owned", tool: hit.tool, message: `workflow ${label} runs ${hit.tool} (${hit.stage}); pipeline workflows are generated and owned by right-git (carry the "Managed by right-git" marker)` });
      }
      text.split(/\r?\n/).forEach((lineText, i) => {
        const uses = /^\s*(?:-\s+)?uses\s*:\s*(\S+)/.exec(lineText);
        const hit = uses ? classifyAction(uses[1]) : null;
        if (hit) add({ code: `R3-${hit.id}`, file: label, line: i + 1, stage: hit.stage, ownership: "app-owned", tool: hit.tool, message: `workflow ${label} uses ${hit.tool}, an action that ${hit.stage}s on its own` });
      });
    }
  }

  // ---- framework hooks: Tauri, build.rs, Xcode --------------------------------------------------------------------------------
  const projectFiles = [];
  const find = (dir, depth) => {
    let entries;
    try { entries = fs.readdir(dir); } catch { return; }
    for (const entry of entries) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory) {
        if (/\.xcodeproj$/i.test(entry.name)) { projectFiles.push({ kind: "xcode", file: path.join(full, "project.pbxproj") }); continue; }
        if (depth > 0 && !SKIP_DIRS.has(entry.name) && !entry.name.startsWith(".")) find(full, depth - 1);
        continue;
      }
      if (/^tauri(?:\.[A-Za-z0-9_-]+)?\.conf\.json$/i.test(entry.name)) projectFiles.push({ kind: "tauri", file: full });
      else if (entry.name === "build.rs") projectFiles.push({ kind: "build-rs", file: full });
      else if (entry.name === "project.yml" || entry.name === "project.yaml") projectFiles.push({ kind: "xcodegen", file: full });
    }
  };
  find(root, 4);
  for (const item of projectFiles) {
    if (!fs.exists(item.file)) continue;
    const label = rel(item.file);
    const text = read(item.file, label);
    if (text === null) continue;
    if (item.kind === "tauri") scanTauriConf(text, label);
    else if (item.kind === "build-rs") scanBuildRs(text, label);
    else scanXcode(text, label, item.kind);
  }

  function scanTauriConf(text, label) {
    inventory.push({ kind: "tauri-config", file: label, ownership: "framework-config" });
    let parsed;
    try { parsed = JSON.parse(text); } catch (error) { add({ code: "R0-unreadable", file: label, stage: "hook", message: `${label} is not valid JSON (${error.message}); its hooks are unknown` }); return; }
    const keyLine = (key) => { const at = text.search(new RegExp(`"${key}"\\s*:`)); return at >= 0 ? lineAt(text, at) : 1; };
    for (const key of ["beforeDevCommand", "beforeBuildCommand", "beforeBundleCommand"]) {
      const entry = parsed?.build?.[key];
      if (entry === undefined || entry === null || entry === "") continue;
      const problem = admitTauriHook(entry, aliasOptions, `${label} build.${key}`);
      if (problem) add({ code: problem.code.replace(/^R\d/, "R2"), file: label, line: keyLine(key), stage: "hook", message: problem.reason });
    }
    const windows = parsed?.bundle?.windows ?? {};
    if (windows.signCommand !== undefined && windows.signCommand !== null) add({ code: "R3-tauri-sign-hook", file: label, line: keyLine("signCommand"), stage: "sign", ownership: "app-owned", message: `${label} bundle.windows.signCommand is a sign hook; signing is RightKit's` });
    if (typeof windows.certificateThumbprint === "string" && windows.certificateThumbprint) add({ code: "R3-tauri-sign-config", file: label, line: keyLine("certificateThumbprint"), stage: "sign", ownership: "app-owned", message: `${label} bundle.windows.certificateThumbprint selects a signing identity; signing is RightKit's` });
    const identity = parsed?.bundle?.macOS?.signingIdentity;
    if (typeof identity === "string" && identity !== "-" && identity !== "") add({ code: "R3-tauri-sign-config", file: label, line: keyLine("signingIdentity"), stage: "sign", ownership: "app-owned", message: `${label} bundle.macOS.signingIdentity selects a signing identity; signing is RightKit's` });
  }

  function scanBuildRs(text, label) {
    inventory.push({ kind: "build-rs", file: label, ownership: "product-compile" });
    for (const hit of scanTextForPipelineTools(text, { kind: "code" })) {
      add({ code: `R2-build-rs-${hit.id}`, file: label, line: hit.line, stage: hit.stage, ownership: "app-owned", tool: hit.tool, message: `${label} runs ${hit.tool} (${hit.stage}); signing, packaging or upload from build.rs is forbidden` });
    }
    text.split(/\r?\n/).forEach((lineText, i) => {
      if (/^\s*\/\//.test(lineText)) return;
      const spawn = /Command::new\(\s*"([^"]+)"/.exec(lineText);
      if (!spawn) return;
      const name = path.basename(spawn[1]).toLowerCase().replace(/\.(?:exe|cmd|bat)$/, "");
      if (name === "rightkit" || name === "right-release") add({ code: "R2-build-rs-dispatch", file: label, line: i + 1, stage: "hook", ownership: "app-owned", message: `${label} starts ${name} from build.rs; a generator cannot dispatch another pipeline` });
      else if (["node", "python", "python3", "py", "bash", "sh", "zsh", "pwsh", "powershell", "cmd", "pnpm", "npm", "npx", "yarn", "deno", "bun", "ruby", "perl"].includes(name)) add({ code: "R4-build-rs-generator", file: label, line: i + 1, stage: "generator", ownership: "unknown", message: `${label} runs ${name} from build.rs; a pipeline generator must be an admitted shared step (pipeline-generators.json), not app code` });
    });
  }

  function scanXcode(text, label, kind) {
    inventory.push({ kind: kind === "xcode" ? "xcode-project" : "xcodegen-spec", file: label, ownership: "framework-config" });
    let bodies;
    if (kind === "xcode") {
      bodies = [...text.matchAll(/shellScript\s*=\s*"((?:[^"\\]|\\.)*)"\s*;/g)].map((match) => ({ line: lineAt(text, match.index), script: match[1].replace(/\\n/g, "\n").replace(/\\t/g, "\t").replace(/\\"/g, '"').replace(/\\\\/g, "\\") }));
    } else bodies = [{ line: 1, script: text }];
    for (const body of bodies) {
      for (const hit of scanTextForPipelineTools(body.script, { kind: "shell" })) {
        add({ code: `R2-xcode-script-${hit.id}`, file: label, line: kind === "xcode" ? body.line : hit.line, stage: hit.stage, ownership: "app-owned", tool: hit.tool, message: `${label} script phase runs ${hit.tool} (${hit.stage}); archive, export, signing and upload belong to RightKit` });
      }
      for (const segment of body.script.split(/\r?\n/).flatMap(splitSegments)) {
        const tokens = segmentTokens(segment);
        if (dispatchesRightKit(tokens) && (tokens[0] === "rightkit" || PIPELINE_DISPATCH_SUBCOMMANDS.has(tokens[1]))) add({ code: "R2-xcode-script-dispatch", file: label, line: kind === "xcode" ? body.line : 1, stage: "hook", ownership: "app-owned", message: `${label} script phase starts a RightKit pipeline run; a hook cannot dispatch another pipeline` });
      }
    }
  }

  findings.sort((a, b) => (a.file < b.file ? -1 : a.file > b.file ? 1 : a.line - b.line));
  return { schema: PIPELINE_SCAN_SCHEMA, app: config?.app ?? path.basename(root), root, findings, inventory, scanned: visited.size };
}

export function formatScan(report) {
  const lines = [`M24 pipeline scan: ${report.app} (${report.root}) - ${report.findings.length} blocking finding(s), ${report.inventory.length} inventoried entrypoint(s)`];
  for (const finding of report.findings) lines.push(`  ${finding.file}:${finding.line} [${finding.code}] ${finding.stage} (${finding.ownership}) ${finding.message}`);
  return `${lines.join("\n")}\n`;
}
