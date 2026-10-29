// Loader and hand-rolled validator for `.rightkit/update.json`.
// Mirrors update.schema.json. Never throws: problems are returned as messages.

import fs from "node:fs";
import path from "node:path";
import { LAND_MODES, POLICIES, PLATFORMS, SCHEMA_VERSION, type Policy, type Source, type UpdateManifest } from "./types.js";

export const MANIFEST_RELATIVE_PATH = ".rightkit/update.json";
export const RIGHTKIT_VERSIONS_DEFAULT = "packages/release/rightkit-versions.json";

const SOURCE_KINDS = ["cargo", "pnpm", "swiftpm", "rightkit", "tool"] as const;
const UPSTREAM_TYPES = ["github-release", "url"] as const;
const TOP_LEVEL_KEYS = new Set(["schemaVersion", "sources", "policy", "gates"]);
const POLICY_KEYS = new Set(["default", "held", "tier", "land"]);
const GATE_KEYS = new Set(["name", "command", "cwd", "timeoutSec"]);
const TOOL_KEYS = new Set(["kind", "name", "pin", "assets", "upstream", "journey", "policy"]);
const TOOL_ASSET_KEYS = new Set(["asset", "checksum", "extract", "maxBytes"]);
const MAX_TIMEOUT_SEC = 86_400;
const MODEL_RE = /^[A-Za-z0-9][A-Za-z0-9._:/@+-]{0,127}$/;
const GATE_NAME_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;

const TOOL_NAME_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
const TOOL_PIN_RE = /^[A-Za-z0-9][A-Za-z0-9._+-]{0,63}$/;
const ASSET_NAME_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const CHECKSUM_RE = /^sha256:[0-9a-f]{64}$/;
const GITHUB_REPO_RE = /^[A-Za-z0-9][A-Za-z0-9-]{0,38}\/[A-Za-z0-9._-]{1,100}$/;
const SWIFT_MANIFEST_NAMES = new Set(["Package.swift", "project.yml"]);
/** Journey placeholders. `check` only rejects unknown ones; `gate` fills them in. */
export const JOURNEY_PLACEHOLDERS = Object.freeze(["{candidate}", "{out}", "{delivery}", "{model}"]);

export interface ManifestValidation {
  valid: boolean;
  errors: string[];
  manifest?: UpdateManifest;
}

type Obj = Record<string, unknown>;

function isObject(value: unknown): value is Obj {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isPolicy(value: unknown): value is Policy {
  return typeof value === "string" && (POLICIES as readonly string[]).includes(value);
}

function checkUnknownKeys(obj: Obj, allowed: Set<string>, where: string, errors: string[]): void {
  for (const key of Object.keys(obj)) {
    if (!allowed.has(key)) errors.push(`${where}: unknown field ${key}`);
  }
}

/** Relative path inside the repo: no absolute paths, no parent traversal. */
function safeRelativePath(value: unknown): boolean {
  if (typeof value !== "string" || value.length === 0 || value.length > 512) return false;
  if (path.isAbsolute(value) || value.startsWith("~")) return false;
  return !value.split(/[\\/]/).includes("..");
}

/**
 * A manifest glob: a safe relative path where `*` may appear inside a segment
 * (never `**`, `?`, `[`, `{`), and the last segment is exactly the manifest file name.
 */
export function safeManifestGlob(value: unknown, fileName: string): boolean {
  if (!safeRelativePath(value)) return false;
  const segments = (value as string).split(/[\\/]/);
  if (segments[segments.length - 1] !== fileName) return false;
  return segments.every((seg) => seg.length > 0 && !seg.includes("**") && !/[?[\]{}]/.test(seg));
}

function validateTimeout(value: unknown, where: string, errors: string[]): void {
  if (value === undefined) return;
  if (typeof value !== "number" || !Number.isInteger(value) || value < 1 || value > MAX_TIMEOUT_SEC) {
    errors.push(`${where} must be an integer from 1 to ${MAX_TIMEOUT_SEC}`);
  }
}

function validateGates(value: unknown, errors: string[]): void {
  if (value === undefined) return;
  if (!Array.isArray(value)) {
    errors.push("gates must be an array");
    return;
  }
  const seen = new Set<string>();
  value.forEach((gate, index) => {
    const where = `gates[${index}]`;
    if (!isObject(gate)) {
      errors.push(`${where} must be an object`);
      return;
    }
    checkUnknownKeys(gate, GATE_KEYS, where, errors);
    if (typeof gate.name !== "string" || !GATE_NAME_RE.test(gate.name)) {
      errors.push(`${where}.name must match ${GATE_NAME_RE.source}`);
    } else if (seen.has(gate.name)) {
      errors.push(`${where}.name duplicates ${gate.name}`);
    } else {
      seen.add(gate.name);
    }
    const cmd = gate.command;
    if (!Array.isArray(cmd) || cmd.length === 0 || cmd.some((a) => typeof a !== "string" || a.length === 0 || a.length > 1024)) {
      errors.push(`${where}.command must be a non-empty array of strings`);
    }
    if (gate.cwd !== undefined && !safeRelativePath(gate.cwd)) errors.push(`${where}.cwd must be a relative path`);
    validateTimeout(gate.timeoutSec, `${where}.timeoutSec`, errors);
  });
}

function validateHttps(value: unknown, where: string, errors: string[]): void {
  if (typeof value !== "string") {
    errors.push(`${where} must be a string`);
    return;
  }
  try {
    const url = new URL(value.replace("{version}", "0"));
    if (url.protocol !== "https:") errors.push(`${where} must be an https URL`);
  } catch {
    errors.push(`${where} must be a valid URL`);
  }
}

function validatePolicyConfig(value: unknown, errors: string[]): void {
  if (!isObject(value)) {
    errors.push("policy must be an object");
    return;
  }
  checkUnknownKeys(value, POLICY_KEYS, "policy", errors);
  if (!isPolicy(value.default)) errors.push(`policy.default must be one of ${POLICIES.join(", ")}`);
  if (value.held !== undefined) {
    const nonEmpty = (v: unknown): boolean => typeof v === "string" && v.trim().length > 0;
    const valid = (h: unknown): boolean =>
      nonEmpty(h) || (isObject(h) && ["name", "reason", "evidence", "exitCondition"].every((k) => nonEmpty(h[k])) && Object.keys(h).every((k) => ["name", "reason", "evidence", "exitCondition"].includes(k)));
    if (!Array.isArray(value.held) || !value.held.every(valid)) {
      errors.push("policy.held must be an array of dependency names or { name, reason, evidence, exitCondition } objects (each field a non-empty string)");
    }
  }
  if (value.land !== undefined && !(LAND_MODES as readonly string[]).includes(value.land as string)) {
    errors.push(`policy.land must be one of ${LAND_MODES.join(", ")}`);
  }
  if (value.tier !== undefined) {
    if (!isObject(value.tier)) {
      errors.push("policy.tier must be an object");
    } else {
      checkUnknownKeys(value.tier, new Set(["held"]), "policy.tier", errors);
      if (value.tier.held !== undefined) {
        if (!Array.isArray(value.tier.held) || value.tier.held.some((p) => typeof p !== "string")) {
          errors.push("policy.tier.held must be an array of strings");
        } else {
          for (const p of value.tier.held as string[]) {
            if (p.startsWith("/")) {
              try {
                new RegExp(p.replace(/^\/|\/[a-z]*$/g, ""));
              } catch {
                errors.push(`policy.tier.held has an invalid regex: ${p}`);
              }
            }
          }
        }
      }
    }
  }
}

/** Reject any `{...}` token that is not a known journey placeholder. */
function checkPlaceholders(text: string, where: string, errors: string[]): void {
  for (const token of text.match(/\{[^{}]*\}/g) ?? []) {
    if (!(JOURNEY_PLACEHOLDERS as readonly string[]).includes(token)) {
      errors.push(`${where}: unknown placeholder ${token} (allowed: ${JOURNEY_PLACEHOLDERS.join(", ")})`);
    }
  }
}

function validateTool(source: Obj, where: string, errors: string[]): void {
  checkUnknownKeys(source, TOOL_KEYS, where, errors);
  if (typeof source.name !== "string" || !TOOL_NAME_RE.test(source.name)) {
    errors.push(`${where}: name must match ${TOOL_NAME_RE.source}`);
  }
  if (typeof source.pin !== "string" || !TOOL_PIN_RE.test(source.pin)) {
    errors.push(`${where}: pin must be a version string`);
  }

  if (!isObject(source.assets) || Object.keys(source.assets).length === 0) {
    errors.push(`${where}: assets must be a non-empty object keyed by platform`);
  } else {
    for (const [platform, entry] of Object.entries(source.assets)) {
      const at = `${where}.assets.${platform}`;
      if (!(PLATFORMS as readonly string[]).includes(platform)) {
        errors.push(`${at}: unknown platform (expected ${PLATFORMS.join(", ")})`);
        continue;
      }
      if (!isObject(entry)) {
        errors.push(`${at} must be an object`);
        continue;
      }
      checkUnknownKeys(entry, TOOL_ASSET_KEYS, at, errors);
      if (typeof entry.asset !== "string" || !ASSET_NAME_RE.test(entry.asset)) {
        errors.push(`${at}: asset must be a plain file name`);
      }
      if (typeof entry.checksum !== "string" || !CHECKSUM_RE.test(entry.checksum)) {
        errors.push(`${at}: checksum must be sha256:<64 lowercase hex>`);
      }
      if (entry.maxBytes !== undefined && (typeof entry.maxBytes !== "number" || !Number.isInteger(entry.maxBytes) || entry.maxBytes < 1 || entry.maxBytes > 8 * 1024 ** 3)) {
        errors.push(`${at}.maxBytes must be an integer from 1 to ${8 * 1024 ** 3}`);
      }
      if (entry.extract !== undefined) {
        if (!isObject(entry.extract)) {
          errors.push(`${at}.extract must be an object`);
        } else {
          checkUnknownKeys(entry.extract, new Set(["member"]), `${at}.extract`, errors);
          if (!safeRelativePath(entry.extract.member)) {
            errors.push(`${at}.extract.member must be a relative path inside the archive`);
          }
        }
      }
    }
  }

  if (!isObject(source.upstream)) {
    errors.push(`${where}: upstream is required`);
  } else if (source.upstream.type === "github-release") {
    checkUnknownKeys(source.upstream, new Set(["type", "repo"]), `${where}.upstream`, errors);
    if (typeof source.upstream.repo !== "string" || !GITHUB_REPO_RE.test(source.upstream.repo)) {
      errors.push(`${where}.upstream.repo must be owner/repo`);
    }
  } else if (source.upstream.type === "url") {
    checkUnknownKeys(source.upstream, new Set(["type", "latest", "download"]), `${where}.upstream`, errors);
    validateHttps(source.upstream.latest, `${where}.upstream.latest`, errors);
    validateHttps(source.upstream.download, `${where}.upstream.download`, errors);
    if (typeof source.upstream.download === "string" && !source.upstream.download.includes("{version}")) {
      errors.push(`${where}.upstream.download must contain {version}`);
    }
  } else {
    errors.push(`${where}.upstream.type must be one of ${UPSTREAM_TYPES.join(", ")}`);
  }

  if (!isObject(source.journey)) {
    errors.push(`${where}: journey must be an object {command, report, inputs?}`);
  } else {
    checkUnknownKeys(source.journey, new Set(["command", "report", "inputs", "model", "timeoutSec"]), `${where}.journey`, errors);
    const cmd = source.journey.command;
    if (!Array.isArray(cmd) || cmd.length === 0 || cmd.some((a) => typeof a !== "string" || a.length === 0 || a.length > 1024)) {
      errors.push(`${where}.journey.command must be a non-empty array of strings`);
    } else {
      for (const arg of cmd as string[]) checkPlaceholders(arg, `${where}.journey.command`, errors);
    }
    if (typeof source.journey.report !== "string" || source.journey.report.length === 0) {
      errors.push(`${where}.journey.report must be a non-empty string`);
    } else {
      checkPlaceholders(source.journey.report, `${where}.journey.report`, errors);
    }
    if (source.journey.inputs !== undefined && !isObject(source.journey.inputs)) {
      errors.push(`${where}.journey.inputs must be an object`);
    }
    if (source.journey.model !== undefined && (typeof source.journey.model !== "string" || !MODEL_RE.test(source.journey.model))) {
      errors.push(`${where}.journey.model must be a model id matching ${MODEL_RE.source}`);
    }
    validateTimeout(source.journey.timeoutSec, `${where}.journey.timeoutSec`, errors);
  }
}

/** Validate a parsed manifest. Never throws. */
export function validateManifest(raw: unknown): ManifestValidation {
  const errors: string[] = [];
  if (!isObject(raw)) return { valid: false, errors: ["manifest must be a JSON object"] };

  checkUnknownKeys(raw, TOP_LEVEL_KEYS, "manifest", errors);
  if (raw.schemaVersion !== SCHEMA_VERSION) errors.push(`schemaVersion must be exactly ${SCHEMA_VERSION}`);

  validatePolicyConfig(raw.policy, errors);
  validateGates(raw.gates, errors);

  if (!Array.isArray(raw.sources)) {
    errors.push("sources must be an array");
  } else {
    raw.sources.forEach((source, index) => {
      const where = `sources[${index}]`;
      if (!isObject(source)) {
        errors.push(`${where} must be an object`);
        return;
      }
      if (!(SOURCE_KINDS as readonly string[]).includes(source.kind as string)) {
        errors.push(`${where}: unknown kind ${JSON.stringify(source.kind)} (expected ${SOURCE_KINDS.join(", ")})`);
        return;
      }
      if (source.policy !== undefined && !isPolicy(source.policy)) {
        errors.push(`${where}.policy must be one of ${POLICIES.join(", ")}`);
      }
      switch (source.kind) {
        case "cargo":
        case "pnpm": {
          checkUnknownKeys(source, new Set(["kind", "manifest", "glob", "policy"]), where, errors);
          const fileName = source.kind === "cargo" ? "Cargo.toml" : "package.json";
          if ((source.manifest === undefined) === (source.glob === undefined)) {
            errors.push(`${where}: set exactly one of manifest and glob`);
          } else if (source.manifest !== undefined) {
            if (!safeRelativePath(source.manifest)) errors.push(`${where}.manifest must be a relative path`);
          } else if (!safeManifestGlob(source.glob, fileName)) {
            errors.push(`${where}.glob must be a relative pattern ending in ${fileName}, using * inside a segment only`);
          }
          break;
        }
        case "swiftpm":
          checkUnknownKeys(source, new Set(["kind", "manifest", "policy"]), where, errors);
          if (!safeRelativePath(source.manifest)) {
            errors.push(`${where}.manifest must be a relative path`);
          } else if (!SWIFT_MANIFEST_NAMES.has(path.basename(source.manifest as string))) {
            errors.push(`${where}.manifest must be Package.swift or project.yml`);
          }
          break;
        case "rightkit":
          checkUnknownKeys(source, new Set(["kind", "versions", "policy"]), where, errors);
          if (source.versions !== undefined && !safeRelativePath(source.versions)) {
            errors.push(`${where}.versions must be a relative path`);
          }
          break;
        case "tool":
          validateTool(source, where, errors);
          break;
      }
    });
  }

  if (errors.length > 0) return { valid: false, errors };
  return { valid: true, errors: [], manifest: raw as unknown as UpdateManifest };
}

/** Read and validate `<repo>/.rightkit/update.json`. Never throws. */
export function loadManifest(repoPath: string): ManifestValidation {
  const file = path.join(repoPath, MANIFEST_RELATIVE_PATH);
  let text: string;
  try {
    text = fs.readFileSync(file, "utf8");
  } catch (error) {
    return { valid: false, errors: [`cannot read ${file}: ${(error as Error).message}`] };
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (error) {
    return { valid: false, errors: [`${file} is not valid JSON: ${(error as Error).message}`] };
  }
  return validateManifest(parsed);
}

export type { Source };
