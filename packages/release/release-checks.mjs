/**
 * Run only app-configured pre-build checks; release safeguards never route here.
 *
 * A `checks` entry is a package script name (it passes when the script exits 0) or an object
 * `{ script, expectOutput?, forbidOutput? }` (rightkit#753): the script must exit 0 AND its standard output must contain every
 * `expectOutput` string and none of the `forbidOutput` strings (plain, case-sensitive substrings; a string or a list of strings).
 * The output is captured, echoed, and judged here; a dry run captures nothing, so it asserts nothing.
 */
export class ReleaseCheckError extends Error {
  constructor(message) { super(message); this.name = "ReleaseCheckError"; this.code = "RK_RELEASE_CHECK"; }
}

const SCRIPT = /^[^\s\0]+$/;
const ENTRY_KEYS = new Set(["script", "expectOutput", "forbidOutput"]);

const stringList = (value, label) => {
  if (value === undefined) return [];
  const list = Array.isArray(value) ? value : [value];
  if (!list.length || list.some((item) => typeof item !== "string" || item === "")) throw new ReleaseCheckError(`${label} must be a non-empty string or a list of non-empty strings`);
  return list;
};

/** Validate one `checks` entry and return `{ script, expect, forbid }`. */
export function releaseCheckEntry(entry, index = 0) {
  const label = `checks[${index}]`;
  if (typeof entry === "string" || entry === false) return { script: entry, expect: [], forbid: [] }; // exit status only, exactly as before (false is a no-op)
  if (entry === null || typeof entry !== "object" || Array.isArray(entry)) throw new ReleaseCheckError(`${label} must be a script name or { script, expectOutput?, forbidOutput? }`);
  const unknown = Object.keys(entry).filter((key) => !ENTRY_KEYS.has(key));
  if (unknown.length) throw new ReleaseCheckError(`${label} has unknown key(s): ${unknown.join(", ")} (allowed: ${[...ENTRY_KEYS].join(", ")})`);
  if (typeof entry.script !== "string" || !SCRIPT.test(entry.script)) throw new ReleaseCheckError(`${label}.script must be a package script name`);
  const expect = stringList(entry.expectOutput, `${label}.expectOutput`);
  const forbid = stringList(entry.forbidOutput, `${label}.forbidOutput`);
  if (!expect.length && !forbid.length) throw new ReleaseCheckError(`${label} names no expectOutput or forbidOutput; use the script name alone for an exit-status check`);
  return { script: entry.script, expect, forbid };
}

/** The assertion problems of a captured output (empty when it passes). `null` output (a dry run) asserts nothing. */
export function releaseCheckOutputProblems({ script, expect, forbid }, output) {
  if (output === null || output === undefined) return [];
  const text = String(output);
  return [
    ...expect.filter((needle) => !text.includes(needle)).map((needle) => `check ${JSON.stringify(script)} output does not contain ${JSON.stringify(needle)}`),
    ...forbid.filter((needle) => text.includes(needle)).map((needle) => `check ${JSON.stringify(script)} output contains the forbidden ${JSON.stringify(needle)}`),
  ];
}

export async function runConfiguredChecks({ packageManager, checks = [], workdir, skipChecks = false, run }) {
  if (skipChecks) return;
  const entries = checks.map((entry, index) => releaseCheckEntry(entry, index));
  for (const entry of entries) {
    if (!entry.expect.length && !entry.forbid.length) { await run(packageManager, entry.script, workdir); continue; }
    const output = await run(packageManager, entry.script, workdir, { captureStdout: true });
    const problems = releaseCheckOutputProblems(entry, output);
    if (problems.length) throw new ReleaseCheckError(problems.join("; "));
  }
}
