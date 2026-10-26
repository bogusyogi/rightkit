// Ownership class rules for the app check (plan items 0.5 and 0.6). Code is placed by class, never by consumer count:
//   foundation    a row of ownership-mandatory-set.json for the app's profile: the app must use its owner;
//   add-on        any other catalogued capability: RightKit-owned from its first consumer;
//   app-specific  product domain logic and presentation only.
// Non-product code stays in an app only under an approved, expiring entry of .rightkit/ownership-exceptions.json that names the
// RightKit issue that moves it. These two rules run wherever the app check runs (`rightkit-gate check`, `right-release check`, the
// pre-push hook, the start of every checked right-release command and RightKit's own preflight):
//
//   mandatory-foundation  OW-05  an app whose profile requires a foundation owner the app does not declare as a dependency (missing,
//                                or dev-only), or that selects an unknown or unratified profile. An app is a repository whose root
//                                holds its declaration, rightkit-mandatory.json, or that ships a desktop app: a root
//                                right-release.config.(mjs|json), or one beside a src-tauri/tauri.conf.json, checked from its own
//                                directory when that holds a declaration (RightKit's desktop-app-template). An app without the
//                                declaration is a finding, so a missing declaration is never an exemption. A row the app declares not applicable is
//                                excused only by a ratified ruling: appAmendments.<app>.notApplicable.<row> for the `app` the
//                                declaration names and its profile, with the reason copied verbatim; any other not-applicable row is
//                                a finding and the row still applies (rightkit#1140). Owner-pending rows never fail. Below-required versions
//                                and path/git overrides stay with `right-release ownership-mandatory --enforce` (versions move with
//                                every publish). Findings carry an identity, so an app may ratchet them in its baseline.
//   ownership-exceptions  OW-06  every committed .rightkit/ownership-exceptions.json: { schema?: 1, generated?, exceptions: [...] };
//                                each entry holds only the known keys, with capability, package (or crate), reason, owner,
//                                approvedBy, a real `date` not later than today, a real `expires` not before `date`, and `issue`, a
//                                RightKit issue (`<owner>/rightkit#<n>` or https://github.com/<owner>/rightkit/issues/<n>). Schema
//                                findings carry an identity (an adoption ratchet, bounded by the baseline's 90-day sunset); an entry
//                                past its `expires` date is a finding with no identity, so no baseline can silence an expiry. Each
//                                entry must also match a ratified ruling, appAmendments.<app>.exceptions in ownership-mandatory-set.json
//                                for the `app` the root rightkit-mandatory.json names: same capability, package (or crate), path when
//                                the ruling names one, and RightKit issue, expiring no later than the ruling. `approvedBy` alone is
//                                text the app writes; an entry no ruling covers is a finding.
import fs from "node:fs";
import path from "node:path";

const readText = (root, rel) => { try { return fs.readFileSync(path.join(root, rel), "utf8"); } catch { return null; } };
const readJson = (root, rel) => { const text = readText(root, rel); try { return text === null ? null : JSON.parse(text); } catch { return null; } };

// The mandatory-set evaluator parses Cargo.toml (smol-toml). It is loaded only when a checked tree is an app, so a copy of the check
// without it still runs every other rule; an app whose check cannot load it fails the rule ("the rule itself failed").
let mandatorySet = null;
let mandatorySetError = null;
try { mandatorySet = await import("./ownership-mandatory-set.mjs"); } catch (error) { mandatorySetError = error; }

export const APP_MARKERS = Object.freeze(["rightkit-mandatory.json"]);
export const APP_DECLARATION_FILE = "rightkit-mandatory.json";
const RELEASE_CONFIG = /^(?:(.+)\/)?right-release\.config\.(?:mjs|json)$/;
const SKIPPED_TREE = /(^|\/)(node_modules|vendor|target)\//;
const MANDATORY_DATA = new URL("./ownership-mandatory-set.json", import.meta.url);
/** The mandatory-set data, read as JSON (no TOML parser needed), or null when it cannot be read. */
export function mandatoryData() { try { return JSON.parse(fs.readFileSync(MANDATORY_DATA, "utf8")); } catch { return null; } }
export const OWNERSHIP_LEDGER_FILE = ".rightkit/ownership-exceptions.json";
export const LEDGER_TOP_KEYS = Object.freeze(["schema", "generated", "exceptions"]);
export const LEDGER_ENTRY_KEYS = Object.freeze(["capability", "package", "crate", "path", "fingerprint", "reason", "owner", "approvedBy", "date", "expires", "issue"]);
const RIGHTKIT_ISSUE = /^(?:https:\/\/github\.com\/([A-Za-z0-9](?:[A-Za-z0-9-]{0,38}))\/rightkit\/issues\/([1-9]\d*)|([A-Za-z0-9](?:[A-Za-z0-9-]{0,38}))\/rightkit#([1-9]\d*))$/i;
const todayOf = (now) => (now instanceof Date ? now : new Date()).toISOString().slice(0, 10);

/** True when `text` is a real calendar date written YYYY-MM-DD (UTC). */
export function isRealDate(text) {
  if (typeof text !== "string" || !/^\d{4}-\d{2}-\d{2}$/.test(text)) return false;
  const parsed = new Date(`${text}T00:00:00Z`);
  return !Number.isNaN(parsed.getTime()) && parsed.toISOString().slice(0, 10) === text;
}

/** The RightKit issue an `issue` field names, as `<owner>/rightkit#<n>`, or null when it names no RightKit issue. */
export function rightkitIssue(text) {
  const m = typeof text === "string" ? RIGHTKIT_ISSUE.exec(text.trim()) : null;
  return m ? `${m[1] ?? m[3]}/rightkit#${m[2] ?? m[4]}` : null;
}

const nonEmpty = (value) => typeof value === "string" && value.trim().length > 0;

/**
 * Validate one ledger entry. Returns { problems: [string], expired: boolean }; `expired` is set only for an otherwise dated entry
 * whose `expires` is before `today` (YYYY-MM-DD, UTC).
 */
export function validateLedgerEntry(entry, { today = todayOf() } = {}) {
  const problems = [];
  if (!entry || typeof entry !== "object" || Array.isArray(entry)) return { problems: ["is not an object"], expired: false };
  const unknown = Object.keys(entry).filter((key) => !LEDGER_ENTRY_KEYS.includes(key));
  if (unknown.length) problems.push(`unknown key(s) ${unknown.join(", ")} (allowed: ${LEDGER_ENTRY_KEYS.join(", ")})`);
  for (const key of ["capability", "reason", "owner", "approvedBy"]) if (!nonEmpty(entry[key])) problems.push(`missing ${key}`);
  if (entry.package !== undefined && entry.crate !== undefined) problems.push("names both package and crate; keep one");
  else if (!nonEmpty(entry.package ?? entry.crate)) problems.push("missing package (or crate)");
  for (const key of ["path", "fingerprint"]) if (entry[key] !== undefined && !nonEmpty(entry[key])) problems.push(`${key} must be a non-empty string when present`);
  const dated = isRealDate(entry.date);
  if (!dated) problems.push("date must be a real date, YYYY-MM-DD (UTC)");
  else if (entry.date > today) problems.push(`date ${entry.date} is later than today (${today}, UTC)`);
  const expiring = isRealDate(entry.expires);
  if (!expiring) problems.push("expires must be a real date, YYYY-MM-DD (UTC): every exception expires");
  else if (dated && entry.expires < entry.date) problems.push(`expires ${entry.expires} is before date ${entry.date}`);
  if (entry.issue === undefined) problems.push("missing issue: the RightKit issue that moves this code into RightKit");
  else if (!rightkitIssue(entry.issue)) problems.push(`issue ${JSON.stringify(entry.issue)} is not a RightKit issue (<owner>/rightkit#<n> or https://github.com/<owner>/rightkit/issues/<n>)`);
  return { problems, expired: expiring && entry.expires < today };
}

/**
 * Validate one ledger document (parsed JSON, or null when unparseable). Returns [{ problem, fix, subject?, expired? }]:
 * a schema finding carries a `subject`; an expired entry carries `expired: true` and no subject.
 */
export function validateLedger(doc, { today = todayOf() } = {}) {
  if (!doc || typeof doc !== "object" || Array.isArray(doc)) return [{ problem: "is not a JSON object { exceptions: [...] }", fix: "write { \"exceptions\": [] } and add approved, expiring entries", subject: "document" }];
  const out = [];
  const unknown = Object.keys(doc).filter((key) => !LEDGER_TOP_KEYS.includes(key));
  if (unknown.length) out.push({ problem: `unknown top-level key(s) ${unknown.join(", ")} (allowed: ${LEDGER_TOP_KEYS.join(", ")})`, fix: "remove them", subject: "document" });
  if (doc.schema !== undefined && doc.schema !== 1) out.push({ problem: `schema ${JSON.stringify(doc.schema)} is not 1`, fix: "set \"schema\": 1 or drop it", subject: "document" });
  if (!Array.isArray(doc.exceptions)) { out.push({ problem: "exceptions must be an array", fix: "write \"exceptions\": []", subject: "document" }); return out; }
  doc.exceptions.forEach((entry, index) => {
    const label = entry && typeof entry === "object" && !Array.isArray(entry) ? `${entry.capability ?? "?"}/${entry.package ?? entry.crate ?? "?"}${entry.path ? ` ${entry.path}` : ""}` : "?";
    const { problems, expired } = validateLedgerEntry(entry, { today });
    if (problems.length) out.push({ problem: `entry #${index} (${label}): ${problems.join("; ")}`, fix: "every exception needs capability, package, reason, owner, approvedBy, date, expires and the RightKit issue that moves the code", subject: `#${index} ${label}` });
    if (expired) out.push({ problem: `entry #${index} (${label}) expired on ${entry.expires} (today ${today}, UTC); an expired exception is a failure and no baseline lists it`, fix: `move the code into RightKit (${rightkitIssue(entry.issue) ?? "its RightKit issue"}) and remove the entry, or record a fresh approval with a new expires date`, expired: true });
  });
  return out;
}

/**
 * The ratified ruling that covers a ledger entry, or null. `rulings` is appAmendments.<app>.exceptions: [{ capability, package | crate,
 * path?, issue, expires }]. A ruling with no `path` covers every path of its capability and package.
 */
export function rulingFor(entry, rulings) {
  if (!entry || typeof entry !== "object" || !Array.isArray(rulings)) return null;
  const issue = rightkitIssue(entry.issue);
  return rulings.find((r) => r && r.capability === entry.capability && (r.package ?? r.crate) === (entry.package ?? entry.crate)
    && (r.path === undefined || r.path === entry.path) && issue !== null && rightkitIssue(r.issue)?.toLowerCase() === issue.toLowerCase()
    && isRealDate(r.expires) && isRealDate(entry.expires) && entry.expires <= r.expires) ?? null;
}

/** The `app` a root declaration names, or null. */
export function declaredApp(root) {
  const doc = readJson(root, APP_DECLARATION_FILE);
  return doc && typeof doc === "object" && typeof doc.app === "string" && doc.app.trim() ? doc.app.trim() : null;
}

/** The ownership-exceptions rule: every committed ledger in the tree. */
export function ownershipExceptions({ root, files, now }, { data = mandatoryData() } = {}) {
  const today = todayOf(now);
  const app = declaredApp(root);
  const amendment = app && data?.appAmendments ? data.appAmendments[app] : undefined;
  const rulings = amendment && Array.isArray(amendment.exceptions) ? amendment.exceptions : [];
  const ledgers = (files ?? []).filter((file) => file === OWNERSHIP_LEDGER_FILE || file.endsWith(`/${OWNERSHIP_LEDGER_FILE}`)).sort();
  const out = [];
  for (const file of ledgers) {
    const text = readText(root, file);
    let doc = null;
    try { doc = JSON.parse(text); } catch (error) {
      out.push({ file, subject: "document", problem: `${file} is not valid JSON (${error.message})`, fix: "repair the JSON" });
      continue;
    }
    for (const finding of validateLedger(doc, { today })) {
      const { subject, expired, ...rest } = finding;
      out.push({ ...rest, problem: `${file}: ${rest.problem}`, ...(expired ? {} : { file, subject }) });
    }
    (Array.isArray(doc?.exceptions) ? doc.exceptions : []).forEach((entry, index) => {
      if (!entry || typeof entry !== "object" || Array.isArray(entry) || rulingFor(entry, rulings)) return;
      const label = `${entry.capability ?? "?"}/${entry.package ?? entry.crate ?? "?"}${entry.path ? ` ${entry.path}` : ""}`;
      out.push({
        file, subject: `#${index} ${label} ruling`,
        problem: `${file}: entry #${index} (${label}) matches no ratified ruling: ${app ? `appAmendments.${app}.exceptions in ownership-mandatory-set.json has none with this capability, ${entry.crate !== undefined ? "crate" : "package"}, path, issue and an expiry no earlier than ${entry.expires}` : `${APP_DECLARATION_FILE} names no "app", so no ruling can apply`}`,
        fix: `ratify the exception in RightKit (appAmendments.<app>.exceptions, with its issue and expiry) and name the app in ${APP_DECLARATION_FILE}, or move the code into RightKit and remove the entry`,
      });
    });
  }
  return out;
}

/** The marker files that make the checked root an app, in APP_MARKERS order. */
export function appMarkers(root) {
  return APP_MARKERS.filter((name) => readText(root, name) !== null);
}

/**
 * The release configs that make a tree an app even without its declaration: a root right-release.config.(mjs|json), or one whose
 * directory also holds src-tauri/tauri.conf.json (a packaged desktop app).
 */
export function releaseAppMarkers(files) {
  const tracked = new Set(files ?? []);
  return [...tracked].filter((file) => {
    const m = RELEASE_CONFIG.exec(file);
    if (!m || SKIPPED_TREE.test(file)) return false;
    return m[1] === undefined || tracked.has(`${m[1]}/src-tauri/tauri.conf.json`);
  }).sort();
}

/** Split declared not-applicable rows into those a ratified ruling covers and those it does not. */
export function ratifiedNotApplicable(declared, { app, profile, data }) {
  const amendment = app && data?.appAmendments ? data.appAmendments[app] : undefined;
  const rulings = amendment && amendment.profile === profile && amendment.notApplicable && typeof amendment.notApplicable === "object" ? amendment.notApplicable : {};
  const ratified = {}, unratified = {};
  for (const [id, reason] of Object.entries(declared ?? {})) {
    if (typeof rulings[id] === "string" && rulings[id].trim() === reason.trim()) ratified[id] = reason;
    else unratified[id] = reason;
  }
  return { ratified, unratified, ruled: Boolean(amendment) };
}

/** The findings of one app root (`dir` relative to the checked root, "" for the root itself). */
function checkAppRoot({ root, dir, evaluator, data }) {
  const appRoot = dir ? path.join(root, dir) : root;
  const declarationFile = data.appDeclarationFile;
  const file = dir ? `${dir}/${declarationFile}` : declarationFile;
  const out = [];
  if (readJson(appRoot, declarationFile) === null) {
    out.push({ file, subject: "declaration", problem: `${file} is not valid JSON, so the app's profile and not-applicable rows cannot be read`, fix: `repair ${file}` });
    return out;
  }
  const declaration = evaluator.loadAppDeclaration(appRoot, data);
  const app = declaredApp(appRoot);
  const { ratified, unratified, ruled } = ratifiedNotApplicable(declaration.notApplicable, { app, profile: declaration.profile, data });
  for (const [row, reason] of Object.entries(unratified)) out.push({
    file, subject: `notApplicable:${row}`,
    problem: `notApplicable.${row} ("${reason.slice(0, 80)}${reason.length > 80 ? "..." : ""}") is not a ratified ruling: ${!app ? `${file} names no "app"` : !ruled ? `appAmendments has no entry for "${app}"` : `appAmendments.${app} (profile ${data.appAmendments[app].profile}) has no ruling for ${row} with this reason verbatim for profile ${declaration.profile}`}; the row still applies`,
    fix: `ratify the ruling in RightKit (appAmendments.${app ?? "<app>"}.notApplicable.${row}, rightkit#1140) and copy its reason verbatim, or adopt the row's owner`,
  });
  const report = evaluator.checkApp(appRoot, { data, declaration: { ...declaration, notApplicable: ratified } });
  for (const owner of report.owners) {
    if (owner.status === "unknown-profile" || owner.status === "unratified") {
      out.push({ file, subject: owner.owner, problem: `profile "${report.profile}" is ${owner.status === "unknown-profile" ? "not a profile of ownership-mandatory-set.json" : "not ratified (or has no rows)"}, so no foundation set can be checked`, fix: `select a ratified profile (${Object.entries(data.profiles).filter(([, p]) => p.ratified !== false).map(([name]) => name).join(", ")}) in ${file}` });
      continue;
    }
    if (owner.status !== "missing") continue;
    const devOnly = owner.evidence.some((line) => line.includes("dev-dependencies"));
    out.push({
      file, subject: owner.owner,
      problem: `profile "${report.profile}" requires the foundation owner ${owner.owner} (${owner.ecosystem}, row${owner.rows.length > 1 ? "s" : ""} ${owner.rows.join(", ")}) and the app does not use it${devOnly ? " (declared only as a dev dependency)" : ""}`,
      fix: `depend on ${owner.owner} >= ${owner.required} from the registry and use it, or, when the row truly does not apply, declare it under notApplicable in ${file} with the reason RightKit ratified for the app`,
    });
  }
  return out;
}

/**
 * The mandatory-foundation rule. The root declaration makes the whole tree one app. Without it, every release config that ships a
 * desktop app (releaseAppMarkers) is an app: one whose own directory holds a declaration is checked from there; the others are one
 * finding, the missing declaration (no profile is chosen for the app, so none of its rows can be excused or checked).
 */
export function mandatoryFoundation({ root, files }, { evaluator = mandatorySet } = {}) {
  const markers = appMarkers(root);
  const releaseMarkers = markers.length ? [] : releaseAppMarkers(files);
  if (!markers.length && !releaseMarkers.length) return [];
  const out = [];
  const declared = [], undeclared = [];
  for (const marker of releaseMarkers) {
    const dir = marker.includes("/") ? marker.slice(0, marker.lastIndexOf("/")) : "";
    (dir && readText(root, `${dir}/${APP_DECLARATION_FILE}`) !== null ? declared : undeclared).push(dir || marker);
  }
  if (undeclared.length) out.push({
    file: APP_DECLARATION_FILE, subject: "declaration",
    problem: `${undeclared.join(", ")} ships a desktop app, and the repository has no ${APP_DECLARATION_FILE}: an app without its declaration is never exempt from the mandatory foundation set`,
    fix: `commit ${APP_DECLARATION_FILE} at the root (or beside the app's release config) with the app's "app" name, its "profile" and only the not-applicable rows ratified for it in appAmendments`,
  });
  if (!markers.length && !declared.length) return out;
  if (!evaluator) throw new Error(`ownership-mandatory-set.mjs could not be loaded: ${mandatorySetError?.message ?? "unknown error"}`);
  const data = evaluator.loadData();
  if (markers.length) return checkAppRoot({ root, dir: "", evaluator, data });
  for (const dir of declared) out.push(...checkAppRoot({ root, dir, evaluator, data }));
  return out;
}

export const OWNERSHIP_CLASS_RULES = Object.freeze([
  { id: "mandatory-foundation", gap: "OW-05", run: (ctx) => mandatoryFoundation(ctx) },
  { id: "ownership-exceptions", gap: "OW-06", run: ownershipExceptions },
]);
