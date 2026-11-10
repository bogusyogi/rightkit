// The one reader of a crate's feature sets in ci/gate.config.json `rust.<crate>` (`featureSets`, `checkFeatureSets`) and of its
// `unlintedFeatures`. No imports, by design: scan-digest.mjs (shared with the credentialed receipt verifiers) uses it.
//
// A set is either an argv array (`["--features", "a,b"]`, `[]` for default features; the form every crate used before, unchanged
// in meaning: every host) or a host-scoped object
//
//   { "features": ["--features", "metal,blas"], "hosts": ["darwin"], "hostsReason": "why only those hosts" }
//
// A host-scoped set is a native-host set (rightkit#1231 follow-up): it is planned only on the hosts it names (check-features, clippy, doc, test and
// publisher rows alike), and it plans no cross-target (foreign OS) lint row, because a feature whose build needs one OS's SDK or system library
// cannot be type-checked for another. Host words are `process.platform` words, the same vocabulary and validation style as the crate-level
// `platforms` / `platformsReason` (cross-targets.mjs crateRunsOn). Coverage (FEATURE_UNCOVERED) is judged over every set whatever its hosts: the
// feature is linted on the host whose plan has it.
//
// Every read site goes through `hostFeatureSets` (the sets one host plans), `crossFeatureSets` (the sets whose rows are also linted for
// foreign OS targets) or `allFeatureSets` (coverage), so downstream code keeps seeing plain argv arrays. When a list holds no object entry,
// each function returns the configured array itself: the planned rows and their ids are byte-for-byte what they were.

/** The host words a set may name; equals Object.keys(CROSS_TARGETS) (cross-targets.mjs; a test holds the two equal). */
export const FEATURE_SET_HOSTS = Object.freeze(["darwin", "win32", "linux"]);

const isPlainObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const where = (crate, key, index) => `ci/gate.config.json rust.${crate}.${key}[${index}]`;

/** True for a host-scoped (object) entry. */
export const isScopedFeatureSet = (entry) => isPlainObject(entry);

/**
 * Validates one list and returns `[{ features, hosts }]` (`hosts` is null for an every-host argv array). Throws, naming the crate, on
 * anything malformed: a quietly ignored scope would drop a crate's checks.
 */
export function featureSetEntries(list, key = "featureSets", crate = "<crate>") {
  if (!Array.isArray(list)) throw new Error(`ci/gate.config.json rust.${crate}.${key} must be a list of feature sets, not ${JSON.stringify(list)}`);
  return list.map((entry, index) => {
    if (Array.isArray(entry)) return { features: entry, hosts: null };
    const at = where(crate, key, index);
    if (!isPlainObject(entry)) throw new Error(`${at} must be an argv array or a { features, hosts, hostsReason } object, not ${JSON.stringify(entry)}`);
    const unknown = Object.keys(entry).filter((k) => !["features", "hosts", "hostsReason"].includes(k));
    if (unknown.length) throw new Error(`${at} has unknown key(s) ${unknown.join(", ")}; a host-scoped set is { features, hosts, hostsReason }`);
    if (!Array.isArray(entry.features) || entry.features.some((word) => typeof word !== "string" || !word)) throw new Error(`${at}.features must be an argv array of non-empty strings`);
    const hosts = entry.hosts;
    if (!Array.isArray(hosts) || hosts.length === 0 || hosts.some((host) => !FEATURE_SET_HOSTS.includes(host)) || new Set(hosts).size !== hosts.length) {
      throw new Error(`${at}.hosts must be a non-empty list of distinct hosts from ${FEATURE_SET_HOSTS.join(", ")}, not ${JSON.stringify(hosts)}`);
    }
    // A host scope narrows "every set on every host" (GC-A14), so it says why, as crate-level `platforms` does.
    if (typeof entry.hostsReason !== "string" || !entry.hostsReason.trim()) throw new Error(`${at} needs a non-empty hostsReason (why the feature set builds on those hosts only)`);
    return { features: entry.features, hosts };
  });
}

// True when a list holds anything but plain argv arrays: the one case that is read through the validator (a list of arrays is used as it is).
const hasScoped = (list) => Array.isArray(list) && list.some((entry) => !Array.isArray(entry));
const needPlatform = (crate, key) => new Error(`ci/gate.config.json rust.${crate}.${key} has host-scoped sets: the host's platform (darwin, win32 or linux) is required to plan them`);

/**
 * The sets `entry[key]` plans on `platform`, as argv arrays: every unscoped set and each scoped set that names the host. `undefined` when
 * the entry has no such list (callers keep their own default). A list with no object entry is returned as it is.
 */
export function hostFeatureSets(entry, platform, key = "featureSets", crate = "<crate>") {
  const list = entry?.[key];
  if (list === undefined || !hasScoped(list)) return list;
  if (!FEATURE_SET_HOSTS.includes(platform)) throw needPlatform(crate, key);
  return featureSetEntries(list, key, crate).filter((set) => set.hosts === null || set.hosts.includes(platform)).map((set) => set.features);
}

/** The sets of `hostFeatureSets` that are also linted for the other OSes' targets: the unscoped ones. A host-scoped set is a native-host set. */
export function crossFeatureSets(entry, platform, key = "featureSets", crate = "<crate>") {
  const list = entry?.[key];
  if (list === undefined || !hasScoped(list)) return list;
  if (!FEATURE_SET_HOSTS.includes(platform)) throw needPlatform(crate, key);
  return featureSetEntries(list, key, crate).filter((set) => set.hosts === null).map((set) => set.features);
}

/** Every set of `entry[key]` whatever its hosts, as argv arrays (feature coverage). `undefined` when the entry has no such list. */
export function allFeatureSets(entry, key = "featureSets", crate = "<crate>") {
  const list = entry?.[key];
  if (list === undefined || !hasScoped(list)) return list;
  return featureSetEntries(list, key, crate).map((set) => set.features);
}

/** True when `entry` declares any host-scoped set in either list (validated: a malformed object throws). */
export function hasHostScopedSets(entry, crate = "<crate>") {
  let scoped = false;
  for (const key of ["featureSets", "checkFeatureSets"]) {
    if (entry?.[key] === undefined) continue;
    featureSetEntries(entry[key], key, crate);
    if (hasScoped(entry[key])) scoped = true;
  }
  return scoped;
}

/**
 * `unlintedFeatures`: `{ "<feature>": "<reason>" }`, the features the gate never lints (rightkit-tts-qwen-sys `regenerate-bindings` rewrites
 * checked-in files and needs libclang). Returns `{ feature: reason }` (empty when the crate names none). Throws on a malformed value. Whether a
 * name is a declared feature, and whether a set enables it, is feature-coverage.mjs `featureCoverageProblems`.
 */
export function unlintedFeatures(entry, crate = "<crate>") {
  const value = entry?.unlintedFeatures;
  if (value === undefined) return {};
  const at = `ci/gate.config.json rust.${crate}.unlintedFeatures`;
  if (!isPlainObject(value) || Object.keys(value).length === 0) throw new Error(`${at} must be an object of { "<feature>": "<reason>" }`);
  for (const [feature, reason] of Object.entries(value)) {
    if (!feature.trim()) throw new Error(`${at} names an empty feature`);
    if (typeof reason !== "string" || !reason.trim()) throw new Error(`${at}.${feature} needs a non-empty reason (why the gate never lints it)`);
  }
  return value;
}

/**
 * The check sets a crate with no `checkFeatureSets` gets: one `--all-features` check, unless the crate names unlinted features, which
 * `--all-features` would switch on (a feature named there is never added to any generated set).
 */
export const defaultCheckFeatureSets = (entry, crate = "<crate>") => (Object.keys(unlintedFeatures(entry, crate)).length ? [] : [["--all-features"]]);
