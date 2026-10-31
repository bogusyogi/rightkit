// The one documented exception to "the broker is mandatory on Mac and Dell" (Adrian, 2026-10-31): a build through the gate WITHOUT the
// broker, only when Adrian expressly asked or approved it, recorded in the repository being built. The gate reads the record from the
// commit snapshot (never the working tree) and refuses unless the id exists, is unexpired, names this host and covers every selected unit.
//
//   .rightkit/broker-exceptions.json = { schema: 1, exceptions: [ { id, approvedBy: "Adrian", quote, issue, units, hosts, expires } ] }
//   units: "rust:<crate>" | "npm:<package>" | "*" (all units; only when his words say all). hosts: "mac" | "dell". expires: ISO date.
//
// A run with no unit selector (a whole door run) selects every unit, so only a record whose units are ["*"] covers it.
export const BROKER_EXCEPTIONS_FILE = ".rightkit/broker-exceptions.json";
export const BROKER_EXCEPTION_SCHEMA = 1;
export const OUTSIDE_BROKER_NOTE = "outside the broker by exception";

export const BROKER_EXCEPTION_ERRORS = Object.freeze({
  usage: "RK_BROKER_EXCEPTION_USAGE",
  missing: "RK_BROKER_EXCEPTION_MISSING",
  invalid: "RK_BROKER_EXCEPTION_INVALID",
  expired: "RK_BROKER_EXCEPTION_EXPIRED",
  host: "RK_BROKER_EXCEPTION_HOST",
  unit: "RK_BROKER_EXCEPTION_UNIT",
});

const HOSTS = Object.freeze(["mac", "dell"]);
const UNIT = /^(?:\*|rust:[A-Za-z0-9][A-Za-z0-9_-]*|npm:(?:@[a-z0-9][a-z0-9._-]*\/)?[a-z0-9][a-z0-9._-]*)$/;
const DATE = /^\d{4}-\d{2}-\d{2}(?:T[0-9:.]+(?:Z|[+-]\d{2}:\d{2}))?$/;

const refuse = (code, message) => Object.assign(new Error(`${code}: ${message}`), { code });

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

function checkEntry(entry) {
  const bad = (what) => refuse(BROKER_EXCEPTION_ERRORS.invalid, `exception ${JSON.stringify(entry?.id)} in ${BROKER_EXCEPTIONS_FILE}: ${what}`);
  if (!entry || typeof entry !== "object" || Array.isArray(entry)) throw bad("not an object");
  if (typeof entry.id !== "string" || !entry.id) throw bad("id is required");
  if (entry.approvedBy !== "Adrian") throw bad('approvedBy must be "Adrian"');
  if (typeof entry.quote !== "string" || !entry.quote.trim()) throw bad("quote (his exact words) is required");
  if (typeof entry.issue !== "string" || !/^https:\/\/\S+$/.test(entry.issue)) throw bad("issue must be an https URL");
  if (!Array.isArray(entry.units) || !entry.units.length || !entry.units.every((u) => typeof u === "string" && UNIT.test(u))) throw bad('units must be a non-empty list of "rust:<crate>", "npm:<package>" or "*"');
  if (!Array.isArray(entry.hosts) || !entry.hosts.length || !entry.hosts.every((h) => HOSTS.includes(h))) throw bad(`hosts must be a non-empty list of ${HOSTS.join(", ")}`);
  if (typeof entry.expires !== "string" || !DATE.test(entry.expires) || expiryOf(entry.expires) === null) throw bad("expires must be an ISO date");
  return entry;
}

/**
 * Judge `--no-broker <id>`. `record` is the text of the committed record (null when the commit holds none). Returns the exception
 * (frozen, with the host and the units this run selected) or throws an error carrying a typed `code`.
 */
export function authorizeNoBroker({ id, record, platform = process.platform, units = ["*"], now = Date.now() }) {
  if (typeof id !== "string" || !id || id.startsWith("--")) throw refuse(BROKER_EXCEPTION_ERRORS.usage, "--no-broker needs the exception id");
  if (record === null || record === undefined) throw refuse(BROKER_EXCEPTION_ERRORS.missing, `the selected commit holds no ${BROKER_EXCEPTIONS_FILE}; the broker stays mandatory`);
  let parsed;
  try { parsed = JSON.parse(record); } catch (error) { throw refuse(BROKER_EXCEPTION_ERRORS.invalid, `${BROKER_EXCEPTIONS_FILE} is not JSON: ${error.message}`); }
  if (parsed?.schema !== BROKER_EXCEPTION_SCHEMA || !Array.isArray(parsed.exceptions)) throw refuse(BROKER_EXCEPTION_ERRORS.invalid, `${BROKER_EXCEPTIONS_FILE} must be { schema: ${BROKER_EXCEPTION_SCHEMA}, exceptions: [...] }`);
  const matches = parsed.exceptions.filter((e) => e?.id === id);
  if (!matches.length) throw refuse(BROKER_EXCEPTION_ERRORS.missing, `${BROKER_EXCEPTIONS_FILE} has no exception ${JSON.stringify(id)}; the broker stays mandatory`);
  if (matches.length > 1) throw refuse(BROKER_EXCEPTION_ERRORS.invalid, `exception id ${JSON.stringify(id)} appears more than once in ${BROKER_EXCEPTIONS_FILE}`);
  const entry = checkEntry(matches[0]);
  if (now > expiryOf(entry.expires)) throw refuse(BROKER_EXCEPTION_ERRORS.expired, `exception ${id} expired ${entry.expires}`);
  const host = exceptionHostOf(platform);
  if (!host || !entry.hosts.includes(host)) throw refuse(BROKER_EXCEPTION_ERRORS.host, `exception ${id} names ${entry.hosts.join(", ")}; this host is ${host ?? platform}`);
  if (!entry.units.includes("*")) {
    const uncovered = units.filter((u) => !entry.units.includes(u));
    if (uncovered.length) throw refuse(BROKER_EXCEPTION_ERRORS.unit, `exception ${id} covers ${entry.units.join(", ")}; not ${uncovered.join(", ")}${units.includes("*") ? " (a run with no --unit selects every unit and needs units [\"*\"])" : ""}`);
  }
  return Object.freeze({ exceptionId: entry.id, quote: entry.quote, issue: entry.issue, units: [...entry.units], selectedUnits: [...units], host, expires: entry.expires, note: OUTSIDE_BROKER_NOTE });
}

/** The line a run under an exception prints (and the receipt records the same fields). */
export function describeBrokerException(x) {
  return `exceptionId=${x.exceptionId} host=${x.host} units=${x.units.join(",")} issue=${x.issue} quote=${JSON.stringify(x.quote)}: ${x.note}`;
}
