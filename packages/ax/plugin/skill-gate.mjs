// Skill description gate. A skill is only used when its one-line description
// matches what the user asks for, so the description is a gate, not prose.
//
// The rule (every clause is an `error` unless marked `warning`):
//   1. SKILL.md starts with a YAML frontmatter block holding `name` and
//      `description`; `name` equals the skill directory name.
//   2. `description` is ONE physical line: no block scalar (`>`, `|`), no
//      embedded newline.
//   3. Plain (unquoted) values must be YAML-safe: no `: ` and no ` #`.
//      A quoted value must close its quote.
//   4. Length 40..1024 characters. 1024 is the shared hard limit of the skill
//      formats in use; above 300 characters is a `warning` because hosts list
//      descriptions in a bounded budget and truncate the tail, where trigger
//      phrases usually sit.
//   5. It starts with what the tool does: the first word is not an article,
//      a demonstrative, "use", or a generic noun (tool, skill, plugin,
//      helper, utility, wrapper, ...).
//   6. It names concrete trigger phrases: a trigger marker ("use when",
//      "use for", "when the user", "triggers on", "invoke when", ...)
//      followed by at least two distinct, non-generic phrases (comma, "or",
//      or semicolon separated), or at least two quoted phrases anywhere.
//   7. No placeholder or vague text: TODO/TBD/FIXME, lorem, template
//      brackets (<...>, [...], {{...}}), "describe", "your description",
//      the scaffold fallback, filler such as "various tasks", "and more",
//      "etc", "general purpose", "helps with", or fewer than 6 distinct words.

export const DESCRIPTION_MIN = 40;
export const DESCRIPTION_WARN = 300;
export const DESCRIPTION_MAX = 1024;

const BAD_FIRST_WORDS = new Set([
  "a", "an", "the", "this", "that", "these", "those", "it", "its", "my", "our", "your", "some", "any",
  "use", "uses", "used", "using", "for", "to", "about", "with", "helps", "help", "helper", "helpers",
  "tool", "tools", "skill", "skills", "plugin", "plugins", "utility", "utilities", "wrapper", "wrappers",
  "cli", "command", "commands", "agent", "description", "todo", "tbd", "fixme", "n/a", "na", "none",
]);

const PLACEHOLDER_PATTERNS = [
  [/\b(todo|tbd|fixme|xxx|lorem|ipsum)\b/i, "contains a TODO/TBD/lorem marker"],
  [/<[^>]*>|\{\{[^}]*\}\}|\[[^\]]*\]/, "contains a template bracket (<...>, [...], {{...}})"],
  [/\b(describe (?:this|what|the (?:tool|skill|plugin))|your description|description here|fill in|replace this|placeholder)\b/i, "reads as template text"],
  [/^agent-plugins\.org packaging for/i, "is the scaffold fallback description"],
  [/\b(various|miscellaneous|assorted)\s+(tasks|things|operations|stuff)\b/i, "uses filler (various tasks)"],
  [/\b(and more|and so on|and other things|etc\.?|stuff|things like that)\b/i, "uses filler (and more / etc)"],
  [/\b(general[- ]purpose|all[- ]purpose|swiss[- ]army)\b/i, "uses a vague category (general purpose)"],
  [/\bhelps? (you |users? |the user )?(with|to)\b/i, "says it helps with something instead of naming what it does"],
];

// Markers that introduce the trigger clause. Each is followed by the phrases.
const TRIGGER_MARKER = /\b(?:use (?:this )?(?:skill )?(?:when|for|if|to)|use whenever|when the user|when a user|if the user|whenever the user|triggers? (?:on|when|for)|invoke (?:when|for|on)|activate (?:when|for|on)|asked to|asks? (?:to|for|about))\b/i;
const GENERIC_PHRASES = new Set([
  "", "it", "this", "that", "things", "stuff", "anything", "something", "everything", "tasks", "task", "work",
  "needed", "necessary", "relevant", "appropriate", "applicable", "required", "possible", "useful", "helpful",
]);

export function parseSkillFrontmatter(text) {
  const normalized = String(text ?? "").replace(/^﻿/, "").replace(/\r\n?/g, "\n");
  if (!normalized.startsWith("---\n")) return { ok: false, reason: "no frontmatter block (file must start with ---)" };
  const end = normalized.indexOf("\n---", 4);
  if (end === -1) return { ok: false, reason: "frontmatter block is not closed with ---" };
  const lines = normalized.slice(4, end).split("\n");
  const fields = {};
  const raw = {};
  for (let index = 0; index < lines.length; index += 1) {
    const match = /^([A-Za-z][A-Za-z0-9_-]*):(?:\s+(.*)|\s*)$/.exec(lines[index]);
    if (!match) continue;
    const key = match[1];
    const value = (match[2] ?? "").trimEnd();
    // A key with no inline value followed by an indented line is a continued
    // (multi-line) value; record that so the description rule can reject it.
    const next = lines[index + 1];
    const continued = next !== undefined && /^\s+\S/.test(next);
    const folded = /^[>|][+-]?$/.test(value);
    raw[key] = { value, multiline: continued || folded };
    fields[key] = value;
  }
  return { ok: true, fields, raw };
}

function unquote(value) {
  const first = value[0];
  if (first !== '"' && first !== "'") return { text: value, quoted: false, balanced: true };
  const last = value[value.length - 1];
  const balanced = value.length >= 2 && last === first;
  return { text: balanced ? value.slice(1, -1) : value.slice(1), quoted: true, balanced };
}

function finding(ruleId, severity, claim, evidence, remediation) {
  return { ruleId, severity, claim, evidence, remediation };
}

function triggerPhrases(description) {
  const quoted = [...description.matchAll(/["“`]([^"”`]{3,80})["”`]/g)].map((m) => m[1].trim());
  const marker = TRIGGER_MARKER.exec(description);
  let listed = [];
  if (marker) {
    const tail = description.slice(marker.index + marker[0].length);
    listed = tail
      .split(/,|;|\bor\b|\band\b/i)
      .map((part) => part.replace(/[."“”]/g, "").trim().toLowerCase())
      .filter((part) => part.length >= 3 && !GENERIC_PHRASES.has(part));
  }
  const distinct = new Set([...listed, ...quoted.map((q) => q.toLowerCase())]);
  return { marker: Boolean(marker), phrases: [...distinct], quotedCount: new Set(quoted.map((q) => q.toLowerCase())).size };
}

// Returns findings for one SKILL.md. `skillDirName` is the directory the file
// sits in (the skill's id); `skillPath` is only used in evidence text.
export function checkSkillDescription(markdown, { skillDirName, skillPath = "SKILL.md" } = {}) {
  const findings = [];
  const parsed = parseSkillFrontmatter(markdown);
  if (!parsed.ok) {
    findings.push(finding("skill-frontmatter", "error", "SKILL.md has no readable frontmatter", `${skillPath}: ${parsed.reason}`, "start SKILL.md with a --- block containing name: and description:"));
    return findings;
  }
  if (!parsed.fields.name) {
    findings.push(finding("skill-frontmatter", "error", "SKILL.md frontmatter has no name", skillPath, "add name: <skill-directory-name>"));
  } else if (skillDirName && unquote(parsed.fields.name).text !== skillDirName) {
    findings.push(finding("skill-frontmatter", "error", "SKILL.md name does not match its directory", `${skillPath}: name ${JSON.stringify(parsed.fields.name)} vs directory ${JSON.stringify(skillDirName)}`, "make name: equal the skills/<name>/ directory name"));
  }

  const rawDescription = parsed.raw.description;
  if (!rawDescription || !rawDescription.value && !rawDescription.multiline) {
    findings.push(finding("skill-description-present", "error", "SKILL.md has no description", skillPath, "add a one-line description: that says what the tool does and when to use it"));
    return findings;
  }
  if (rawDescription.multiline) {
    findings.push(finding("skill-description-one-line", "error", "skill description spans several lines", `${skillPath}: description uses a block scalar or continuation lines`, "write the description as one physical line"));
    return findings;
  }

  const { text, quoted, balanced } = unquote(rawDescription.value);
  if (quoted && !balanced) {
    findings.push(finding("skill-description-yaml", "error", "skill description has an unclosed quote", `${skillPath}: ${rawDescription.value.slice(0, 80)}`, "close the quote, or drop the quotes"));
  }
  if (!quoted) {
    if (/:\s/.test(text) || /\s#/.test(text)) {
      findings.push(finding("skill-description-yaml", "error", "unquoted description is not valid YAML", `${skillPath}: contains ': ' or ' #' in a plain scalar`, "rephrase without ': ' / ' #', or wrap the whole description in double quotes"));
    }
    if (/^[&*!%@`|>]/.test(text)) {
      findings.push(finding("skill-description-yaml", "error", "unquoted description starts with a YAML indicator", `${skillPath}: starts with ${JSON.stringify(text[0])}`, "start with a letter or wrap the description in double quotes"));
    }
  }

  const description = text.trim();
  if (description.length < DESCRIPTION_MIN) {
    findings.push(finding("skill-description-length", "error", `skill description is shorter than ${DESCRIPTION_MIN} characters`, `${skillPath}: ${description.length} characters`, "say what the tool does and name the requests that should trigger it"));
  } else if (description.length > DESCRIPTION_MAX) {
    findings.push(finding("skill-description-length", "error", `skill description is longer than ${DESCRIPTION_MAX} characters`, `${skillPath}: ${description.length} characters`, "shorten to one line; move detail into the SKILL.md body"));
  } else if (description.length > DESCRIPTION_WARN) {
    findings.push(finding("skill-description-length", "warning", `skill description is longer than ${DESCRIPTION_WARN} characters; hosts may truncate the trigger phrases`, `${skillPath}: ${description.length} characters`, "keep what-it-does and the trigger phrases in the first 300 characters"));
  }

  const firstWord = (/^[A-Za-z][A-Za-z/'-]*/.exec(description)?.[0] ?? "").toLowerCase();
  if (!firstWord || firstWord.length < 3 && !["go"].includes(firstWord) || BAD_FIRST_WORDS.has(firstWord)) {
    findings.push(finding("skill-description-opening", "error", "skill description does not start with what the tool does", `${skillPath}: starts with ${JSON.stringify(description.split(/\s+/)[0] ?? "")}`, "open with a verb phrase naming the capability, for example \"Scrape web pages into ...\""));
  }

  for (const [pattern, why] of PLACEHOLDER_PATTERNS) {
    if (pattern.test(description)) {
      findings.push(finding("skill-description-vague", "error", `skill description ${why}`, `${skillPath}: ${description.slice(0, 120)}`, "replace with concrete capability and trigger text"));
    }
  }
  const words = new Set(description.toLowerCase().match(/[a-z0-9][a-z0-9'/-]*/g) ?? []);
  if (words.size < 6) {
    findings.push(finding("skill-description-vague", "error", "skill description has fewer than 6 distinct words", `${skillPath}: ${words.size} distinct words`, "state what the tool does and when to use it"));
  }

  const triggers = triggerPhrases(description);
  if (triggers.phrases.length < 2) {
    findings.push(finding(
      "skill-description-triggers",
      "error",
      "skill description names fewer than two concrete trigger phrases",
      `${skillPath}: marker ${triggers.marker ? "found" : "missing"}, ${triggers.phrases.length} phrase(s)`,
      "add a clause such as \"use when the user asks to <request>, <request>, or <request>\" with the real requests users make",
    ));
  }
  return findings;
}
