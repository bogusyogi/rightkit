import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, symlinkSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
import {
  buildClaudeMarketplace,
  buildCodexMarketplace,
  buildMarketplace,
  parseSourceList,
  renderMarketplaceFiles,
  validateMarketplace,
  workspaceIsMarketplaceRoot,
} from "./plugin/marketplace.mjs";

const CLI = path.join(path.dirname(fileURLToPath(import.meta.url)), "cli", "right-ax.mjs");
const PULSE_DESCRIPTION = "Message AI chats on paired machines through Pulse chat; use when the user mentions another machine, the Dell/Mac, a chat elsewhere, or asks to tell/ask a session on another computer.";
const SCRAPE_DESCRIPTION = "Scrape web pages into clean markdown with the scraperight CLI; use when the user asks to scrape a URL, crawl a site, or extract page content.";

function tmp(label = "right-ax-market-") {
  return mkdtempSync(path.join(os.tmpdir(), label));
}

function sources(overrides = {}) {
  return {
    schemaVersion: 1,
    name: "right-suite",
    owner: { name: "Orthic Labs" },
    plugins: [
      {
        name: "pulse",
        description: "Message AI chats on paired machines through Pulse chat.",
        category: "productivity",
        version: "0.1.0",
        url: "https://github.com/Orthic-Labs/pulse.git",
        ref: "main",
        hosts: { claude: { path: "plugins/claude/pulse" }, codex: { path: "plugins/codex/pulse" } },
      },
      {
        name: "legion",
        description: "Legion orchestrates capability selection and authority attachment for AI-assisted engineering.",
        version: "0.3.23",
        url: "https://github.com/Orthic-Labs/legion.git",
        ref: "main",
        hosts: { claude: { path: "." } },
      },
    ],
    ...overrides,
  };
}

function writeFile(root, rel, text) {
  const target = path.join(root, ...rel.split("/"));
  mkdirSync(path.dirname(target), { recursive: true });
  writeFileSync(target, text);
}

// A checkout of the "pulse" repository holding a Claude-native plugin.
function pulseCheckout(workspace, { description = PULSE_DESCRIPTION, name = "pulse", version = "0.1.0" } = {}) {
  const root = path.join(workspace, "pulse", "plugins", "claude", "pulse");
  writeFile(root, ".claude-plugin/plugin.json", JSON.stringify({ name, version, description: "Pulse Chat for Claude Code.", author: { name: "Orthic Labs" } }));
  writeFile(root, "skills/pulse-bridge/SKILL.md", `---\nname: pulse-bridge\ndescription: ${description}\n---\n\n# Pulse\n`);
  writeFile(root, "hooks/hooks.json", JSON.stringify({ hooks: { Stop: [{ hooks: [{ type: "command", command: 'sh "${CLAUDE_PLUGIN_ROOT}/hooks/run-pulse.sh" claude-hook', timeout: 10 }] }] } }));
  writeFile(root, "hooks/run-pulse.sh", "#!/bin/sh\ncommand -v pulse >/dev/null 2>&1 || exit 0\nexec pulse chat claude-hook\n");
  return root;
}

function pulseOnly() {
  const list = sources();
  list.plugins = [list.plugins[0]];
  return list;
}

function findingRules(findings) {
  return findings.filter((item) => item.severity === "error").map((item) => item.ruleId);
}

test("a well-formed source list parses with no findings", () => {
  const { list, findings } = parseSourceList(sources());
  assert.ok(list);
  assert.deepEqual(findings, []);
});

test("the source list rejects bad names, duplicates, urls, refs, shas, versions and paths", () => {
  const bad = (mutate) => {
    const input = sources();
    mutate(input.plugins[0], input);
    const { list, findings } = parseSourceList(input);
    assert.equal(list, null);
    assert.ok(findings.some((item) => item.severity === "error"), JSON.stringify(findings));
    return findings;
  };
  bad((entry) => { entry.name = "Pulse Chat"; });
  bad((entry, input) => { input.plugins[1].name = entry.name; });
  bad((entry) => { entry.url = "http://github.com/Orthic-Labs/pulse.git"; });
  bad((entry) => { entry.url = "git@github.com:Orthic-Labs/pulse.git"; });
  bad((entry) => { entry.url = "file:///tmp/pulse"; });
  bad((entry) => { entry.url = "https://user:secret@github.com/Orthic-Labs/pulse.git"; });
  bad((entry) => { delete entry.ref; });
  bad((entry) => { entry.ref = "has space"; });
  bad((entry) => { entry.sha = "abc123"; });
  bad((entry) => { entry.version = "one"; });
  bad((entry) => { entry.description = "short"; });
  bad((entry) => { entry.hosts = {}; });
  bad((entry) => { entry.hosts = { vim: { path: "." } }; });
  bad((entry) => { entry.hosts.claude.path = "../escape"; });
  bad((entry) => { entry.hosts.claude.path = "/abs/path"; });
  bad((entry) => { entry.hosts.claude.path = "a\\b"; });
  bad((entry) => { entry.hosts.codex.local = "plugins/pulse"; });
  bad((entry) => { entry.hosts.claude.local = "/abs/plugin"; });
  bad((entry) => { entry.hosts.claude.local = "./../escape"; });
  bad((entry) => { entry.hosts.claude.local = "./a/../b"; });
  bad((entry) => { entry.hosts.claude.local = "plugins/pulse"; });
  bad((entry) => { entry.hosts.claude.local = ".\\plugins"; });
  bad((entry) => { entry.hosts.claude.local = "./"; });
  bad((entry) => { entry.hosts.claude.local = "C:/plugins/pulse"; });
  bad((entry) => { entry.hosts.claude.local = 5; });
  bad((entry) => { entry.hosts.claude = { local: "./plugins/pulse" }; delete entry.url; });
  bad((entry, input) => { input.schemaVersion = 2; });
  bad((entry, input) => { input.name = "Right Suite"; });
  bad((entry, input) => { delete input.owner; });
  assert.equal(parseSourceList({ ...sources(), plugins: "nope" }).list, null);
  assert.equal(parseSourceList(null).list, null);
});

test("file URLs are accepted only when explicitly allowed", () => {
  const input = pulseOnly();
  input.plugins[0].url = "file:///tmp/pulse";
  assert.equal(parseSourceList(input).list, null);
  assert.ok(parseSourceList(input, { allowFileUrls: true }).list);
});

test("the Claude marketplace uses a url source for a repo-root plugin and git-subdir for a subdirectory", () => {
  const manifest = buildClaudeMarketplace(parseSourceList(sources()).list);
  assert.equal(manifest.$schema, "https://json.schemastore.org/claude-code-plugin-marketplace.json");
  assert.equal(manifest.name, "right-suite");
  assert.deepEqual(manifest.owner, { name: "Orthic Labs" });
  assert.deepEqual(manifest.plugins[0], {
    name: "pulse",
    source: { source: "git-subdir", url: "https://github.com/Orthic-Labs/pulse.git", path: "plugins/claude/pulse", ref: "main" },
    description: "Message AI chats on paired machines through Pulse chat.",
    version: "0.1.0",
    category: "productivity",
  });
  assert.deepEqual(manifest.plugins[1].source, { source: "url", url: "https://github.com/Orthic-Labs/legion.git", ref: "main" });
  assert.equal("category" in manifest.plugins[1], false);
});

test("a pinned sha is carried into the source object", () => {
  const input = pulseOnly();
  input.plugins[0].sha = "a".repeat(40);
  const manifest = buildClaudeMarketplace(parseSourceList(input).list);
  assert.equal(manifest.plugins[0].source.sha, "a".repeat(40));
});

test("a plugin without a claude host is absent from the Claude marketplace", () => {
  const input = pulseOnly();
  delete input.plugins[0].hosts.claude;
  const { list, findings } = parseSourceList(input);
  assert.ok(findings.some((item) => item.severity === "warning"));
  assert.deepEqual(buildClaudeMarketplace(list).plugins, []);
});

test("the Codex marketplace lists only plugins vendored locally, in the evidenced local shape", () => {
  const input = pulseOnly();
  const remote = buildCodexMarketplace(parseSourceList(input).list);
  assert.deepEqual(remote.manifest.plugins, []);
  assert.deepEqual(remote.skipped, ["pulse"]);

  input.plugins[0].hosts.codex.local = "./plugins/pulse";
  const local = buildCodexMarketplace(parseSourceList(input).list);
  assert.deepEqual(local.manifest, {
    name: "right-suite",
    plugins: [{
      name: "pulse",
      source: { source: "local", path: "./plugins/pulse" },
      policy: { installation: "AVAILABLE", authentication: "ON_INSTALL" },
      category: "productivity",
    }],
  });
});

test("render produces the Claude file always and the Codex file only when it has entries", () => {
  const list = parseSourceList(sources()).list;
  const { files, codexSkipped } = renderMarketplaceFiles(list);
  assert.deepEqual(files.map((file) => file.path), [".claude-plugin/marketplace.json"]);
  assert.deepEqual(codexSkipped, ["pulse"]);
  list.plugins[0].hosts.codex.local = "./plugins/pulse";
  assert.deepEqual(renderMarketplaceFiles(list).files.map((file) => file.path), [".claude-plugin/marketplace.json", ".agents/plugins/marketplace.json"]);
});

test("build writes the manifest; build --check passes, then reports drift after an edit", () => {
  const work = tmp();
  const sourcesPath = path.join(work, "plugins.json");
  writeFileSync(sourcesPath, JSON.stringify(sources()));
  const out = path.join(work, "out");
  const built = buildMarketplace({ sourcesPath, outDir: out });
  assert.equal(built.ok, true, JSON.stringify(built.findings));
  const target = path.join(out, ".claude-plugin", "marketplace.json");
  assert.ok(existsSync(target));
  assert.equal(JSON.parse(readFileSync(target, "utf8")).plugins.length, 2);

  assert.equal(buildMarketplace({ sourcesPath, outDir: out, check: true }).ok, true);

  writeFileSync(target, "{}\n");
  const drift = buildMarketplace({ sourcesPath, outDir: out, check: true });
  assert.equal(drift.ok, false);
  assert.deepEqual(drift.drift, [".claude-plugin/marketplace.json"]);

  const missing = buildMarketplace({ sourcesPath, outDir: path.join(work, "nowhere"), check: true });
  assert.equal(missing.ok, false);
});

test("build refuses an invalid source list and writes nothing", () => {
  const work = tmp();
  const sourcesPath = path.join(work, "plugins.json");
  const input = sources();
  input.plugins[0].ref = "";
  writeFileSync(sourcesPath, JSON.stringify(input));
  const built = buildMarketplace({ sourcesPath, outDir: work });
  assert.equal(built.ok, false);
  assert.equal(existsSync(path.join(work, ".claude-plugin")), false);
  assert.equal(buildMarketplace({ sourcesPath: path.join(work, "missing.json"), outDir: work }).ok, false);
});

test("validate resolves every listed plugin from a workspace and runs plugin validate on it", () => {
  const work = tmp();
  const workspace = path.join(work, "ws");
  pulseCheckout(workspace);
  const sourcesPath = path.join(work, "plugins.json");
  writeFileSync(sourcesPath, JSON.stringify(pulseOnly()));
  const claudeOnly = JSON.parse(readFileSync(sourcesPath, "utf8"));
  delete claudeOnly.plugins[0].hosts.codex;
  writeFileSync(sourcesPath, JSON.stringify(claudeOnly));
  const result = validateMarketplace({ sourcesPath, workspace, skipDrift: true });
  assert.equal(result.valid, true, JSON.stringify(result, null, 2));
  assert.equal(result.plugins.length, 1);
  assert.equal(result.plugins[0].host, "claude");
  assert.match(result.plugins[0].source, /workspace checkout/);
});

test("validate fails when a listed plugin's skill description is vague", () => {
  const work = tmp();
  const workspace = path.join(work, "ws");
  pulseCheckout(workspace, { description: "A chat tool." });
  const sourcesPath = path.join(work, "plugins.json");
  const input = pulseOnly();
  delete input.plugins[0].hosts.codex;
  writeFileSync(sourcesPath, JSON.stringify(input));
  const result = validateMarketplace({ sourcesPath, workspace, skipDrift: true });
  assert.equal(result.valid, false);
  assert.ok(result.plugins[0].findings.some((item) => item.ruleId === "skill-description-length"));
});

test("validate fails on a name or version that differs from the marketplace entry", () => {
  const work = tmp();
  const input = pulseOnly();
  delete input.plugins[0].hosts.codex;
  const sourcesPath = path.join(work, "plugins.json");
  writeFileSync(sourcesPath, JSON.stringify(input));

  const renamed = path.join(work, "renamed");
  pulseCheckout(renamed, { name: "pulse-bridge" });
  const nameResult = validateMarketplace({ sourcesPath, workspace: renamed, skipDrift: true });
  assert.equal(nameResult.valid, false);
  assert.ok(nameResult.plugins[0].findings.some((item) => item.ruleId === "marketplace-plugin-name"));

  const bumped = path.join(work, "bumped");
  pulseCheckout(bumped, { version: "0.2.0" });
  const versionResult = validateMarketplace({ sourcesPath, workspace: bumped, skipDrift: true });
  assert.equal(versionResult.valid, false);
  assert.ok(versionResult.plugins[0].findings.some((item) => item.ruleId === "marketplace-plugin-version"));
});

test("validate fails when a plugin cannot be resolved", () => {
  const work = tmp();
  const input = pulseOnly();
  delete input.plugins[0].hosts.codex;
  const sourcesPath = path.join(work, "plugins.json");
  writeFileSync(sourcesPath, JSON.stringify(input));

  const noCheckout = validateMarketplace({ sourcesPath, workspace: path.join(work, "empty"), skipDrift: true });
  assert.equal(noCheckout.valid, false);
  assert.ok(noCheckout.plugins[0].findings.some((item) => item.ruleId === "marketplace-plugin-unresolved"));

  const noSource = validateMarketplace({ sourcesPath, skipDrift: true });
  assert.equal(noSource.valid, false);
  assert.match(noSource.plugins[0].findings[0].evidence, /--workspace|--fetch/);

  const wrongPath = tmp();
  mkdirSync(path.join(wrongPath, "pulse"), { recursive: true });
  const missingPath = validateMarketplace({ sourcesPath, workspace: wrongPath, skipDrift: true });
  assert.equal(missingPath.valid, false);
  assert.match(missingPath.plugins[0].findings[0].evidence, /does not exist/);
});

test("validate fails when the committed marketplace manifest is stale", () => {
  const work = tmp();
  const workspace = path.join(work, "ws");
  pulseCheckout(workspace);
  const input = pulseOnly();
  delete input.plugins[0].hosts.codex;
  const sourcesPath = path.join(work, "plugins.json");
  writeFileSync(sourcesPath, JSON.stringify(input));
  const out = path.join(work, "out");

  const before = validateMarketplace({ sourcesPath, outDir: out, workspace });
  assert.equal(before.valid, false);
  assert.ok(findingRules(before.findings).includes("marketplace-drift"));

  buildMarketplace({ sourcesPath, outDir: out });
  const after = validateMarketplace({ sourcesPath, outDir: out, workspace });
  assert.equal(after.valid, true, JSON.stringify(after, null, 2));
});

const GIT = spawnSync("git", ["--version"], { encoding: "utf8" });

test("validate --fetch clones the listed ref and validates the plugin at the listed path", { skip: GIT.status !== 0 }, () => {
  const work = tmp();
  const repoDir = path.join(work, "pulse");
  mkdirSync(repoDir);
  pulseCheckout(work);
  const git = (...args) => {
    const result = spawnSync("git", ["-c", "user.name=AX test", "-c", "user.email=test@example.invalid", "-c", "commit.gpgsign=false", ...args], { cwd: repoDir, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
    return result.stdout.trim();
  };
  git("init", "-q", "-b", "main");
  git("add", ".");
  git("commit", "-q", "-m", "plugin");

  const input = pulseOnly();
  delete input.plugins[0].hosts.codex;
  input.plugins[0].url = pathToFileURL(repoDir).href;
  const sourcesPath = path.join(work, "plugins.json");
  writeFileSync(sourcesPath, JSON.stringify(input));

  const result = validateMarketplace({ sourcesPath, fetch: true, allowFileUrls: true, skipDrift: true });
  assert.equal(result.valid, true, JSON.stringify(result, null, 2));
  assert.match(result.plugins[0].source, /cloned/);

  const sha = git("rev-parse", "HEAD");
  input.plugins[0].sha = sha;
  writeFileSync(sourcesPath, JSON.stringify(input));
  assert.equal(validateMarketplace({ sourcesPath, fetch: true, allowFileUrls: true, skipDrift: true }).valid, true);

  input.plugins[0].ref = "no-such-branch";
  delete input.plugins[0].sha;
  writeFileSync(sourcesPath, JSON.stringify(input));
  const missingRef = validateMarketplace({ sourcesPath, fetch: true, allowFileUrls: true, skipDrift: true });
  assert.equal(missingRef.valid, false);
  assert.ok(missingRef.plugins[0].findings.some((item) => item.ruleId === "marketplace-plugin-unresolved"));
});

test("right-ax marketplace build / validate end to end", () => {
  const work = tmp();
  const workspace = path.join(work, "ws");
  pulseCheckout(workspace);
  const input = pulseOnly();
  delete input.plugins[0].hosts.codex;
  mkdirSync(path.join(work, "marketplace"));
  const sourcesPath = path.join(work, "marketplace", "plugins.json");
  writeFileSync(sourcesPath, JSON.stringify(input));

  const run = (args) => spawnSync(process.execPath, [CLI, ...args], { encoding: "utf8", cwd: work });
  const stale = run(["marketplace", "build", "--sources", sourcesPath, "--out", work, "--check"]);
  assert.equal(stale.status, 1);
  const built = run(["marketplace", "build", "--sources", sourcesPath, "--out", work]);
  assert.equal(built.status, 0, built.stderr);
  assert.ok(existsSync(path.join(work, ".claude-plugin", "marketplace.json")));
  assert.equal(run(["marketplace", "build", "--sources", sourcesPath, "--out", work, "--check"]).status, 0);
  const validated = run(["marketplace", "validate", "--sources", sourcesPath, "--out", work, "--workspace", workspace]);
  assert.equal(validated.status, 0, validated.stdout + validated.stderr);
  assert.equal(JSON.parse(validated.stdout).valid, true);
  const unresolved = run(["marketplace", "validate", "--sources", sourcesPath, "--out", work]);
  assert.equal(unresolved.status, 1);
  assert.equal(run(["marketplace", "bogus"]).status, 2);
});

test("a portable-layout plugin at the repository root validates through its listing", () => {
  const work = tmp();
  const workspace = path.join(work, "ws");
  const root = path.join(workspace, "scraperight");
  writeFile(root, "plugin.json", JSON.stringify({ $schema: "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json", name: "scraperight", version: "0.1.0" }));
  writeFile(root, "skills/scraperight/SKILL.md", `---\nname: scraperight\ndescription: ${SCRAPE_DESCRIPTION}\n---\n`);
  const input = sources();
  input.plugins = [{
    name: "scraperight",
    description: "Scrape web pages into markdown from the command line.",
    version: "0.1.0",
    url: "https://github.com/Orthic-Labs/scraperight.git",
    ref: "v0.1.0",
    hosts: { claude: { path: "." } },
  }];
  const sourcesPath = path.join(work, "plugins.json");
  writeFileSync(sourcesPath, JSON.stringify(input));
  const result = validateMarketplace({ sourcesPath, workspace, skipDrift: true });
  assert.equal(result.valid, true, JSON.stringify(result, null, 2));
});

// ---- vendored plugins: hosts.<host>.local ------------------------------------------------------

function vendoredList(extraPlugins = []) {
  return {
    schemaVersion: 1,
    name: "right-suite",
    owner: { name: "Orthic Labs" },
    plugins: [
      {
        name: "pulse",
        description: "Message AI chats on paired machines through Pulse chat.",
        category: "productivity",
        version: "0.1.0",
        hosts: { claude: { local: "./plugins/claude/pulse" }, codex: { local: "./plugins/codex/pulse" } },
      },
      ...extraPlugins,
    ],
  };
}

const LEGION_ENTRY = {
  name: "legion",
  description: "Legion orchestrates capability selection and authority attachment for AI-assisted engineering.",
  version: "0.3.23",
  category: "development",
  url: "https://github.com/Orthic-Labs/legion.git",
  ref: "main",
  hosts: { claude: { path: "." } },
};

// A marketplace repository that vendors its plugins: <root>/marketplace/plugins.json plus plugins/ beside it.
function vendoredRepo({ description = PULSE_DESCRIPTION, extraPlugins = [] } = {}) {
  const parent = tmp();
  const root = path.join(parent, "right-plugins");
  const claude = path.join(root, "plugins", "claude", "pulse");
  writeFile(claude, ".claude-plugin/plugin.json", JSON.stringify({ name: "pulse", version: "0.1.0", description: "Pulse Chat for Claude Code.", author: { name: "Orthic Labs" } }));
  writeFile(claude, "skills/pulse-bridge/SKILL.md", `---\nname: pulse-bridge\ndescription: ${description}\n---\n\n# Pulse\n`);
  const codex = path.join(root, "plugins", "codex", "pulse");
  writeFile(codex, ".codex-plugin/plugin.json", JSON.stringify({ name: "pulse", version: "0.1.0", description: "Pulse Chat for Codex.", skills: "./skills/" }));
  writeFile(codex, "skills/pulse-bridge/SKILL.md", `---\nname: pulse-bridge\ndescription: ${description}\n---\n\n# Pulse\n`);
  const sourcesPath = path.join(root, "marketplace", "plugins.json");
  writeFile(root, "marketplace/plugins.json", JSON.stringify(vendoredList(extraPlugins)));
  return { parent, root, sourcesPath };
}

test("a local host needs no url or ref, and emits the relative-path source each host's manifest uses", () => {
  const parsed = parseSourceList(vendoredList());
  assert.deepEqual(parsed.findings, []);
  assert.deepEqual(buildClaudeMarketplace(parsed.list).plugins, [{ name: "pulse", source: "./plugins/claude/pulse", description: "Message AI chats on paired machines through Pulse chat.", version: "0.1.0", category: "productivity" }]);
  assert.deepEqual(buildCodexMarketplace(parsed.list).manifest.plugins, [{ name: "pulse", source: { source: "local", path: "./plugins/codex/pulse" }, policy: { installation: "AVAILABLE", authentication: "ON_INSTALL" }, category: "productivity" }]);
  // a sha never leaks into a local source; a local entry beside a remote one keeps each shape
  const mixed = parseSourceList(vendoredList([{ ...LEGION_ENTRY, sha: "a".repeat(40) }])).list;
  const plugins = buildClaudeMarketplace(mixed).plugins;
  assert.equal(plugins[0].source, "./plugins/claude/pulse");
  assert.deepEqual(plugins[1].source, { source: "url", url: "https://github.com/Orthic-Labs/legion.git", ref: "main", sha: "a".repeat(40) });
  // a host that is NOT local still needs its repository
  const needsUrl = vendoredList();
  needsUrl.plugins[0].hosts.codex = { path: "plugins/codex/pulse" };
  assert.ok(parseSourceList(needsUrl).findings.some((item) => /\.url must be a public https git URL/.test(item.claim)));
  // local and path together: local wins for the source, path stays a valid upstream path
  const both = vendoredList();
  both.plugins[0].hosts.claude.path = "plugins/claude/pulse";
  assert.equal(buildClaudeMarketplace(parseSourceList(both).list).plugins[0].source, "./plugins/claude/pulse");
});

test("local paths refuse absolute, parent-traversal and malformed values with the host named", () => {
  for (const value of ["/abs/plugin", "./../escape", "./a/../b", "plugins/pulse", ".\\plugins", "./", "C:/plugins/pulse", "//server/share", "./a//b"]) {
    for (const host of ["claude", "codex"]) {
      const input = vendoredList();
      input.plugins[0].hosts[host].local = value;
      const { list, findings } = parseSourceList(input);
      assert.equal(list, null, `${host} ${value}`);
      assert.ok(findings.some((item) => item.claim.includes(`hosts.${host}.local must be a ./relative path inside the marketplace repository`)), `${host} ${value}: ${JSON.stringify(findings)}`);
    }
  }
});

test("build then validate on a vendoring repository: drift checking passes against the generated files", () => {
  const { root, sourcesPath } = vendoredRepo({ extraPlugins: [LEGION_ENTRY] });
  const built = buildMarketplace({ sourcesPath, outDir: root });
  assert.equal(built.ok, true, JSON.stringify(built.findings));
  const claude = JSON.parse(readFileSync(path.join(root, ".claude-plugin", "marketplace.json"), "utf8"));
  assert.equal(claude.plugins[0].source, "./plugins/claude/pulse");
  assert.equal(claude.plugins[1].source.source, "url");
  const codex = JSON.parse(readFileSync(path.join(root, ".agents", "plugins", "marketplace.json"), "utf8"));
  assert.deepEqual(codex.plugins.map((plugin) => plugin.source.path), ["./plugins/codex/pulse"]);
  assert.equal(buildMarketplace({ sourcesPath, outDir: root, check: true }).ok, true);
  // legion is a sibling checkout of the marketplace repository (beside it, as in a workspace of checkouts)
  const legion = path.join(path.dirname(root), "legion");
  writeFile(legion, "plugin.json", JSON.stringify({ $schema: "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json", name: "legion", version: "0.3.23" }));
  writeFile(legion, "skills/legion/SKILL.md", `---\nname: legion\ndescription: ${SCRAPE_DESCRIPTION}\n---\n`);
  const result = validateMarketplace({ sourcesPath, outDir: root, workspace: root });
  assert.equal(result.valid, true, JSON.stringify(result, null, 2));
  assert.deepEqual(result.plugins.map((record) => [record.plugin, record.host]), [["pulse", "claude"], ["pulse", "codex"], ["legion", "claude"]]);
  assert.match(result.plugins[0].source, /^local \.\/plugins\/claude\/pulse in marketplace repository /);
  assert.match(result.plugins[2].source, /workspace checkout .*legion/);
  // a stale generated file is drift, even though every plugin validates
  writeFileSync(path.join(root, ".claude-plugin", "marketplace.json"), "{}\n");
  const stale = validateMarketplace({ sourcesPath, outDir: root, workspace: root });
  assert.equal(stale.valid, false);
  assert.ok(stale.findings.some((item) => item.ruleId === "marketplace-drift"));
});

test("--workspace may be the marketplace repository root; a directory of checkouts still works for remote entries", () => {
  const { parent, root, sourcesPath } = vendoredRepo();
  assert.equal(workspaceIsMarketplaceRoot(root, sourcesPath), true);
  assert.equal(workspaceIsMarketplaceRoot(parent, sourcesPath), true, "any directory the source list lives under");
  assert.equal(workspaceIsMarketplaceRoot(path.join(parent, "elsewhere"), sourcesPath), false);
  assert.equal(workspaceIsMarketplaceRoot(undefined, sourcesPath), false);
  // local entries resolve against --out when no workspace is given, and against the workspace when it is the repository
  assert.equal(validateMarketplace({ sourcesPath, outDir: root, skipDrift: true }).valid, true);
  assert.equal(validateMarketplace({ sourcesPath, workspace: root, skipDrift: true }).valid, true);
  const noRoot = validateMarketplace({ sourcesPath, skipDrift: true });
  assert.equal(noRoot.valid, false);
  assert.ok(noRoot.plugins[0].findings.some((item) => item.ruleId === "marketplace-plugin-unresolved" && /pass --out <marketplace repository root> or --workspace/.test(item.evidence)));
  // a repository entry whose own name matches the workspace directory resolves to the workspace itself
  const self = vendoredList();
  self.plugins[0] = { ...self.plugins[0], url: "https://github.com/Orthic-Labs/right-plugins.git", ref: "main", hosts: { claude: { path: "plugins/claude/pulse" } } };
  writeFileSync(sourcesPath, JSON.stringify(self));
  const viaSelf = validateMarketplace({ sourcesPath, workspace: root, skipDrift: true });
  assert.equal(viaSelf.valid, true, JSON.stringify(viaSelf, null, 2));
  assert.match(viaSelf.plugins[0].source, /workspace checkout .*right-plugins/);
});

test("validate fails a local plugin that is missing, escapes the repository, or fails plugin validate", () => {
  const missing = vendoredRepo();
  const list = vendoredList();
  list.plugins[0].hosts.claude.local = "./plugins/nowhere";
  writeFileSync(missing.sourcesPath, JSON.stringify(list));
  const gone = validateMarketplace({ sourcesPath: missing.sourcesPath, outDir: missing.root, skipDrift: true });
  assert.equal(gone.valid, false);
  assert.ok(gone.plugins[0].findings.some((item) => item.ruleId === "marketplace-plugin-unresolved" && /local path \.\/plugins\/nowhere does not exist in the marketplace repository/.test(item.evidence)));

  const vague = vendoredRepo({ description: "A chat tool." });
  const vagueResult = validateMarketplace({ sourcesPath: vague.sourcesPath, outDir: vague.root, skipDrift: true });
  assert.equal(vagueResult.valid, false);
  assert.ok(vagueResult.plugins[0].findings.some((item) => item.ruleId === "skill-description-length"));

  const linked = vendoredRepo();
  const outside = path.join(linked.parent, "outside-plugin");
  writeFile(outside, ".claude-plugin/plugin.json", JSON.stringify({ name: "pulse", version: "0.1.0", description: "Outside the repository." }));
  symlinkSync(outside, path.join(linked.root, "plugins", "linked"));
  const escaping = vendoredList();
  escaping.plugins[0].hosts = { claude: { local: "./plugins/linked" } };
  writeFileSync(linked.sourcesPath, JSON.stringify(escaping));
  const escaped = validateMarketplace({ sourcesPath: linked.sourcesPath, outDir: linked.root, skipDrift: true });
  assert.equal(escaped.valid, false);
  assert.ok(escaped.plugins[0].findings.some((item) => /escapes the marketplace repository/.test(item.evidence)), JSON.stringify(escaped.plugins));

  // identity checks still apply to a vendored plugin
  const renamed = vendoredRepo();
  const wrong = vendoredList();
  wrong.plugins[0].version = "9.9.9";
  writeFileSync(renamed.sourcesPath, JSON.stringify(wrong));
  const versionResult = validateMarketplace({ sourcesPath: renamed.sourcesPath, outDir: renamed.root, skipDrift: true });
  assert.ok(versionResult.plugins.some((record) => record.findings.some((item) => item.ruleId === "marketplace-plugin-version")));
});

test("the CLI validates a vendoring repository with --workspace <its root> and drift checking on", () => {
  const { root, sourcesPath } = vendoredRepo();
  const run = (args) => spawnSync(process.execPath, [CLI, ...args], { encoding: "utf8", cwd: root });
  assert.equal(run(["marketplace", "build"]).status, 0);
  assert.equal(run(["marketplace", "build", "--check"]).status, 0);
  const validated = run(["marketplace", "validate", "--workspace", "."]);
  assert.equal(validated.status, 0, validated.stdout + validated.stderr);
  assert.equal(JSON.parse(validated.stdout).valid, true);
  assert.ok(existsSync(sourcesPath));
  writeFileSync(path.join(root, ".agents", "plugins", "marketplace.json"), "{}\n");
  assert.equal(run(["marketplace", "validate", "--workspace", "."]).status, 1);
});
