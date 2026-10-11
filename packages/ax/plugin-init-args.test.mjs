import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const CLI = path.join(HERE, "cli", "right-ax.mjs");
const DESCRIPTION = "Scrape web pages into clean markdown with the scraperight CLI; use when the user asks to scrape a URL, crawl a site, or extract page content.";

function tmp(label) {
  return mkdtempSync(path.join(os.tmpdir(), `right-ax-${label}-`));
}

// Runs the CLI with cwd set to a fresh directory so any write to "." is visible.
function runIn(cwd, args) {
  return spawnSync(process.execPath, [CLI, ...args], { encoding: "utf8", cwd });
}

function listTree(root) {
  const entries = [];
  const walk = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, entry.name);
      entries.push(path.relative(root, full).split(path.sep).join("/"));
      if (entry.isDirectory()) walk(full);
    }
  };
  walk(root);
  return entries.sort();
}

test("plugin init --help prints usage, exits 0 and writes nothing in the working directory", () => {
  for (const args of [["plugin", "init", "--help"], ["plugin", "init", "-h"], ["plugin", "init", "--name", "x", "--help"], ["plugin", "init", ".", "--help"]]) {
    const cwd = tmp("init-help");
    const result = runIn(cwd, args);
    assert.equal(result.status, 0, `${args.join(" ")}: ${result.stderr}`);
    assert.match(result.stdout, /usage: right-ax plugin init <repo-path>/);
    assert.deepEqual(listTree(cwd), [], `${args.join(" ")} wrote files`);
  }
});

test("help for other subcommands exits 0 without touching the working directory", () => {
  for (const args of [["plugin", "validate", "--help"], ["marketplace", "build", "-h"], ["marketplace", "validate", "--help"], ["ax", "report", "--help"], ["ax", "behavioral", "--help"], ["adopt", "--help"], ["ownership-review", "prepare", "--help"], ["ownership-review", "verify", "-h"]]) {
    const cwd = tmp("help");
    const result = runIn(cwd, args);
    assert.equal(result.status, 0, `${args.join(" ")}: ${result.stderr}`);
    assert.match(result.stdout, /^usage: right-ax /);
    assert.deepEqual(listTree(cwd), [], `${args.join(" ")} wrote files`);
  }
});

test("an unknown flag exits 2 with no writes", () => {
  const cwd = tmp("unknown-flag");
  const result = runIn(cwd, ["plugin", "init", "target", "--bogus", "--cli", "scraperight", "--skill-description", DESCRIPTION]);
  assert.equal(result.status, 2);
  assert.match(result.stderr, /unknown option --bogus/);
  assert.deepEqual(listTree(cwd), []);
});

test("an unknown flag is refused for every leaf subcommand", () => {
  for (const args of [["plugin", "validate", "--nope"], ["marketplace", "build", "--nope"], ["ax", "static", "--nope"], ["adopt", "--nope"], ["ownership-review", "verify", "--nope"]]) {
    const cwd = tmp("unknown-each");
    const result = runIn(cwd, args);
    assert.equal(result.status, 2, `${args.join(" ")} should exit 2`);
    assert.match(result.stderr, /unknown option --nope/);
    assert.deepEqual(listTree(cwd), [], `${args.join(" ")} wrote files`);
  }
});

test("a target that starts with - is refused with no writes", () => {
  const cwd = tmp("dash-target");
  const result = runIn(cwd, ["plugin", "init", "-rf", "--cli", "scraperight", "--skill-description", DESCRIPTION]);
  assert.equal(result.status, 2);
  assert.match(result.stderr, /starts with - is refused/);
  assert.deepEqual(listTree(cwd), []);
});

test("plugin init without an explicit target is an error and never defaults to the working directory", () => {
  const cwd = tmp("no-target");
  const bare = runIn(cwd, ["plugin", "init"]);
  assert.equal(bare.status, 2);
  assert.match(bare.stderr, /requires an explicit target directory/);
  const flagsOnly = runIn(cwd, ["plugin", "init", "--cli", "scraperight", "--skill-description", DESCRIPTION]);
  assert.equal(flagsOnly.status, 2);
  assert.match(flagsOnly.stderr, /requires an explicit target directory/);
  assert.deepEqual(listTree(cwd), []);
});

test("plugin init creates a missing target directory recursively", () => {
  const cwd = tmp("create-target");
  const target = path.join(cwd, "made", "by", "init");
  const result = runIn(cwd, ["plugin", "init", target, "--cli", "scraperight", "--skill-description", DESCRIPTION, "--version", "0.1.0"]);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(existsSync(path.join(target, "plugin.json")), true);
  assert.equal(existsSync(path.join(target, ".claude-plugin", "plugin.json")), true);
  assert.equal(existsSync(path.join(target, "skills", "scraperight", "SKILL.md")), true);
});

test("a failed scaffold removes the target directory it created", () => {
  const cwd = tmp("rollback");
  const target = path.join(cwd, "fresh");
  const result = runIn(cwd, ["plugin", "init", target, "--cli", "scraperight"]);
  assert.equal(result.status, 2);
  assert.match(result.stderr, /needs a skill description/);
  assert.equal(existsSync(target), false);
});

test("plugin init refuses an existing plugin.json without --force and leaves it untouched", () => {
  const cwd = tmp("existing");
  const target = path.join(cwd, "app");
  mkdirSync(target, { recursive: true });
  writeFileSync(path.join(target, "plugin.json"), "{\"name\":\"keep-me\"}\n");
  const refused = runIn(cwd, ["plugin", "init", target, "--cli", "scraperight", "--skill-description", DESCRIPTION]);
  assert.equal(refused.status, 2);
  assert.match(refused.stderr, /already exists; pass --force to overwrite it/);
  assert.equal(readFileSync(path.join(target, "plugin.json"), "utf8"), "{\"name\":\"keep-me\"}\n");
  assert.equal(existsSync(path.join(target, "skills")), false);

  const forced = runIn(cwd, ["plugin", "init", target, "--cli", "scraperight", "--skill-description", DESCRIPTION, "--force"]);
  assert.equal(forced.status, 0, forced.stderr);
  assert.equal(JSON.parse(readFileSync(path.join(target, "plugin.json"), "utf8")).name, "scraperight");
});

test("adopt honours its positional target and --dry-run as the last token writes nothing", () => {
  const cwd = tmp("adopt-target");
  const repo = path.join(cwd, "harness");
  mkdirSync(path.join(repo, "evals", "ax", "scenarios"), { recursive: true });
  runIn(cwd, ["adopt", repo, "--serve", "node", "--dry-run"]);
  assert.equal(existsSync(path.join(repo, ".rightax.json")), false);
  assert.deepEqual(listTree(cwd), ["harness", "harness/evals", "harness/evals/ax", "harness/evals/ax/scenarios"]);
});
