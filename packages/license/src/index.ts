// @rightkit/license — the ONE license client for every Right Suite app.
//
// Online token-exchange against the unified RightApps backend, with a locally
// cached entitlement + per-kind grace so the app tolerates brief offline use
// while the server stays AUTHORITATIVE (revocation + tier/feature changes land
// at the next refresh). Replaces the five diverged per-app license clients.
//
// Platform-agnostic: the host injects `storage` (keyring-backed in Tauri,
// in-memory in tests) and `deviceId`, so this module has zero Tauri/DOM coupling
// and is fully unit-testable.

export interface Entitlements {
  /** Contract version. Additive-only; bumps on a breaking change. */
  v: number;
  tier: string | null;
  features: string[];
}

export type LicenseTier = 'free' | 'plus' | 'pro' | (string & {});

export interface LicenseRequirement {
  tier?: LicenseTier;
  feature?: string;
  allFeatures?: string[];
  anyFeature?: string[];
}

export type LicenseStatus = 'none' | 'active' | 'invalid' | 'failed';
export type LicenseKind = 'trial' | 'lifetime' | 'term';
export type EntitlementStage = 'time_bound' | 'provisional' | 'final';

export interface StoredLicense {
  status: LicenseStatus;
  deviceId: string;
  /** Raw key, kept only until exchanged for a token (then dropped). */
  key?: string;
  activationToken?: string;
  licenseId?: string;
  /** ISO date the license is valid until. null = perpetual (never expires offline). */
  validUntil?: string | null;
  entitlements?: Entitlements;
  message?: string;
  activatedAt?: string;
  lastRefreshedAt?: string;
  /** v2 Ed25519-signed entitlement token. Opaque to JS — carried through to storage
   *  so the app's native (Rust) layer can verify + enforce it. Absent until the
   *  backend has a signing key configured. */
  signedToken?: string;
  /** Server-owned lifecycle hints for UX. Native authorization trusts signedToken. */
  licenseKind?: LicenseKind;
  entitlementStage?: EntitlementStage;
  licenseIssuedAt?: string;
  confirmationDueAt?: string | null;
  /** ISO time of the FIRST failed refresh of the current offline episode. Present only
   *  while the cached license is stale (the last `refresh()` could not reach the server);
   *  cleared by the next refresh that reaches the server. */
  staleSince?: string;
  /** What the most recent `refresh()` did to this record. */
  lastRefreshOutcome?: RefreshOutcomeKind;
}

/** What a refresh did. Mirrors the native `RefreshOutcome` of `rightkit-license` 0.2.0
 *  (minus `rejected`: JS never verifies the signed token, so it cannot reject one). */
export type RefreshOutcomeKind =
  | 'tokenReplaced'
  | 'tokenKept'
  | 'tokenDropped'
  | 'stale'
  | 'revoked'
  | 'noCredential';

export type RefreshOutcome =
  /** The server minted a new signed token; it replaced the cached one. */
  | { kind: 'tokenReplaced' }
  /** The server said active but sent no token (or the one we already hold); the cached
   *  token is still within its validity, so it was kept. */
  | { kind: 'tokenKept'; reason: 'server_omitted_token' | 'server_returned_same_token'; validUntil: string }
  /** The server said active but there is no usable cached token to keep: either none
   *  was stored, or it is expired/unreadable and was removed from the record. */
  | { kind: 'tokenDropped'; reason: 'no_cached_token' | 'cached_token_lapsed' }
  /** The server could not be reached (offline, 5xx, malformed body). The cached record
   *  is unchanged except for `staleSince`. */
  | { kind: 'stale'; staleSince: string; error: string }
  /** The server authoritatively said the license is no longer active. */
  | { kind: 'revoked'; message?: string }
  /** There is no activation credential to refresh with. Nothing was sent. */
  | { kind: 'noCredential' };

export interface RefreshResult {
  outcome: RefreshOutcome;
  /** The record after the refresh (also what storage now holds), or null if none. */
  license: StoredLicense | null;
}

/** Where a license stands, for the UI. Same names as `rightkit-license`'s `Lifecycle`
 *  (serde snake_case). The Rust value is the authority; this one is derived from the
 *  stored record and is a UI hint only. */
export type Lifecycle =
  | 'unlicensed'
  | 'migration_required'
  | 'trial'
  | 'expired_trial'
  | 'active'
  | 'grace'
  | 'expired'
  | 'lifetime_provisional'
  | 'verification_required'
  | 'lifetime'
  | 'clock_repair_required'
  | 'revoked'
  | 'invalid';

/** Async persistence the host provides (OS keyring in Tauri; Map in tests). */
export interface LicenseStorage {
  get(): Promise<StoredLicense | null>;
  set(lic: StoredLicense | null): Promise<void>;
}

export interface LicenseConfig {
  /** appKey, e.g. 'coderight' — sent as `app` + matched server-side. */
  app: string;
  /** Licence host, e.g. 'heardright.app' → https://heardright.app/v1/licenses/*.
   *  Omit to use `SHARED_LICENSE_HOST`, the shared RightSites licence server;
   *  apps should not register a per-app brand host. */
  host?: string;
  appVersion: string;
  storage: LicenseStorage;
  /** Stable per-install device id (host derives from keyring/localStorage). */
  deviceId: () => string;
  /** Grace days past `validUntil` for TIME-BOUND licenses before they lapse
   *  offline. Perpetual licenses (validUntil=null) ignore this entirely. */
  graceDays?: number;
  /** Injectable for tests. */
  fetchImpl?: typeof fetch;
  /** Injectable clock (epoch ms) for tests. Defaults to `Date.now`. */
  now?: () => number;
}

interface ActivateResponse {
  ok?: boolean;
  status?: string;
  activationToken?: string;
  licenseId?: string;
  validUntil?: string | null;
  entitlements?: Entitlements;
  message?: string;
  signedToken?: string;
  licenseKind?: LicenseKind;
  entitlementStage?: EntitlementStage;
  licenseIssuedAt?: string;
  confirmationDueAt?: string | null;
}

/** The shared RightSites licence server every Right Suite app uses unless it
 *  passes an explicit `host`. Mirrors `rightkit_license::SHARED_LICENSE_HOST`
 *  (Orthic-Labs/rightkit#272). */
export const SHARED_LICENSE_HOST = 'heardright.app';

const endpoint = (host: string, path: string) => `https://${host}/v1/licenses/${path}`;
const nowIso = () => new Date().toISOString();

// In a Tauri app the webview's fetch triggers a CORS preflight that the brand
// hosts don't answer (OPTIONS -> 404 on /v1/licenses/*), so the request never
// leaves the app. Route through the Tauri HTTP plugin (Rust-side, no preflight)
// when the Tauri runtime is present; everywhere else (browser QA, tests, SSR) stay
// on the global fetch. Resolved lazily + memoized so non-Tauri contexts never load
// the plugin. Each consuming app still wires the Rust plugin + capability and adds
// `@tauri-apps/plugin-http` (the SDK can't do those for them) — but no longer needs
// to inject fetchImpl itself.
let tauriHttpFetch: Promise<typeof fetch | undefined> | undefined;
function resolveDefaultFetch(): Promise<typeof fetch | undefined> {
  if (typeof window === 'undefined' || !('__TAURI_INTERNALS__' in window)) {
    return Promise.resolve(undefined);
  }
  if (!tauriHttpFetch) {
    tauriHttpFetch = import('@tauri-apps/plugin-http')
      .then((m) => m.fetch as unknown as typeof fetch)
      .catch(() => undefined);
  }
  return tauriHttpFetch;
}

export function createLicenseClient(cfg: LicenseConfig) {
  async function post(path: string, body: object): Promise<ActivateResponse> {
    // Resolve fetch at CALL time, not client-creation time: a host that spies on
    // the global fetch after this module loads (every Vitest suite) must still be
    // intercepted. Capturing it once would bind the pre-spy reference. Priority:
    // explicit override > Tauri HTTP (in-app, bypasses the CORS preflight + CSP) >
    // global fetch.
    const doFetch = cfg.fetchImpl ?? (await resolveDefaultFetch()) ?? fetch;
    const res = await doFetch(endpoint(cfg.host || SHARED_LICENSE_HOST, path), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
      body: JSON.stringify(body),
    });
    if (!res.ok) {
      // Only explicit client-side rejections are authoritative. A proxy error,
      // malformed body or 5xx must preserve refresh's offline-grace behavior.
      const body = await res.json().catch(() => null) as ActivateResponse | null;
      if (res.status >= 400 && res.status < 500 && body?.ok === false
        && typeof body.status === 'string' && body.status !== 'active') return body;
      const message = typeof body?.message === 'string' ? body.message : undefined;
      const err = new Error(message ?? `Activation request failed (HTTP ${res.status}).`) as Error & { httpStatus?: number };
      err.httpStatus = res.status;
      throw err;
    }
    return (await res.json()) as ActivateResponse;
  }

  /** Exchange a license key for an activation token + entitlements. */
  async function activate(licenseKey: string): Promise<StoredLicense> {
    const deviceId = cfg.deviceId();
    const key = licenseKey.trim();
    let body: ActivateResponse;
    try {
      body = await post('activate', { app: cfg.app, deviceId, licenseKey: key, version: cfg.appVersion });
    } catch (e) {
      const httpStatus = (e as { httpStatus?: number }).httpStatus;
      const lic: StoredLicense = {
        status: 'failed', deviceId, key,
        message: httpStatus ? (e as Error).message : 'Could not reach the activation server.',
      };
      await cfg.storage.set(lic);
      return lic;
    }
    // `status === 'active'` is the single canonical success signal.
    if (body.status !== 'active') {
      const lic: StoredLicense = { status: 'invalid', deviceId, key, message: body.message ?? 'License key was not accepted.' };
      await cfg.storage.set(lic);
      return lic;
    }
    const lic: StoredLicense = {
      status: 'active', deviceId,
      activationToken: body.activationToken,
      key: body.activationToken ? undefined : key, // drop the raw key once we hold a token
      licenseId: body.licenseId,
      validUntil: body.validUntil ?? null,
      entitlements: body.entitlements,
      signedToken: body.signedToken,
      licenseKind: body.licenseKind,
      entitlementStage: body.entitlementStage,
      licenseIssuedAt: body.licenseIssuedAt,
      confirmationDueAt: body.confirmationDueAt,
      activatedAt: nowIso(),
      lastRefreshedAt: nowIso(),
      message: body.message,
    };
    await cfg.storage.set(lic);
    return lic;
  }

  /** Re-validate with the server and report exactly what happened.
   *
   *  - `tokenReplaced`: the server minted a new signed token; it replaced the cached one.
   *  - `tokenKept`: the server said active but sent no token (or the same one); the
   *    cached token is kept ONLY while its own `exp` has not passed.
   *  - `tokenDropped`: the server said active but there is no usable cached token
   *    (none stored, or expired/unreadable); `signedToken` is removed from the record.
   *  - `stale`: the server could not be reached (offline, 5xx, malformed body). The
   *    cached entitlement stands (grace, not the server, decides offline) and
   *    `staleSince` records when this offline episode began.
   *  - `revoked`: the server authoritatively said the license is no longer active.
   *  - `noCredential`: nothing to refresh with; no request was sent. */
  async function refreshWithOutcome(): Promise<RefreshResult> {
    const cur = await cfg.storage.get();
    if (!cur?.activationToken) return { outcome: { kind: 'noCredential' }, license: cur ?? null };
    const nowMs = (cfg.now ?? Date.now)();
    const stamp = new Date(nowMs).toISOString();
    let body: ActivateResponse;
    try {
      body = await post('refresh', { app: cfg.app, activationToken: cur.activationToken, deviceId: cur.deviceId });
    } catch (e) {
      const staleSince = cur.staleSince ?? stamp;
      const lic: StoredLicense = { ...cur, staleSince, lastRefreshOutcome: 'stale' };
      await cfg.storage.set(lic);
      const error = e instanceof Error ? e.message : String(e);
      return { outcome: { kind: 'stale', staleSince, error }, license: lic };
    }
    // The server answered, so any offline episode is over.
    const { staleSince: _cleared, ...settled } = cur;
    if (body.status !== 'active') {
      const message = body.message ?? 'License is no longer active';
      const lic: StoredLicense = { ...settled, status: 'invalid', message, lastRefreshedAt: stamp, lastRefreshOutcome: 'revoked' };
      await cfg.storage.set(lic);
      return { outcome: { kind: 'revoked', message }, license: lic };
    }

    const fresh = typeof body.signedToken === 'string' && body.signedToken.trim() !== '' ? body.signedToken : undefined;
    let signedToken: string | undefined;
    let outcome: RefreshOutcome;
    if (fresh !== undefined && fresh !== cur.signedToken) {
      signedToken = fresh;
      outcome = { kind: 'tokenReplaced' };
    } else if (!cur.signedToken) {
      outcome = { kind: 'tokenDropped', reason: 'no_cached_token' };
    } else {
      // Keep the cached token only while it is still within its own validity. The
      // expiry is read from the unverified payload: a UI-side decision only, native
      // code re-verifies the signature and expiry before anything is authorised.
      const expiresMs = signedTokenExpiryMs(cur.signedToken);
      if (expiresMs !== null && nowMs < expiresMs) {
        signedToken = cur.signedToken;
        outcome = {
          kind: 'tokenKept',
          reason: fresh !== undefined ? 'server_returned_same_token' : 'server_omitted_token',
          validUntil: new Date(expiresMs).toISOString(),
        };
      } else {
        outcome = { kind: 'tokenDropped', reason: 'cached_token_lapsed' };
      }
    }

    const lic: StoredLicense = {
      ...settled,
      status: 'active',
      validUntil: body.validUntil !== undefined ? body.validUntil : (cur.validUntil ?? null),
      entitlements: body.entitlements ?? cur.entitlements,
      signedToken,
      licenseKind: body.licenseKind ?? cur.licenseKind,
      entitlementStage: body.entitlementStage ?? cur.entitlementStage,
      licenseIssuedAt: body.licenseIssuedAt ?? cur.licenseIssuedAt,
      confirmationDueAt: body.confirmationDueAt !== undefined
        ? body.confirmationDueAt
        : cur.confirmationDueAt,
      lastRefreshedAt: stamp,
      lastRefreshOutcome: outcome.kind,
    };
    if (lic.signedToken === undefined) delete lic.signedToken;
    await cfg.storage.set(lic);
    return { outcome, license: lic };
  }

  /** Re-validate with the server (revocation + fresh entitlements) and return the record.
   *  The record says what happened: `lastRefreshOutcome` is `tokenReplaced` / `tokenKept`
   *  / `tokenDropped` / `stale` / `revoked`, and `staleSince` is set while the server is
   *  unreachable. Use `refreshWithOutcome()` for the typed outcome with its reason. */
  async function refresh(): Promise<StoredLicense | null> {
    return (await refreshWithOutcome()).license;
  }

  /** Free this device's seat + clear local state. Idempotent + best-effort. */
  async function deactivate(): Promise<void> {
    const cur = await cfg.storage.get();
    if (cur?.activationToken) {
      try { await post('deactivate', { app: cfg.app, activationToken: cur.activationToken }); } catch { /* best-effort */ }
    }
    await cfg.storage.set(null);
  }

  return { activate, refresh, refreshWithOutcome, deactivate, load: () => cfg.storage.get() };
}

// ── pure entitlement readers (no I/O; safe to call on every render) ───────────

/** Licensed = active AND (perpetual OR still within validUntil + grace). A
 *  perpetual license (validUntil=null) NEVER lapses offline — a lifetime buyer
 *  must not be bricked by our server. Time-bound licenses lapse `graceDays`
 *  after validUntil. */
export function isLicensed(lic: StoredLicense | null, graceDays = 7, now = Date.now()): boolean {
  if (!lic || lic.status !== 'active') return false;
  if (lic.validUntil == null) return true;
  const until = Date.parse(lic.validUntil);
  const exactBoundary = lic.licenseKind === 'trial'
    || lic.entitlementStage === 'provisional';
  if (Number.isNaN(until)) return !exactBoundary;
  const effectiveGraceDays = exactBoundary ? 0 : graceDays;
  return now < until + effectiveGraceDays * 86_400_000;
}

export function tier(lic: StoredLicense | null): string | null {
  return lic?.entitlements?.tier ?? null;
}

export function hasFeature(lic: StoredLicense | null, feature: string): boolean {
  return !!lic?.entitlements?.features?.includes(feature);
}

export function isPro(lic: StoredLicense | null, graceDays = 7, now = Date.now()): boolean {
  return isLicensed(lic, graceDays, now) && tierAtLeast(lic, 'pro');
}

export function tierAtLeast(lic: StoredLicense | null, required: LicenseTier): boolean {
  return tierRank(tier(lic)) >= tierRank(required);
}

export function hasAllFeatures(lic: StoredLicense | null, features: readonly string[]): boolean {
  return features.every((feature) => hasFeature(lic, feature));
}

export function hasAnyFeature(lic: StoredLicense | null, features: readonly string[]): boolean {
  return features.some((feature) => hasFeature(lic, feature));
}

/** Shared JS/UI-side gate. This is a product/UI hint only; native security
 *  boundaries must verify `signedToken` with the canonical crates.io
 *  `rightkit-license` crate and apply the same requirement there. */
export function satisfiesRequirement(
  lic: StoredLicense | null,
  requirement: LicenseRequirement,
  graceDays = 7,
  now = Date.now(),
): boolean {
  if (!isLicensed(lic, graceDays, now)) return false;
  if (requirement.tier && !tierAtLeast(lic, requirement.tier)) return false;
  if (requirement.feature && !hasFeature(lic, requirement.feature)) return false;
  if (requirement.allFeatures && !hasAllFeatures(lic, requirement.allFeatures)) return false;
  if (requirement.anyFeature && !hasAnyFeature(lic, requirement.anyFeature)) return false;
  return true;
}

// ── lifecycle classification (UI hint; the native `Lifecycle` is the authority) ──

export interface ClassifyOptions {
  /** Grace days past `validUntil` for term/legacy/final-lifetime licenses. Default 7,
   *  the same default as `isLicensed`. Trials and provisional lifetime licenses never
   *  get grace. */
  graceDays?: number;
  /** Epoch ms. Default `Date.now()`. */
  now?: number;
  /** Report `migration_required` for an active record that holds an activation
   *  credential but no signed token. Off by default: backends without a signing key
   *  legitimately return none. Turn on in apps whose native layer needs the token. */
  requireSignedToken?: boolean;
  /** The native layer reported a clock rollback. Everything but a final lifetime
   *  license becomes `clock_repair_required`. */
  clockRolledBack?: boolean;
}

type ClaimsShape = 'legacy' | 'term' | 'trial' | 'provisional' | 'final' | 'invalid';

/** kind/stage pairing only. Date-order checks need the signed `iat`, which JS never
 *  reads, so they stay native-only. */
function claimsShape(lic: StoredLicense): ClaimsShape {
  const { licenseKind: kind, entitlementStage: stage } = lic;
  if (kind == null && stage == null) return 'legacy';
  if (kind === 'trial' && stage === 'time_bound') return 'trial';
  if (kind === 'term' && stage === 'time_bound') return 'term';
  if (kind === 'lifetime' && stage === 'provisional') return 'provisional';
  if (kind === 'lifetime' && stage === 'final') return 'final';
  return 'invalid';
}

/**
 * Classify a stored license for the UI. Consistent with `isLicensed`: for a record with
 * `status: 'active'`, `lifecycleGrantsAccess(classifyLifecycle(lic, o))` equals
 * `isLicensed(lic, o.graceDays, o.now)` (except where `requireSignedToken` /
 * `clockRolledBack` / an inconsistent kind-stage pair deny access).
 *
 * Mapping: no record, `none`, `failed` -> `unlicensed`; `invalid` after a refresh
 * revocation (the record still holds credentials) -> `revoked`; any other `invalid` ->
 * `invalid`. Trials and provisional lifetime licenses switch state at exactly
 * `validUntil`; the rest get `graceDays` of `grace` first.
 */
export function classifyLifecycle(lic: StoredLicense | null, opts: ClassifyOptions = {}): Lifecycle {
  const graceDays = opts.graceDays ?? 7;
  const now = opts.now ?? Date.now();
  if (!lic || lic.status === 'none' || lic.status === 'failed') return 'unlicensed';
  if (lic.status === 'invalid') {
    return lic.activationToken || lic.licenseId || lic.signedToken ? 'revoked' : 'invalid';
  }
  if (opts.requireSignedToken && !lic.signedToken) {
    return lic.activationToken ? 'migration_required' : 'invalid';
  }
  const shape = claimsShape(lic);
  if (shape === 'invalid') return 'invalid';
  if (opts.clockRolledBack && shape !== 'final') return 'clock_repair_required';

  const until = lic.validUntil == null ? null : Date.parse(lic.validUntil);
  const live = until === null || (!Number.isNaN(until) && now < until);
  const openEnded = (): Lifecycle => {
    if (live) return shape === 'final' ? 'lifetime' : 'active';
    // An unparseable validUntil never lapses a graced license (same as isLicensed).
    if (until !== null && Number.isNaN(until)) return shape === 'final' ? 'lifetime' : 'active';
    return until !== null && now < until + graceDays * 86_400_000 ? 'grace' : 'expired';
  };

  switch (shape) {
    case 'trial':
      return live ? 'trial' : 'expired_trial';
    case 'provisional':
      return live ? 'lifetime_provisional' : 'verification_required';
    case 'legacy':
    case 'term':
    case 'final':
      return openEnded();
  }
}

/** Whether the UI should treat this lifecycle as entitled. */
export function lifecycleGrantsAccess(lifecycle: Lifecycle): boolean {
  return lifecycle === 'trial' || lifecycle === 'active' || lifecycle === 'grace'
    || lifecycle === 'lifetime_provisional' || lifecycle === 'lifetime';
}

/** States one online refresh can resolve. */
export function lifecycleNeedsOnlineRefresh(lifecycle: Lifecycle): boolean {
  return lifecycle === 'migration_required' || lifecycle === 'verification_required'
    || lifecycle === 'clock_repair_required';
}

/** `exp` of a signed token in epoch ms, read from the UNVERIFIED payload, or null when
 *  the token is not a decodable `payload.signature` pair. A UI-side hint only. */
function signedTokenExpiryMs(token: string): number | null {
  try {
    const dot = token.indexOf('.');
    if (dot <= 0) return null;
    const b64 = token.slice(0, dot).replace(/-/g, '+').replace(/_/g, '/');
    const bytes = Uint8Array.from(atob(b64), (ch) => ch.charCodeAt(0));
    const claims = JSON.parse(new TextDecoder().decode(bytes)) as { exp?: unknown };
    return typeof claims.exp === 'number' && Number.isFinite(claims.exp) ? claims.exp * 1000 : null;
  } catch {
    return null;
  }
}

function tierRank(value: string | null | undefined): number {
  switch (value) {
    case 'pro':
      return 2;
    case 'plus':
      return 1;
    default:
      return 0;
  }
}
