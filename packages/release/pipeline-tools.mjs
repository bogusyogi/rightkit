// M24 R3: the build, sign, notarize, package and upload tool inventory, and the text scanner that finds those tools in a pipeline
// context (an app script, a package hook, a workflow step, a framework hook, a build.rs). Pure functions: no file access.
//
// Scope (Astra, M24 R3): the tools below are tracked when a pipeline context or a helper reachable from one runs them. Read-only
// forms (`gh release view`, `codesign --verify`, `signtool verify`, `stapler validate`, `aws s3 ls`, ...) are not an upload or a
// signature and are never reported. This is a static text scan: it proves nothing about what a native tool does, and ordinary
// product runtime code (src/, crates) is never scanned by it, so there is no blanket ban on a product that happens to sign
// or package something at runtime.

/** The tool ids R3 inventories, with the pipeline stage each one acts in. */
export const PIPELINE_TOOL_IDS = Object.freeze([
  "codesign", "notarytool", "altool", "stapler", "signtool", "azuresigntool", "productbuild", "pkgbuild", "hdiutil", "create-dmg",
  "makensis", "iscc", "wix", "wrangler", "rclone", "aws-s3", "gh-release", "cargo-publish", "npm-publish", "pnpm-publish",
  "tauri-build", "xcode-archive",
]);

const READ_ONLY = (list) => (rest) => list.includes(String(rest[0] ?? "").toLowerCase());

/** name (lowercased basename without a script or exe extension) -> classifier of the remaining argument tokens. */
const RULES = [
  { id: "codesign", stage: "sign", names: ["codesign"], flagged: (rest) => {
    const flags = rest.filter((arg) => arg.startsWith("-"));
    const signs = flags.some((arg) => /^(?:-s|--sign|-f|--force|--remove-signature)$/.test(arg));
    const readsOnly = flags.some((arg) => /^(?:--verify|-v+|-d+v*|--display|--test-requirement|-r+-?|-h|--help)$/.test(arg));
    return signs || !readsOnly;
  } },
  { id: "notarytool", stage: "notarize", names: ["notarytool"], flagged: (rest) => !READ_ONLY(["info", "log", "history", "--help", "--version", "help"])(rest) },
  { id: "altool", stage: "notarize", names: ["altool"], flagged: () => true },
  { id: "stapler", stage: "notarize", names: ["stapler"], flagged: (rest) => !READ_ONLY(["validate", "--help", "help"])(rest) },
  { id: "signtool", stage: "sign", names: ["signtool"], flagged: (rest) => !READ_ONLY(["verify", "/?", "-?", "--help"])(rest) },
  { id: "azuresigntool", stage: "sign", names: ["azuresigntool", "trusted-signing-cli"], flagged: () => true },
  { id: "productbuild", stage: "package", names: ["productbuild", "productsign"], flagged: () => true },
  { id: "pkgbuild", stage: "package", names: ["pkgbuild"], flagged: () => true },
  { id: "hdiutil", stage: "package", names: ["hdiutil"], flagged: (rest) => READ_ONLY(["create", "convert", "makehybrid", "resize", "compact", "burn"])(rest) },
  { id: "create-dmg", stage: "package", names: ["create-dmg"], flagged: () => true },
  { id: "makensis", stage: "package", names: ["makensis"], flagged: () => true },
  { id: "iscc", stage: "package", names: ["iscc"], flagged: () => true },
  { id: "wix", stage: "package", names: ["candle"], flagged: () => true },
  { id: "wix", stage: "package", names: ["wix"], flagged: (rest) => READ_ONLY(["build", "msi", "burn", "convert", "extension"])(rest) },
  { id: "wix", stage: "package", names: ["light"], flagged: (rest) => rest.some((arg) => /\.(?:wixobj|wxs|wixlib)$/i.test(arg) || /^-out$/i.test(arg)) },
  { id: "wrangler", stage: "upload", names: ["wrangler"], flagged: (rest) => !READ_ONLY(["--version", "-v", "whoami", "--help", "-h"])(rest) },
  { id: "rclone", stage: "upload", names: ["rclone"], flagged: (rest) => !READ_ONLY(["ls", "lsf", "lsd", "lsl", "lsjson", "size", "check", "version", "listremotes", "about", "cat", "--version", "--help"])(rest) },
  { id: "aws-s3", stage: "upload", names: ["aws"], flagged: (rest) => (rest[0] === "s3" && !["ls", "help"].includes(String(rest[1] ?? "")))
    || (rest[0] === "s3api" && /^(?:put-|create-multipart-upload|upload-part|complete-multipart-upload|copy-object|delete-)/.test(String(rest[1] ?? ""))) },
  { id: "gh-release", stage: "upload", names: ["gh"], flagged: (rest) => rest[0] === "release" && ["create", "upload", "edit", "delete", "delete-asset"].includes(String(rest[1] ?? "")) },
  { id: "cargo-publish", stage: "upload", names: ["cargo"], flagged: (rest) => rest[0] === "publish" || (rest[0] === "tauri" && ["build", "bundle", "signer"].includes(String(rest[1] ?? ""))) },
  { id: "tauri-build", stage: "build", names: ["cargo-tauri"], flagged: (rest) => ["build", "bundle", "signer"].includes(String(rest[0] ?? "")) },
  { id: "npm-publish", stage: "upload", names: ["npm"], flagged: (rest) => rest[0] === "publish" },
  { id: "pnpm-publish", stage: "upload", names: ["pnpm"], flagged: (rest) => rest[0] === "publish" },
  { id: "npm-publish", stage: "upload", names: ["yarn"], flagged: (rest) => rest[0] === "publish" || (rest[0] === "npm" && rest[1] === "publish") },
  { id: "tauri-build", stage: "build", names: ["tauri"], flagged: (rest) => ["build", "bundle", "signer"].includes(String(rest[0] ?? "")) },
  { id: "xcode-archive", stage: "package", names: ["xcodebuild"], flagged: (rest) => rest.some((arg) => /^(?:archive|-exportArchive|-archivePath|-exportOptionsPlist)$/.test(arg)) },
];

/** `rustright cargo publish` reaches the same crates.io upload through the managed client. */
const MANAGED_CLIENT_RULE = { id: "cargo-publish", stage: "upload" };

/** Wrapper words that run another command: skipped so the tool behind them is the one classified. */
const WRAPPERS = new Set(["sudo", "env", "time", "command", "exec", "call", "start", "nohup", "xcrun", "npx", "corepack", "pwsh", "powershell", "bash", "sh", "zsh", "cmd", "node", "python", "python3", "py"]);

const stripQuotes = (token) => String(token).replace(/^[\s"'`@&]+|[\s"'`,;]+$/g, "");
export const commandBaseName = (token) => stripQuotes(token).split(/[\\/]/).pop().toLowerCase().replace(/\.(?:exe|cmd|bat|ps1|sh|com|command)$/i, "");

/** Split one script line into command segments at the shell operators. */
export function splitSegments(line) {
  return String(line).split(/&&|\|\||\$\(|[;|&`()]/).map((part) => part.trim()).filter(Boolean);
}

/** Tokens of one segment after the leading VAR=value assignments and wrapper words. */
export function segmentTokens(segment) {
  let tokens = segment.match(/"[^"]*"|'[^']*'|\S+/g) ?? [];
  tokens = tokens.map((token) => token.replace(/^["']|["']$/g, ""));
  while (tokens.length && /^[A-Za-z_][A-Za-z0-9_]*=/.test(tokens[0])) tokens = tokens.slice(1);
  for (let guard = 0; guard < 8 && tokens.length; guard += 1) {
    const name = commandBaseName(tokens[0]);
    if (name === "pnpm" && (tokens[1] === "exec" || tokens[1] === "dlx")) { tokens = tokens.slice(2); continue; }
    if (name === "npm" && tokens[1] === "exec") { const dash = tokens.indexOf("--"); tokens = dash >= 0 ? tokens.slice(dash + 1) : tokens.slice(2); continue; }
    if (WRAPPERS.has(name) && tokens.length > 1) {
      // Interpreters and shells run a script, not another tool, unless they take -c/-Command text: keep scanning the rest.
      tokens = tokens.slice(1).filter((token, index) => !(index === 0 && /^-+(?:c|command|noprofile|nologo|noninteractive|executionpolicy|file|f|e|s)$/i.test(token)));
      // `bash -c "tool args"` and `powershell -Command "tool args"`: the quoted text is the command line.
      if (tokens.length && /\s/.test(tokens[0])) tokens = [...tokens[0].trim().split(/\s+/), ...tokens.slice(1)];
      continue;
    }
    break;
  }
  return tokens;
}

/** The RustRight client's names. `rightkit` is its retired name (rustright#91): never admitted (pipeline-admission.mjs refuses
 * it, scripts/preflight.mjs retired-alias fails it), but still recognised here, so a script that uses it is classified, never missed. */
export const MANAGED_CLIENT_NAMES = Object.freeze(["rustright", "rightkit"]);

/** Classify one command's tokens against the R3 inventory: `{ id, stage, tool }` for a pipeline tool run in a write form, else null. */
export function classifyTokens(tokens) {
  if (!tokens.length) return null;
  const name = commandBaseName(tokens[0]);
  const rest = tokens.slice(1);
  if (MANAGED_CLIENT_NAMES.includes(name) && rest[0] === "cargo" && rest[1] === "publish") return { ...MANAGED_CLIENT_RULE, tool: "rustright cargo publish" };
  for (const rule of RULES) {
    if (rule.names.includes(name) && rule.flagged(rest)) return { id: rule.id, stage: rule.stage, tool: [name, ...rest.slice(0, 2)].join(" ").trim() };
  }
  return null;
}

/** True when the tokens run RightKit's own pipeline CLI or managed client (a generator must never dispatch another pipeline). */
export function dispatchesRightKit(tokens) {
  if (!tokens.length) return false;
  const name = commandBaseName(tokens[0]);
  if (MANAGED_CLIENT_NAMES.includes(name) || name === "right-release") return true;
  return tokens.some((token) => /(?:^|[\\/])(?:@rightkit[\\/]release[\\/]cli[\\/]right-release|right-release)(?:\.mjs)?$/i.test(token));
}

const YAML_RUN_PREFIX = /^\s*(?:-\s+)?(?:run|script|command)\s*:\s*[|>]?[-+]?\s*/;
const COMMENT_LINE = /^\s*(?:#|\/\/|::|rem\s|@rem\s)/i;

/**
 * Pipeline tool matches in the text of a shell-like source (a script, a hook, a workflow). `kind`:
 *  - "shell": every line is read as a command line (sh, ps1, bat, cmd, command, yml run steps, a package.json script value);
 *  - "code": quoted tool names in a js/mjs/py/rs source (`spawnSync("codesign", ...)`), qualified by the next literals.
 * Returns `[{ line, id, stage, tool, text }]` (1-based lines).
 */
export function scanTextForPipelineTools(text, { kind = "shell" } = {}) {
  const found = [];
  const lines = String(text).split(/\r?\n/);
  if (kind === "code") {
    const names = new Map();
    for (const rule of RULES) for (const name of rule.names) names.set(name, rule);
    lines.forEach((line, index) => {
      if (COMMENT_LINE.test(line)) return;
      for (const literal of line.matchAll(/["'`]([^"'`\n]{1,200})["'`]/g)) {
        const tokens = literal[1].trim().split(/\s+/);
        const first = commandBaseName(tokens[0]);
        const rule = names.get(first) ?? (MANAGED_CLIENT_NAMES.includes(first) ? MANAGED_CLIENT_RULE : null);
        if (!rule) continue;
        // The arguments are the rest of this literal, then the next few literals of the same call (an argv array).
        const tail = line.slice(literal.index + literal[0].length, literal.index + literal[0].length + 240);
        const following = [...tail.matchAll(/["'`]([^"'`\n]{0,120})["'`]/g)].map((match) => match[1]);
        const rest = [...tokens.slice(1), ...following].map((arg) => arg.trim()).filter(Boolean);
        const hit = MANAGED_CLIENT_NAMES.includes(first) ? classifyTokens([first, ...rest]) : rule.flagged(rest) ? { id: rule.id, stage: rule.stage, tool: first } : null;
        if (hit) found.push({ line: index + 1, ...hit, text: line.trim().slice(0, 160) });
      }
    });
    return found;
  }
  lines.forEach((line, index) => {
    if (COMMENT_LINE.test(line)) return;
    const body = line.replace(YAML_RUN_PREFIX, "");
    for (const segment of splitSegments(body)) {
      const hit = classifyTokens(segmentTokens(segment));
      if (hit) found.push({ line: index + 1, ...hit, text: line.trim().slice(0, 160) });
    }
  });
  return found;
}

/** GitHub Actions that build, sign or upload on their own (R3: the same tools behind an action). */
export const PIPELINE_ACTIONS = Object.freeze([
  { pattern: /^tauri-apps\/tauri-action(?:@|$)/i, id: "tauri-build", stage: "build" },
  { pattern: /^apple-actions\//i, id: "codesign", stage: "sign" },
  { pattern: /^azure\/(?:trusted-signing-action|artifact-signing-action)(?:@|$)/i, id: "azuresigntool", stage: "sign" },
  { pattern: /^cloudflare\/wrangler-action(?:@|$)/i, id: "wrangler", stage: "upload" },
  { pattern: /^softprops\/action-gh-release(?:@|$)/i, id: "gh-release", stage: "upload" },
  { pattern: /^ncipollo\/release-action(?:@|$)/i, id: "gh-release", stage: "upload" },
  { pattern: /^svenstaro\/upload-release-action(?:@|$)/i, id: "gh-release", stage: "upload" },
  { pattern: /^actions\/upload-release-asset(?:@|$)/i, id: "gh-release", stage: "upload" },
  { pattern: /^shogo82148\/actions-upload-release-asset(?:@|$)/i, id: "gh-release", stage: "upload" },
  { pattern: /^jakejarvis\/s3-sync-action(?:@|$)/i, id: "aws-s3", stage: "upload" },
]);

export function classifyAction(uses) {
  const value = String(uses).trim().replace(/^["']|["']$/g, "");
  for (const entry of PIPELINE_ACTIONS) if (entry.pattern.test(value)) return { id: entry.id, stage: entry.stage, tool: value };
  return null;
}
