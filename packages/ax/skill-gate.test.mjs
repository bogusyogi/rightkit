import assert from "node:assert/strict";
import test from "node:test";
import { checkSkillDescription, parseSkillFrontmatter } from "./plugin/skill-gate.mjs";

const GOOD = "Scrape web pages into clean markdown with the scraperight CLI; use when the user asks to scrape a URL, crawl a site, or extract page content.";
const PULSE = "Message AI chats on paired machines through Pulse chat; use when the user mentions another machine, the Dell/Mac, a chat elsewhere, or asks to tell/ask a session on another computer.";

function md(description, name = "demo") {
  const lines = ["---", `name: ${name}`];
  if (description !== null) lines.push(`description: ${description}`);
  lines.push("---", "", "# Demo", "");
  return lines.join("\n");
}

function check(description, name = "demo") {
  return checkSkillDescription(md(description, name), { skillDirName: "demo", skillPath: "skills/demo/SKILL.md" });
}

function errors(findings) {
  return findings.filter((item) => item.severity === "error");
}

function rules(findings) {
  return errors(findings).map((item) => item.ruleId);
}

test("a description that says what the tool does and names trigger phrases passes cleanly", () => {
  assert.deepEqual(check(GOOD), []);
});

test("the Pulse description in use today passes the gate", () => {
  assert.deepEqual(check(PULSE), []);
});

test("quoted trigger phrases satisfy the trigger rule", () => {
  assert.deepEqual(check('Scrape web pages into markdown; triggers on "scrape this site" or "crawl the docs".'), []);
});

test("a quoted description may contain a colon", () => {
  assert.deepEqual(check(`"Scrape pages: use when the user asks to scrape a URL, crawl a site, or extract page content."`), []);
});

test("frontmatter is required", () => {
  const findings = checkSkillDescription("# no frontmatter\n", { skillDirName: "demo" });
  assert.deepEqual(rules(findings), ["skill-frontmatter"]);
});

test("a missing description is an error", () => {
  assert.deepEqual(rules(check(null)), ["skill-description-present"]);
});

test("a skill name that differs from its directory is an error", () => {
  assert.deepEqual(rules(check(GOOD, "other")), ["skill-frontmatter"]);
});

test("a block scalar description is rejected as multi-line", () => {
  const text = `---\nname: demo\ndescription: >\n  ${GOOD}\n---\n`;
  assert.deepEqual(rules(checkSkillDescription(text, { skillDirName: "demo" })), ["skill-description-one-line"]);
});

test("an unquoted description with ': ' is not valid YAML", () => {
  const findings = check("Scrape pages: use when the user asks to scrape a URL, crawl a site, or extract page content.");
  assert.ok(rules(findings).includes("skill-description-yaml"), JSON.stringify(findings));
});

test("an unclosed quote is rejected", () => {
  const findings = check('"Scrape pages; use when the user asks to scrape a URL, crawl a site, or extract page content.');
  assert.ok(rules(findings).includes("skill-description-yaml"), JSON.stringify(findings));
});

test("a description under 40 characters is rejected", () => {
  const findings = check("Scrape pages for users.");
  assert.ok(rules(findings).includes("skill-description-length"));
});

test("a description over 1024 characters is rejected", () => {
  const findings = check(`Scrape pages; use when the user asks to scrape a URL, crawl a site, or extract ${"content ".repeat(150)}`);
  const length = errors(findings).find((item) => item.ruleId === "skill-description-length");
  assert.ok(length, JSON.stringify(findings));
  assert.match(length.claim, /longer than 1024/);
});

test("a description over 300 characters warns but does not fail", () => {
  const findings = check(`Scrape web pages into markdown; use when the user asks to scrape a URL, crawl a site, or extract content. ${"Pages include blogs, docs, and news sites. ".repeat(8)}`);
  assert.deepEqual(errors(findings), []);
  assert.ok(findings.some((item) => item.ruleId === "skill-description-length" && item.severity === "warning"));
});

test("openings that do not say what the tool does are rejected", () => {
  for (const opening of [
    "Use this tool when you need to scrape, crawl, or extract content from sites.",
    "This tool scrapes pages; use when the user asks to scrape, crawl, or extract content.",
    "A scraper for websites; use when the user asks to scrape a URL, crawl a site, or extract content.",
    "The scraperight skill; use when the user asks to scrape a URL, crawl a site, or extract content.",
  ]) {
    assert.ok(rules(check(opening)).includes("skill-description-opening"), opening);
  }
});

test("placeholder and template text is rejected", () => {
  for (const text of [
    "Scrape pages. TODO write trigger text; use when the user asks to scrape, crawl, or extract.",
    "Scrape pages for [site]; use when the user asks to scrape, crawl, or extract content.",
    "Scrape pages for <site>; use when the user asks to scrape, crawl, or extract content.",
    "Agent-plugins.org packaging for scraperight.",
    "Describe this tool here; use when the user asks to scrape, crawl, or extract content.",
  ]) {
    assert.ok(rules(check(text)).includes("skill-description-vague"), text);
  }
});

test("filler such as 'and more', 'etc' and 'helps with' is rejected", () => {
  for (const text of [
    "Scrape pages and more; use when the user asks to scrape, crawl, or extract content.",
    "Scrape pages, crawl sites, etc; use when the user asks to scrape, crawl, or extract content.",
    "Helps with scraping websites; use when the user asks to scrape, crawl, or extract content.",
    "Scrape pages for various tasks; use when the user asks to scrape, crawl, or extract content.",
    "A general purpose scraper; use when the user asks to scrape, crawl, or extract content.",
  ]) {
    assert.ok(rules(check(text)).includes("skill-description-vague"), text);
  }
});

test("a description without trigger phrases is rejected", () => {
  const findings = check("Scrape web pages into clean markdown and structured data for downstream agents.");
  assert.deepEqual(rules(findings), ["skill-description-triggers"]);
});

test("a single trigger phrase is not enough", () => {
  const findings = check("Scrape web pages into markdown; use when the user asks to scrape a URL.");
  assert.deepEqual(rules(findings), ["skill-description-triggers"]);
});

test("every finding carries ruleId, severity, claim, evidence and remediation", () => {
  for (const item of check("Use this.")) {
    for (const key of ["ruleId", "severity", "claim", "evidence", "remediation"]) assert.ok(item[key], `${key} missing on ${JSON.stringify(item)}`);
  }
});

test("parseSkillFrontmatter reads fields and tolerates CRLF", () => {
  const parsed = parseSkillFrontmatter("---\r\nname: demo\r\ndescription: Hello there world\r\n---\r\nbody");
  assert.equal(parsed.ok, true);
  assert.equal(parsed.fields.name, "demo");
  assert.equal(parsed.fields.description, "Hello there world");
});
