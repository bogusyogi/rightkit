// TestFlight distribution through the App Store Connect API: wait for one EXACT build to finish processing, assign it to beta
// groups, and (HeardRight parity) put it into external beta review when an external group is selected. A port of HeardRight's
// scripts/testflight-distribute.py to Node, with the JWT signed in-process from the Keychain key (signAppleApiJwtWithKey)
// instead of shelling out to openssl on a .p8 file. No App Store submission happens here: the only review object created is
// the TestFlight beta app review submission, and only when `submitExternalBetaReview` is true.
//
// Everything external is injected (`fetchImpl`, `sleep`, `monotonic`, `now`) so tests run no network and no real wait.
import { signAppleApiJwtWithKey } from "./apple-profile-renew.mjs";

export const ASC_API = "https://api.appstoreconnect.apple.com";
const FAILED_PROCESSING_STATES = new Set(["FAILED", "INVALID"]);
const EXTERNAL_REVIEW_PENDING_OR_READY = new Set(["WAITING_FOR_BETA_REVIEW", "IN_BETA_REVIEW", "READY_FOR_BETA_TESTING", "IN_BETA_TESTING", "BETA_APPROVED"]);
const ACCEPTED_REVIEW_STATES = new Set(["WAITING_FOR_REVIEW", "IN_REVIEW", "APPROVED"]);

export class TestFlightError extends Error {
  constructor(code, message) { super(message); this.name = "TestFlightError"; this.code = code; }
}
const fail = (code, message) => { throw new TestFlightError(code, message); };
const text = (value) => String(value ?? "");

// Apple's limit is 20 minutes; stay well under it (1080 s at most) and renew one minute before expiry, so a 30 minute wait never
// uses a stale token.
export const MAX_JWT_LIFETIME_SECONDS = 1080;
export function createTokenProvider({ privateKey, keyId, issuer, now = () => new Date(), lifetimeSeconds = MAX_JWT_LIFETIME_SECONDS, sign = signAppleApiJwtWithKey }) {
  if (!Number.isInteger(lifetimeSeconds) || lifetimeSeconds < 120 || lifetimeSeconds > MAX_JWT_LIFETIME_SECONDS) fail("TESTFLIGHT_CONFIG_INVALID", `the JWT lifetime must be 120..${MAX_JWT_LIFETIME_SECONDS} seconds`);
  let token = "";
  let expiresAt = 0;
  return {
    get() {
      const current = now();
      const seconds = Math.floor(current.getTime() / 1000);
      if (token && seconds < expiresAt - 60) return token;
      token = sign(privateKey, { keyId, issuer }, current, { lifetimeSeconds });
      expiresAt = seconds + lifetimeSeconds;
      return token;
    },
  };
}

// ---- bounded retry of idempotent calls ----------------------------------------------------------------------------
export const RETRY_DEFAULTS = Object.freeze({ attempts: 5, baseMs: 1000, maxBackoffMs: 30_000, maxRetryAfterMs: 120_000 });
const IDEMPOTENT = new Set(["GET", "HEAD", "PUT", "PATCH", "DELETE"]);

/** Seconds or an HTTP date in a Retry-After header, as milliseconds (undefined when absent or unreadable). */
export function parseRetryAfter(value, nowMs = Date.now()) {
  const raw = String(value ?? "").trim();
  if (!raw) return undefined;
  if (/^\d+$/.test(raw)) return Number(raw) * 1000;
  const date = Date.parse(raw);
  return Number.isNaN(date) ? undefined : Math.max(0, date - nowMs);
}
export const retryableStatus = (status) => status === 429 || (status >= 500 && status <= 599);

/** Delay before retry number `attempt` (0-based): Retry-After when given (capped), else exponential backoff (capped). */
export function retryDelayMs(attempt, retryAfterMs, { baseMs, maxBackoffMs, maxRetryAfterMs } = RETRY_DEFAULTS) {
  if (retryAfterMs !== undefined) return Math.min(retryAfterMs, maxRetryAfterMs);
  return Math.min(baseMs * 2 ** attempt, maxBackoffMs);
}

export function createAscClient({ tokenProvider, fetchImpl = globalThis.fetch, apiBase = ASC_API, timeoutMs = 30_000, sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)), retry = RETRY_DEFAULTS, now = () => Date.now() }) {
  const base = new URL(apiBase);
  // Only idempotent calls (GET, PATCH, PUT, DELETE) are retried, on a network error, 429 or 5xx, honouring Retry-After. A POST is never
  // retried: a create that reached Apple but lost its answer must be looked up, not repeated.
  const request = async (method, pathOrUrl, body, { retries = IDEMPOTENT.has(method) ? retry.attempts - 1 : 0 } = {}) => {
    const url = new URL(pathOrUrl, base);
    if (url.origin !== base.origin || !url.pathname.startsWith("/v1/")) fail("TESTFLIGHT_API_INVALID", "an App Store Connect URL is outside the API origin");
    for (let attempt = 0; ; attempt += 1) {
      let response;
      try {
        response = await fetchImpl(url.href, {
          method,
          headers: { Authorization: `Bearer ${tokenProvider.get()}`, Accept: "application/json", ...(body === undefined ? {} : { "Content-Type": "application/json" }) },
          ...(body === undefined ? {} : { body: JSON.stringify(body) }),
          redirect: "error",
          signal: AbortSignal.timeout(timeoutMs),
        });
      } catch {
        if (attempt < retries) { await sleep(retryDelayMs(attempt, undefined, retry)); continue; }
        fail("TESTFLIGHT_API_FAILED", `App Store Connect ${method} ${url.pathname} request failed`);
      }
      if (!response.ok) {
        if (attempt < retries && retryableStatus(response.status)) {
          await sleep(retryDelayMs(attempt, parseRetryAfter(response.headers?.get?.("retry-after"), now()), retry));
          continue;
        }
        let detail = "";
        try {
          const payload = await response.json();
          detail = (payload?.errors ?? []).map((item) => text(item.detail || item.title)).join("; ");
        } catch { /* no body */ }
        detail = detail.replace(/Bearer\s+\S+/gi, "Bearer [redacted]").replace(/\s+/g, " ").slice(0, 400);
        fail("TESTFLIGHT_API_FAILED", `App Store Connect ${method} ${url.pathname} failed with HTTP ${response.status}${detail ? `: ${detail}` : ""}`);
      }
      if (response.status === 204) return {};
      let payload;
      try { payload = await response.json(); } catch { return {}; }
      return payload ?? {};
    }
  };
  const getAll = async (pathOrUrl) => {
    const items = [];
    const visited = new Set();
    let next = pathOrUrl;
    while (next) {
      if (visited.has(next) || visited.size >= 100) fail("TESTFLIGHT_API_INVALID", "invalid or excessive App Store Connect pagination");
      visited.add(next);
      const payload = await request("GET", next);
      if (!Array.isArray(payload.data)) fail("TESTFLIGHT_API_INVALID", "an App Store Connect collection response was not a list");
      items.push(...payload.data);
      next = payload.links?.next;
    }
    return items;
  };
  return { request, getAll };
}

const groupName = (group) => text(group?.attributes?.name || group?.id);
const isInternal = (group) => Boolean(group?.attributes?.isInternalGroup);

export function normalizeGroupSelection(groups) {
  // No implicit policy: the app says "all" or names its groups.
  if (groups === "all") return "all";
  if (Array.isArray(groups) && groups.length && groups.every((name) => typeof name === "string" && name.trim())) return [...new Set(groups.map((name) => name.trim()))];
  return fail("TESTFLIGHT_CONFIG_INVALID", 'groups is required: "all" or a non-empty list of beta group names');
}

/**
 * Wait for the exact build, assign it to the selected beta groups (re-read until the assignment converges), and handle
 * external beta review. Returns the evidence object (no secret in it).
 */
export async function distributeToTestFlight({
  client, appId, marketingVersion, buildNumber, groups, submitExternalBetaReview = false,
  pollIntervalMs = 20_000, timeoutMs = 30 * 60_000, sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
  monotonic = () => performance.now(), log = () => {},
}) {
  if (!/^\d+$/.test(text(appId))) fail("TESTFLIGHT_CONFIG_INVALID", "appId must be the numeric App Store Connect app id");
  if (!text(marketingVersion) || !text(buildNumber)) fail("TESTFLIGHT_CONFIG_INVALID", "the marketing version and the build number are required");
  const selection = normalizeGroupSelection(groups);
  const deadline = monotonic() + timeoutMs;
  const wait = async (reason) => {
    if (monotonic() >= deadline) fail("TESTFLIGHT_TIMEOUT", `timed out waiting for ${reason}`);
    await sleep(pollIntervalMs);
  };
  const identity = `${marketingVersion} (${buildNumber})`;

  const candidates = async () => {
    const query = new URLSearchParams({ "filter[app]": text(appId), "filter[version]": text(buildNumber), include: "preReleaseVersion", limit: "200" });
    const payload = await client.request("GET", `/v1/builds?${query}`);
    const releases = new Map((payload.included ?? []).filter((item) => item.type === "preReleaseVersions").map((item) => [item.id, text(item.attributes?.version)]));
    return (payload.data ?? []).filter((build) => text(build.attributes?.version) === text(buildNumber)
      && releases.get(build.relationships?.preReleaseVersion?.data?.id) === text(marketingVersion));
  };
  const waitForBuild = async () => {
    for (;;) {
      const matches = await candidates();
      if (matches.length > 1) fail("TESTFLIGHT_AMBIGUOUS_BUILD", `the exact build identity is ambiguous in App Store Connect: ${matches.map((item) => item.id).join(", ")}`);
      if (!matches.length) { await wait(`build ${identity} to appear`); continue; }
      const state = text(matches[0].attributes?.processingState);
      if (state === "VALID") return matches[0];
      if (FAILED_PROCESSING_STATES.has(state)) fail("TESTFLIGHT_PROCESSING_FAILED", `build ${identity} processing failed: ${state}`);
      await wait(`build ${identity} processing`);
    }
  };

  const listGroups = async () => {
    const all = await client.getAll(`/v1/apps/${appId}/betaGroups?limit=200`);
    if (!all.length) fail("TESTFLIGHT_NO_GROUPS", `app ${appId} has no TestFlight beta groups`);
    if (selection === "all") return all;
    const missing = selection.filter((name) => !all.some((group) => groupName(group) === name));
    if (missing.length) fail("TESTFLIGHT_GROUP_MISSING", `beta group(s) not found for app ${appId}: ${missing.join(", ")}`);
    return all.filter((group) => selection.includes(groupName(group)));
  };
  const hasBuild = async (groupId, buildId) => (await client.getAll(`/v1/betaGroups/${groupId}/relationships/builds?limit=200`)).some((item) => text(item.id) === buildId);
  const assignedIds = async (buildId, list) => {
    const ids = new Set();
    for (const group of list) if (await hasBuild(text(group.id), buildId)) ids.add(text(group.id));
    return ids;
  };

  const build = await waitForBuild();
  const buildId = text(build.id);
  log(`TestFlight: build ${identity} is VALID (${buildId})`);

  // An internal group with access to all builds gets every build by itself, and App Store Connect refuses to assign one
  // (HTTP 422 "Cannot add internal group to a build", live run 2026-10-19), so it is not assigned and counts as holding the build.
  const automatic = (group) => isInternal(group) && group.attributes?.hasAccessToAllBuilds === true;
  for (const group of await listGroups()) {
    if (automatic(group)) log(`TestFlight: internal group ${groupName(group)} receives every build (access to all builds)`);
  }
  let converged = false;
  for (let attempt = 0; attempt < 3 && !converged; attempt += 1) {
    const list = (await listGroups()).filter((group) => !automatic(group));
    const assigned = await assignedIds(buildId, list);
    const missing = list.filter((group) => !assigned.has(text(group.id)));
    if (!missing.length) { converged = true; break; }
    for (const group of missing) {
      try {
        await client.request("POST", `/v1/betaGroups/${group.id}/relationships/builds`, { data: [{ type: "builds", id: buildId }] });
      } catch (error) {
        if (!(await hasBuild(text(group.id), buildId))) fail("TESTFLIGHT_ASSIGN_FAILED", `could not assign build ${identity} to beta group ${groupName(group)}: ${error.message}`);
      }
    }
  }
  const finalGroups = (await listGroups()).filter((group) => !automatic(group));
  const finalAssigned = await assignedIds(buildId, finalGroups);
  const unassigned = finalGroups.filter((group) => !finalAssigned.has(text(group.id)));
  if (unassigned.length) fail("TESTFLIGHT_ASSIGN_FAILED", `build assignment did not converge for beta groups: ${unassigned.map(groupName).join(", ")}`);

  const reviewSubmissions = async () => client.getAll(`/v1/betaAppReviewSubmissions?${new URLSearchParams({ "filter[build]": buildId, limit: "200" })}`);
  const betaDetail = async () => {
    const payload = await client.request("GET", `/v1/builds/${buildId}/buildBetaDetail`);
    if (!payload.data || typeof payload.data !== "object") fail("TESTFLIGHT_API_INVALID", "App Store Connect returned no build beta detail");
    return payload.data;
  };
  const stateSet = (submissions) => new Set(submissions.map((item) => text(item.attributes?.betaReviewState)));
  const ensureExternalReview = async () => {
    for (;;) {
      let detail = await betaDetail();
      const state = text(detail.attributes?.externalBuildState);
      let submissions = await reviewSubmissions();
      let states = stateSet(submissions);
      if ([...states].some((item) => ACCEPTED_REVIEW_STATES.has(item))) return { detail, submissions };
      if (submissions.length) fail("TESTFLIGHT_REVIEW_STATE", `external beta review is not pending or approved: ${[...states].sort().join(", ") || "UNKNOWN"}`);
      if (EXTERNAL_REVIEW_PENDING_OR_READY.has(state)) return { detail, submissions };
      if (state === "READY_FOR_BETA_SUBMISSION") {
        await client.request("POST", "/v1/betaAppReviewSubmissions", { data: { type: "betaAppReviewSubmissions", relationships: { build: { data: { type: "builds", id: buildId } } } } });
        submissions = await reviewSubmissions();
        if (!submissions.length) fail("TESTFLIGHT_REVIEW_STATE", "the external beta review submission was not visible after creation");
        states = stateSet(submissions);
        if (![...states].some((item) => ACCEPTED_REVIEW_STATES.has(item))) fail("TESTFLIGHT_REVIEW_STATE", `the external beta review submission has an invalid state: ${[...states].sort().join(", ") || "UNKNOWN"}`);
        detail = await betaDetail();
        return { detail, submissions };
      }
      if (state === "PROCESSING" || state === "") { await wait("the external TestFlight state"); continue; }
      return fail("TESTFLIGHT_REVIEW_STATE", `build cannot enter external TestFlight review: ${state}`);
    }
  };

  let detail = null;
  let submissions = [];
  if (submitExternalBetaReview && finalGroups.some((group) => !isInternal(group))) ({ detail, submissions } = await ensureExternalReview());

  return {
    appId: text(appId),
    marketingVersion: text(marketingVersion),
    buildNumber: text(buildNumber),
    buildId,
    processingState: "VALID",
    groupSelection: selection,
    groups: finalGroups.map((group) => ({ id: text(group.id), name: groupName(group), internal: isInternal(group), assigned: finalAssigned.has(text(group.id)) })),
    externalBuildState: detail ? text(detail.attributes?.externalBuildState) : null,
    betaReviewSubmissionIds: submissions.map((item) => text(item.id)),
    betaReviewStates: submissions.map((item) => text(item.attributes?.betaReviewState)),
  };
}

/** The API plan printed by a dry run (pure data, no secret). */
export function testflightPlan({ appId, marketingVersion, buildNumber, groups, submitExternalBetaReview }) {
  const selection = normalizeGroupSelection(groups);
  return [
    { id: "builds.wait", method: "GET", url: `${ASC_API}/v1/builds?filter[app]=${appId}&filter[version]=${buildNumber}&include=preReleaseVersion`, note: `poll until ${marketingVersion} (${buildNumber}) is VALID` },
    { id: "betaGroups.list", method: "GET", url: `${ASC_API}/v1/apps/${appId}/betaGroups`, note: selection === "all" ? "every beta group" : `groups: ${selection.join(", ")}` },
    { id: "betaGroups.assign", method: "POST", url: `${ASC_API}/v1/betaGroups/<groupId>/relationships/builds`, note: "only for groups that do not hold the build yet; re-read until converged" },
    ...(submitExternalBetaReview ? [{ id: "betaAppReviewSubmissions.create", method: "POST", url: `${ASC_API}/v1/betaAppReviewSubmissions`, note: "only when an external group is selected and the build is READY_FOR_BETA_SUBMISSION" }] : []),
  ];
}
