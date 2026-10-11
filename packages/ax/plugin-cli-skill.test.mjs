import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { buildCliSkillMarkdown, scaffoldPlugin } from "./plugin/scaffold.mjs";
import { detectLayout, validateRepoLayout } from "./plugin/validate.mjs";

const CLI = path.join(path.dirname(fileURLToPath(import.meta.url)), "cli", "right-ax.mjs");
const DESCRIPTION = "Scrape web pages into clean markdown with the scraperight CLI; use when the user asks to scrape a URL, crawl a site, or extract page content.";
const PULSE = "Message AI chats on paired machines through Pulse chat; use when the user mentions another machine, the Dell/Mac, a chat elsewhere, or asks to tell/ask a session on another computer.";

function repo() {
  return mkdtempSync(path.join(os.tmpdir(), "right-ax-cliskill-"));
}

function run(args) {
  return spawnSync(process.execPath, [CLI, ...args], { encoding: "utf8" });
}

function readJson(...parts) {
  return JSON.parse(readFileSync(path.join(...parts), "utf8"));
}

test("CLI-skill scaffold writes one skill, three manifests and NO mcp.json, and validates clean", () => {
  const root = repo();
  const result = scaffoldPlugin(root, { cli: "scraperight", skillDescription: DESCRIPTION, commands: ["scrape", "crawl"], version: "0.1.0" });
  assert.equal(result.mcpManifest, null);
  assert.equal(result.hooksManifest, null);
  assert.equal(result.written.length, 4);
  assert.equal(existsSync(path.join(root, "mcp.json")), false);
  assert.equal(existsSync(path.join(root, "hooks")), false);
  const { valid, findings, layout } = validateRepoLayout(root);
  assert.equal(valid, true, JSON.stringify(findings));
  assert.equal(layout, "portable");
  assert.deepEqual(findings, []);
});

test("the scaffolded manifests follow the in-use host formats", () => {
  const root = repo();
  scaffoldPlugin(root, { cli: "scraperight", skillDescription: DESCRIPTION, version: "0.1.0", homepage: "https://example.test/scraperight" });
  const portable = readJson(root, "plugin.json");
  assert.equal(portable.$schema, "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json");
  assert.equal(portable.name, "scraperight");
  assert.equal(portable.description, DESCRIPTION);
  const claude = readJson(root, ".claude-plugin", "plugin.json");
  assert.deepEqual(Object.keys(claude), ["name", "version", "description", "homepage"]);
  assert.equal("$schema" in claude, false);
  const codex = readJson(root, ".codex-plugin", "plugin.json");
  assert.equal(codex.name, "scraperight");
  assert.equal(codex.skills, "./skills/");
});

test("the CLI skill teaches the command line and invents no flags", () => {
  const text = buildCliSkillMarkdown("scraperight", "scraperight", DESCRIPTION, ["scrape", "crawl"]);
  assert.match(text, /^---\nname: scraperight\ndescription: Scrape web pages/);
  assert.match(text, /`scraperight --help`/);
  assert.match(text, /`scraperight scrape`: run `scraperight scrape --help`/);
  assert.match(text, /`scraperight crawl`/);
  assert.match(text, /command -v scraperight/);
  assert.doesNotMatch(text, /TODO|TBD|FIXME/);
});

test("--mcp-command is still optional on top of the CLI skill", () => {
  const root = repo();
  const result = scaffoldPlugin(root, { cli: "scraperight", skillDescription: DESCRIPTION, mcpCommand: "scraperight serve --stdio" });
  assert.ok(result.mcpManifest);
  assert.ok(existsSync(path.join(root, "mcp.json")));
  const { valid, findings } = validateRepoLayout(root);
  assert.equal(valid, true, JSON.stringify(findings));
});

test("hook scaffolding adds hooks/hooks.json and a shim, and validates clean", () => {
  const root = repo();
  const result = scaffoldPlugin(root, { cli: "pulse", skillDescription: PULSE, hookArgs: "chat claude-hook" });
  assert.ok(result.hooksManifest);
  assert.ok(existsSync(path.join(root, "hooks", "hooks.json")));
  assert.ok(existsSync(path.join(root, "hooks", "run-pulse.sh")));
  assert.equal(result.written.length, 6);
  const { valid, findings } = validateRepoLayout(root);
  assert.equal(valid, true, JSON.stringify(findings));
  assert.deepEqual(findings, []);
});

test("custom hook events are honoured and unknown ones refused", () => {
  const root = repo();
  scaffoldPlugin(root, { cli: "pulse", skillDescription: PULSE, hookArgs: "chat claude-hook", hookEvents: ["Stop"] });
  assert.deepEqual(Object.keys(readJson(root, "hooks", "hooks.json").hooks), ["Stop"]);
  assert.throws(() => scaffoldPlugin(repo(), { cli: "pulse", skillDescription: PULSE, hookArgs: "x", hookEvents: ["Nope"] }), /unknown hook event/);
});

test("scaffolding refuses a description the gate would reject, and writes nothing", () => {
  const root = repo();
  assert.throws(() => scaffoldPlugin(root, { cli: "scraperight", skillDescription: "A widget tool." }), /description gate/);
  assert.equal(existsSync(path.join(root, "plugin.json")), false);
  assert.equal(existsSync(path.join(root, "skills")), false);
});

test("scaffolding needs a skill description, a bare program name and a safe skill name", () => {
  assert.throws(() => scaffoldPlugin(repo(), { cli: "scraperight" }), /skill description/);
  assert.throws(() => scaffoldPlugin(repo(), { cli: "../scraperight", skillDescription: DESCRIPTION }), /bare program name/);
  assert.throws(() => scaffoldPlugin(repo(), { cli: "scraperight", skillDescription: DESCRIPTION, skillName: "../x" }), /kebab-case/);
});

test("hooks without --cli are refused", () => {
  assert.throws(() => scaffoldPlugin(repo(), { hookArgs: "chat claude-hook" }), /--cli/);
});

test("validate understands host-native layouts as shipped in pulse", () => {
  const claude = repo();
  mkdirSync(path.join(claude, ".claude-plugin"));
  mkdirSync(path.join(claude, "skills", "pulse-bridge"), { recursive: true });
  writeFileSync(path.join(claude, ".claude-plugin", "plugin.json"), JSON.stringify({ name: "pulse", description: "Pulse Chat for Claude Code.", version: "0.1.0", author: { name: "Orthic Labs" } }));
  writeFileSync(path.join(claude, "skills", "pulse-bridge", "SKILL.md"), `---\nname: pulse-bridge\ndescription: ${PULSE}\n---\n\n# Pulse\n`);
  assert.equal(detectLayout(claude), "claude-native");
  const claudeResult = validateRepoLayout(claude);
  assert.equal(claudeResult.valid, true, JSON.stringify(claudeResult.findings));
  assert.equal(claudeResult.layout, "claude-native");

  const codex = repo();
  mkdirSync(path.join(codex, ".codex-plugin"));
  writeFileSync(path.join(codex, ".codex-plugin", "plugin.json"), JSON.stringify({ name: "pulse", version: "0.1.0", skills: "./skills/" }));
  assert.equal(detectLayout(codex), "codex-native");
  assert.equal(validateRepoLayout(codex).valid, true);

  assert.equal(detectLayout(repo()), "none");
});

test("host manifests must agree with plugin.json on the name", () => {
  const root = repo();
  scaffoldPlugin(root, { cli: "scraperight", skillDescription: DESCRIPTION });
  writeFileSync(path.join(root, ".claude-plugin", "plugin.json"), JSON.stringify({ name: "something-else" }));
  const { valid, findings } = validateRepoLayout(root);
  assert.equal(valid, false);
  assert.ok(findings.some((item) => item.ruleId === "plugin-host-name-mismatch"));
});

test("plugin validate fails a skill with a vague description; the gate can be switched off in the library", () => {
  const root = repo();
  scaffoldPlugin(root, { cli: "scraperight", skillDescription: DESCRIPTION });
  const skill = path.join(root, "skills", "scraperight", "SKILL.md");
  writeFileSync(skill, readFileSync(skill, "utf8").replace(DESCRIPTION, "A widget tool."));
  const gated = validateRepoLayout(root);
  assert.equal(gated.valid, false);
  assert.ok(gated.findings.some((item) => item.ruleId === "skill-description-length"));
  assert.equal(validateRepoLayout(root, { descriptionGate: false }).valid, true);
});

test("legacy scaffold (no --cli) is unchanged: plugin.json and a skill, mcp.json only on request", () => {
  const root = repo();
  const { written } = scaffoldPlugin(root, { name: "widget-tool", description: DESCRIPTION });
  assert.equal(written.length, 2);
  assert.equal(existsSync(path.join(root, ".claude-plugin")), false);
});

test("right-ax plugin init --cli then plugin validate, end to end", () => {
  const root = repo();
  const init = run(["plugin", "init", root, "--cli", "scraperight", "--skill-description", DESCRIPTION, "--commands", "scrape,crawl", "--version", "0.1.0"]);
  assert.equal(init.status, 0, init.stderr);
  assert.match(init.stdout, /wrote 4 file/);
  const validate = run(["plugin", "validate", root]);
  assert.equal(validate.status, 0, validate.stdout + validate.stderr);
  const report = JSON.parse(validate.stdout);
  assert.equal(report.valid, true);
  assert.equal(report.layout, "portable");
});

test("right-ax plugin init --cli with --hook-args writes the hooks", () => {
  const root = repo();
  const init = run(["plugin", "init", root, "--cli", "pulse", "--skill-description", PULSE, "--hook-args", "chat claude-hook", "--hook-events", "UserPromptSubmit,Stop"]);
  assert.equal(init.status, 0, init.stderr);
  assert.deepEqual(Object.keys(readJson(root, "hooks", "hooks.json").hooks), ["UserPromptSubmit", "Stop"]);
  assert.equal(run(["plugin", "validate", root]).status, 0);
});

test("right-ax plugin init --cli without a description exits 2 with one controlled error", () => {
  const root = repo();
  const init = run(["plugin", "init", root, "--cli", "scraperight"]);
  assert.equal(init.status, 2);
  assert.match(init.stderr, /^right-ax plugin init: CLI-skill scaffolding needs a skill description/);
  assert.doesNotMatch(init.stderr, /\bat\s+.*\.mjs/);
});

test("right-ax plugin validate exits 1 on a gate failure", () => {
  const root = repo();
  writeFileSync(path.join(root, "plugin.json"), JSON.stringify({ $schema: "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json", name: "ok-name" }));
  mkdirSync(path.join(root, "skills", "ok-name"), { recursive: true });
  writeFileSync(path.join(root, "skills", "ok-name", "SKILL.md"), "---\nname: ok-name\ndescription: A tool.\n---\n");
  const validate = run(["plugin", "validate", root]);
  assert.equal(validate.status, 1);
  assert.ok(JSON.parse(validate.stdout).findings.some((item) => item.ruleId === "skill-description-length"));
});
