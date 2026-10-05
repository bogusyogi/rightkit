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
}

/** Async persistence the host provides (OS keyring in Tauri; Map in tests). */
export interface LicenseStorage {
  get(): Promise<StoredLicense | null>;
  set(lic: StoredLicense | null): Promise<void>;
}

export interface LicenseConfig {
  /** appKey, e.g. 'coderight' — sent as `app` + matched server-side. */
  app: string;
  /** Brand host, e.g. 'coderight.cc' → https://coderight.cc/v1/licenses/*. */
  host: string;
  appVersion: string;
  storage: LicenseStorage;
  /** Stable per-install device id (host derives from keyring/localStorage). */
  deviceId: () => string;
  /** Grace days past `validUntil` for TIME-BOUND licenses before they lapse
   *  offline. Perpetual licenses (validUntil=null) ignore this entirely. */
  graceDays?: number;
  /** Injectable for tests. */
  fetchImpl?: typeof fetch;
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
    const res = await doFetch(endpoint(cfg.host, path), {
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

  /** Re-validate with the server (revocation + fresh entitlements). On network
   *  failure it KEEPS the cached license — grace, not the server, decides offline. */
  async function refresh(): Promise<StoredLicense | null> {
    const cur = await cfg.storage.get();
    if (!cur?.activationToken) return cur ?? null;
    let body: ActivateResponse;
    try {
      body = await post('refresh', { app: cfg.app, activationToken: cur.activationToken, deviceId: cur.deviceId });
    } catch {
      return cur; // offline / server down — cached entitlement stands until grace lapses
    }
    if (body.status !== 'active') {
      const lic: StoredLicense = { ...cur, status: 'invalid', message: body.message ?? 'License is no longer active', lastRefreshedAt: nowIso() };
      await cfg.storage.set(lic);
      return lic;
    }
    const lic: StoredLicense = {
      ...cur,
      status: 'active',
      validUntil: body.validUntil !== undefined ? body.validUntil : (cur.validUntil ?? null),
      entitlements: body.entitlements ?? cur.entitlements,
      signedToken: body.signedToken ?? cur.signedToken, // keep the cached token if the server didn't re-mint
      licenseKind: body.licenseKind ?? cur.licenseKind,
      entitlementStage: body.entitlementStage ?? cur.entitlementStage,
      licenseIssuedAt: body.licenseIssuedAt ?? cur.licenseIssuedAt,
      confirmationDueAt: body.confirmationDueAt !== undefined
        ? body.confirmationDueAt
        : cur.confirmationDueAt,
      lastRefreshedAt: nowIso(),
    };
    await cfg.storage.set(lic);
    return lic;
  }

  /** Free this device's seat + clear local state. Idempotent + best-effort. */
  async function deactivate(): Promise<void> {
    const cur = await cfg.storage.get();
    if (cur?.activationToken) {
      try { await post('deactivate', { app: cfg.app, activationToken: cur.activationToken }); } catch { /* best-effort */ }
    }
    await cfg.storage.set(null);
  }

  return { activate, refresh, deactivate, load: () => cfg.storage.get() };
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
