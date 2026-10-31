// The records Adrian's decisions live in, read by the gate from the COMMIT being built (never the working tree), and the repository rules
// they serve. Three records, one identity test:
//
//  1. .rightkit/broker-exceptions.json (Adrian, 2026-10-31): the one documented exception to "the broker is mandatory on Mac and Dell": a
//     build through the gate WITHOUT the broker, only when Adrian expressly asked or approved it, recorded in the repository being built.
//     The gate refuses unless the id exists, is unexpired when it carries a date, names this host and, in RightKit, covers every selected unit.
//
//       { schema: 1, exceptions: [ { id, approvedBy: "Adrian", quote, issue, units?, hosts, expires? } ] }
//       units: "rust:<crate>" | "npm:<package>" | "*" (all units; only when his words say all). hosts: "mac" | "dell".
//       RightKit only: `units` names what the exception covers; a run with no unit selector (a whole door run) selects every unit, so only a
//       record whose units are ["*"] covers it. Every other repository is built whole (one fixed build definition, below): a broker exception
//       there needs no `units` and covers the repository's run (#1325, Adrian: "the only exception is RightKit, where the unit has to be mentioned").
//       expires (optional): ISO date. Absent = no date: the exception lasts for what he asked for (Adrian, 2026-10-31: "exception lasts for what i
//       asked for, till the time it's published"), which `hosts`, `units` (RightKit) and his quote bound. Present = validated and enforced as ever.
//
//  2. ci/all-units-exceptions.json (RightKit only, #1325): "all units" in RightKit is Adrian's exception, never a default. RightKit's door names
//     its unit on every run: one `--unit`, a unit list written for that run (`--units-file`), or every unit with `--all-units-exception <id>`.
//
//       { exceptions: [ { id, approvedBy: "Adrian", quote, issue, expires? } ] }   (the file CI's dispatch reads, minus its mandatory expiry)
//
//  3. ci/gate.config.json `cargoWorkspaces` (every repository but RightKit, #1325): the fixed, committed build definition. It lists every Cargo
//     workspace of the repository; a gate run builds all of them and nothing is chosen per run. The gate refuses a commit with no list (and prints
//     the exact list it discovered, to commit), and a commit whose discovered workspaces (discoverRustWorkspaces) differ from the list, missing or
//     extra. No grace period and no generated list.
//
//       ci/gate.config.json = { ..., "cargoWorkspaces": [ "<workspace directory, '.' for the root>", ... ] }   (the lock of each is <directory>/Cargo.lock)
//
// RightKit is told apart by the repository identity `check` already uses for RightKit's own rules: the commit holds
// packages/release/package.json named @rightkit/release (isRightKitIdentity). Never a path guess.
export const BROKER_EXCEPTIONS_FILE = ".rightkit/broker-exceptions.json";
export const BROKER_EXCEPTION_SCHEMA = 1;
export const OUTSIDE_BROKER_NOTE = "outside the broker by exception";
export const ALL_UNITS_EXCEPTIONS_FILE = "ci/all-units-exceptions.json";
export const CARGO_WORKSPACE_LIST_FILE = "ci/gate.config.json";
export const CARGO_WORKSPACE_LIST_KEY = "cargoWorkspaces";
export const RIGHTKIT_IDENTITY_FILE = "packages/release/package.json";
export const RIGHTKIT_IDENTITY_NAME = "@rightkit/release";

export const BROKER_EXCEPTION_ERRORS = Object.freeze({
  usage: "RK_BROKER_EXCEPTION_USAGE",
  missing: "RK_BROKER_EXCEPTION_MISSING",
  invalid: "RK_BROKER_EXCEPTION_INVALID",
  expired: "RK_BROKER_EXCEPTION_EXPIRED",
  host: "RK_BROKER_EXCEPTION_HOST",
  unit: "RK_BROKER_EXCEPTION_UNIT",
});

export const ALL_UNITS_ERRORS = Object.freeze({
  usage: "RK_ALL_UNITS_USAGE",
  missing: "RK_ALL_UNITS_MISSING",
  invalid: "RK_ALL_UNITS_INVALID",
  expired: "RK_ALL_UNITS_EXPIRED",
});

export const CARGO_WORKSPACE_LIST_ERRORS = Object.freeze({
  missing: "RK_CARGO_WORKSPACE_LIST_MISSING",
  invalid: "RK_CARGO_WORKSPACE_LIST_INVALID",
  differs: "RK_CARGO_WORKSPACE_LIST_DIFFERS",
});

const HOSTS = Object.freeze(["mac", "dell"]);
const UNIT = /^(?:\*|rust:[A-Za-z0-9][A-Za-z0-9_-]*|npm:(?:@[a-z0-9][a-z0-9._-]*\/)?[a-z0-9][a-z0-9._-]*)$/;
const DATE = /^\d{4}-\d{2}-\d{2}(?:T[0-9:.]+(?:Z|[+-]\d{2}:\d{2}))?$/;

const refuse = (code, message) => Object.assign(new Error(`${code}: ${message}`), { code });

/** True when `packageJsonText` is the package.json of RightKit's own release package: the identity of the RightKit repository. */
export function isRightKitIdentity(packageJsonText) {
  try { return JSON.parse(String(packageJsonText))?.name === RIGHTKIT_IDENTITY_NAME; } catch { return false; }
}

/** The exception host name for a Node platform: Mac and Dell (Windows) only; every other host is not an exception host. */
export function exceptionHostOf(platform = process.platform) {
  return platform === "darwin" ? "mac" : platform === "win32" ? "dell" : null;
}

/** `cargo:rightkit-x` / `npm:@rightkit/x` selectors of `--unit` as record unit names; null for no selector (whole run). */
export function unitsOfSelector(selector) {
  if (selector === undefined || selector === null) return ["*"];
  const cargo = /^cargo:(rightkit-[a-z0-9-]+)$/.exec(selector);
  if (cargo) return [`rust:${cargo[1]}`];
  if (/^npm:@rightkit\/[a-z0-9-]+$/.test(selector)) return [selector];
  return [String(selector)];
}

function expiryOf(value) {
  const at = Date.parse(/T/.test(value) ? value : `${value}T23:59:59.999Z`);
  return Number.isNaN(at) ? null : at;
}

/** What every record entry states: an id, Adrian's approval, his words and the issue that records the decision. Null when it does. */
function statedProblem(entry, { issueIsUrl }) {
  if (!entry || typeof entry !== "object" || Array.isArray(entry)) return "not an object";
  if (typeof entry.id !== "string" || !entry.id) return "id is required";
  if (entry.approvedBy !== "Adrian") return 'approvedBy must be "Adrian"';
  if (typeof entry.quote !== "string" || !entry.quote.trim()) return "quote (his exact words) is required";
  if (issueIsUrl ? typeof entry.issue !== "string" || !/^https:\/\/\S+$/.test(entry.issue) : typeof entry.issue !== "string" || !entry.issue.trim()) return issueIsUrl ? "issue must be an https URL" : "issue is required";
  return null;
}

/** `expires` is optional: absent means no date. Anything present must be an ISO date (null, "" and a malformed value are refused, never read as absent). */
const expiryProblem = (entry) => (Object.hasOwn(entry, "expires") && (typeof entry.expires !== "string" || !DATE.test(entry.expires) || expiryOf(entry.expires) === null) ? "expires, when present, must be an ISO date" : null);

function checkEntry(entry, { inRightKit }) {
  const bad = (what) => refuse(BROKER_EXCEPTION_ERRORS.invalid, `exception ${JSON.stringify(entry?.id)} in ${BROKER_EXCEPTIONS_FILE}: ${what}`);
  const stated = statedProblem(entry, { issueIsUrl: true });
  if (stated) throw bad(stated);
  // `units` bounds an exception in RightKit only. Elsewhere it is optional (a repository is built whole) and, when a record states it, is still
  // read as a unit list so a typo is not silently accepted; it never narrows what a repository's run is covered for.
  if ((inRightKit || Object.hasOwn(entry, "units")) && (!Array.isArray(entry.units) || !entry.units.length || !entry.units.every((u) => typeof u === "string" && UNIT.test(u)))) throw bad('units must be a non-empty list of "rust:<crate>", "npm:<package>" or "*"');
  if (!Array.isArray(entry.hosts) || !entry.hosts.length || !entry.hosts.every((h) => HOSTS.includes(h))) throw bad(`hosts must be a non-empty list of ${HOSTS.join(", ")}`);
  const dated = expiryProblem(entry);
  if (dated) throw bad(dated);
  return entry;
}

/**
 * Judge `--no-broker <id>`. `record` is the text of the committed record (null when the commit holds none). `inRightKit` is the repository
 * identity of the commit (isRightKitIdentity; true unless the caller says otherwise, the stricter reading). Returns the exception (frozen, with
 * the host and the units this run selected) or throws an error carrying a typed `code`.
 */
export function authorizeNoBroker({ id, record, platform = process.platform, units = ["*"], now = Date.now(), inRightKit = true }) {
  if (typeof id !== "string" || !id || id.startsWith("--")) throw refuse(BROKER_EXCEPTION_ERRORS.usage, "--no-broker needs the exception id");
  if (record === null || record === undefined) throw refuse(BROKER_EXCEPTION_ERRORS.missing, `the selected commit holds no ${BROKER_EXCEPTIONS_FILE}; the broker stays mandatory`);
  let parsed;
  try { parsed = JSON.parse(record); } catch (error) { throw refuse(BROKER_EXCEPTION_ERRORS.invalid, `${BROKER_EXCEPTIONS_FILE} is not JSON: ${error.message}`); }
  if (parsed?.schema !== BROKER_EXCEPTION_SCHEMA || !Array.isArray(parsed.exceptions)) throw refuse(BROKER_EXCEPTION_ERRORS.invalid, `${BROKER_EXCEPTIONS_FILE} must be { schema: ${BROKER_EXCEPTION_SCHEMA}, exceptions: [...] }`);
  const matches = parsed.exceptions.filter((e) => e?.id === id);
  if (!matches.length) throw refuse(BROKER_EXCEPTION_ERRORS.missing, `${BROKER_EXCEPTIONS_FILE} has no exception ${JSON.stringify(id)}; the broker stays mandatory`);
  if (matches.length > 1) throw refuse(BROKER_EXCEPTION_ERRORS.invalid, `exception id ${JSON.stringify(id)} appears more than once in ${BROKER_EXCEPTIONS_FILE}`);
  const entry = checkEntry(matches[0], { inRightKit });
  if (entry.expires !== undefined && now > expiryOf(entry.expires)) throw refuse(BROKER_EXCEPTION_ERRORS.expired, `exception ${id} expired ${entry.expires}`);
  const host = exceptionHostOf(platform);
  if (!host || !entry.hosts.includes(host)) throw refuse(BROKER_EXCEPTION_ERRORS.host, `exception ${id} names ${entry.hosts.join(", ")}; this host is ${host ?? platform}`);
  if (inRightKit && !entry.units.includes("*")) {
    const uncovered = units.filter((u) => !entry.units.includes(u));
    if (uncovered.length) throw refuse(BROKER_EXCEPTION_ERRORS.unit, `exception ${id} covers ${entry.units.join(", ")}; not ${uncovered.join(", ")}${units.includes("*") ? " (a run with no --unit selects every unit and needs units [\"*\"])" : ""}`);
  }
  // Outside RightKit the run is the whole repository, whatever units the record happens to list.
  const covered = inRightKit ? [...entry.units] : ["*"];
  return Object.freeze({ exceptionId: entry.id, quote: entry.quote, issue: entry.issue, units: covered, selectedUnits: [...units], host, expires: entry.expires ?? null, inRightKit, note: OUTSIDE_BROKER_NOTE });
}

/** The line a run under an exception prints (and the receipt records the same fields). */
export function describeBrokerException(x) {
  return `exceptionId=${x.exceptionId} host=${x.host} units=${x.units.join(",")} issue=${x.issue} quote=${JSON.stringify(x.quote)}: ${x.note}`;
}

/**
 * Judge `--all-units-exception <id>`: RightKit's whole-repository door run, which is Adrian's exception and never a default. `record` is the
 * text of the commit's ci/all-units-exceptions.json (null when it holds none). Same fields as CI's dispatch reads (id, approvedBy "Adrian", quote,
 * issue); `expires` is optional and honoured when present. Returns the exception (frozen) or throws an error carrying a typed `code`.
 */
export function authorizeAllUnits({ id, record, now = Date.now() }) {
  if (typeof id !== "string" || !id || id.startsWith("--")) throw refuse(ALL_UNITS_ERRORS.usage, "--all-units-exception needs the exception id");
  if (record === null || record === undefined) throw refuse(ALL_UNITS_ERRORS.missing, `the selected commit holds no ${ALL_UNITS_EXCEPTIONS_FILE}; an all-units run is Adrian's exception`);
  let parsed;
  try { parsed = JSON.parse(record); } catch (error) { throw refuse(ALL_UNITS_ERRORS.invalid, `${ALL_UNITS_EXCEPTIONS_FILE} is not JSON: ${error.message}`); }
  if (!Array.isArray(parsed?.exceptions)) throw refuse(ALL_UNITS_ERRORS.invalid, `${ALL_UNITS_EXCEPTIONS_FILE} must be { exceptions: [...] }`);
  const matches = parsed.exceptions.filter((e) => e?.id === id);
  if (!matches.length) throw refuse(ALL_UNITS_ERRORS.missing, `${ALL_UNITS_EXCEPTIONS_FILE} has no exception ${JSON.stringify(id)}; an all-units run is Adrian's exception`);
  if (matches.length > 1) throw refuse(ALL_UNITS_ERRORS.invalid, `exception id ${JSON.stringify(id)} appears more than once in ${ALL_UNITS_EXCEPTIONS_FILE}`);
  const entry = matches[0];
  const problem = statedProblem(entry, { issueIsUrl: false }) ?? expiryProblem(entry);
  if (problem) throw refuse(ALL_UNITS_ERRORS.invalid, `exception ${JSON.stringify(id)} in ${ALL_UNITS_EXCEPTIONS_FILE}: ${problem}`);
  if (entry.expires !== undefined && now > expiryOf(entry.expires)) throw refuse(ALL_UNITS_ERRORS.expired, `exception ${id} expired ${entry.expires}`);
  return Object.freeze({ exceptionId: entry.id, quote: entry.quote, issue: entry.issue, expires: entry.expires ?? null });
}

/** The line an all-units run prints. */
export function describeAllUnitsException(x) {
  return `exceptionId=${x.exceptionId} issue=${x.issue} quote=${JSON.stringify(x.quote)}: every unit of RightKit, by exception`;
}

/** A directory the build definition may name: `.` or a plain repository-relative path (no empty, `.` or `..` segment, no backslash, drive or NUL). */
const plainWorkspaceDir = (d) => typeof d === "string" && d.length > 0 && (d === "." || d.split("/").every((seg) => seg !== "" && seg !== "." && seg !== ".." && !/[\\:\0]/.test(seg)));

/**
 * The fixed build definition of a repository that is not RightKit: compare the Cargo workspaces the commit holds (`discovered`, the
 * `cargoDir` of each discoverRustWorkspaces workspace) with the committed list: the `cargoWorkspaces` array of the commit's ci/gate.config.json
 * (`record` is that file's text, null when the commit holds none). Equal sets pass; a repository with no Cargo workspace needs no list. A
 * commit with workspaces and no list throws, and the message carries the exact list the gate discovered, ready to commit (no grace period, no
 * generated file). A list that differs throws too, naming what is missing from it and what it names that the commit does not hold, and the
 * list that would agree. Returns { workspaces, listed } (frozen).
 */
export function checkCargoWorkspaceList({ discovered, record }) {
  const found = [...new Set(discovered)].sort();
  const snippet = `"${CARGO_WORKSPACE_LIST_KEY}": ${JSON.stringify(found)}`;
  let parsed; let unreadable = "";
  if (record !== null && record !== undefined) {
    try { parsed = JSON.parse(record); } catch (error) { unreadable = ` (${CARGO_WORKSPACE_LIST_FILE} is not valid JSON: ${error.message})`; }
  }
  if (parsed === undefined || parsed === null || typeof parsed !== "object" || Array.isArray(parsed) || !Object.hasOwn(parsed, CARGO_WORKSPACE_LIST_KEY)) {
    if (!found.length) return Object.freeze({ workspaces: [], listed: false });
    throw refuse(CARGO_WORKSPACE_LIST_ERRORS.missing, `the commit holds ${found.length} Cargo workspace(s) and ${CARGO_WORKSPACE_LIST_FILE} names none${unreadable}; every repository but RightKit commits one fixed build definition listing every Cargo workspace, and a run builds all of them. The gate discovered exactly these; commit them in ${CARGO_WORKSPACE_LIST_FILE}: ${snippet}`);
  }
  const listed = parsed[CARGO_WORKSPACE_LIST_KEY];
  if (!Array.isArray(listed) || !listed.every(plainWorkspaceDir)) throw refuse(CARGO_WORKSPACE_LIST_ERRORS.invalid, `${CARGO_WORKSPACE_LIST_FILE} ${CARGO_WORKSPACE_LIST_KEY} must be an array of workspace directories ("." for the root, otherwise a plain repository-relative path), e.g. ${snippet}`);
  const sorted = [...listed].sort();
  const duplicate = sorted.find((d, i) => i > 0 && d === sorted[i - 1]);
  if (duplicate !== undefined) throw refuse(CARGO_WORKSPACE_LIST_ERRORS.invalid, `${CARGO_WORKSPACE_LIST_FILE} ${CARGO_WORKSPACE_LIST_KEY} names the workspace ${JSON.stringify(duplicate)} more than once`);
  const unlisted = found.filter((d) => !sorted.includes(d));
  const absent = sorted.filter((d) => !found.includes(d));
  if (unlisted.length || absent.length) {
    const parts = [];
    if (unlisted.length) parts.push(`the commit holds ${unlisted.join(", ")} which the list does not name`);
    if (absent.length) parts.push(`the list names ${absent.join(", ")} which the commit does not hold`);
    throw refuse(CARGO_WORKSPACE_LIST_ERRORS.differs, `${CARGO_WORKSPACE_LIST_FILE} ${CARGO_WORKSPACE_LIST_KEY} and the commit's Cargo workspaces differ: ${parts.join("; ")}; the build definition and the repository must agree before anything runs. The list that agrees: ${snippet}`);
  }
  return Object.freeze({ workspaces: sorted, listed: true });
}
