import type { LicenseBackend, LicenseState, SignInBackend, SignInProvider } from "./types.js";

/**
 * The fields of `@rightkit/license` 0.3.0 `LicenseView` that the adapter reads. Structural on purpose: this package does not
 * import the licence package, so `createLicenseClient(...)` output is accepted as it is and an app that has no licence step
 * never loads it.
 */
export interface LicenseViewLike {
  status: string;
  lifecycle?: string;
  tier?: string | null;
  /** Unix milliseconds. */
  validUntil?: number | null;
  /** A closed `MessageCode`; text comes from `messageText`. */
  message?: string;
}

/** The slice of `createLicenseClient(...)` the licence step needs. */
export interface LicenseViewClient {
  status(): Promise<LicenseViewLike>;
  activate(key: string): Promise<LicenseViewLike>;
  /** Present on the real client. A trial needs `trialEmail` as well. */
  requestTrial?(email: string): Promise<{ accepted: boolean; message?: string }>;
}

export interface LicenseViewBackendOptions {
  /** Text for a `MessageCode`: pass `licenseMessage` from `@rightkit/license`. Without it the step shows no detail text. */
  messageText?: (code: string) => string;
  /**
   * Where the trial email comes from (an input the app renders, or the signed-in account). A trial is offered only when this
   * and the client's `requestTrial` both exist. Returning nothing refuses the trial with a message.
   */
  trialEmail?: () => string | undefined | Promise<string | undefined>;
  purchaseUrl?: string;
  openExternal?: (url: string) => void | Promise<void>;
  /** Replaces the English status labels. */
  labels?: Partial<Record<LicenseState["status"], string>>;
}

const BY_LIFECYCLE: Readonly<Record<string, LicenseState["status"]>> = {
  unlicensed: "none",
  migration_required: "invalid",
  trial: "trial",
  expired_trial: "expired",
  active: "active",
  grace: "active",
  lifetime: "active",
  lifetime_provisional: "active",
  verification_required: "active",
  expired: "expired",
  clock_repair_required: "invalid",
  revoked: "invalid",
  invalid: "invalid",
};

const BY_STATUS: Readonly<Record<string, LicenseState["status"]>> = {
  none: "none",
  active: "active",
  grace: "active",
  expired: "expired",
  revoked: "invalid",
  invalid: "invalid",
  unverifiable: "invalid",
};

const DEFAULT_LABELS: Readonly<Record<LicenseState["status"], string>> = {
  none: "No licence",
  trial: "Trial",
  active: "Active",
  expired: "Expired",
  invalid: "Needs attention",
};

/** Maps a native `LicenseView` to the step's `LicenseState`. The lifecycle decides; the coarse status is the fallback. An unknown value is `invalid`, never `active`. */
export function licenseStateFromView(view: LicenseViewLike, options: Pick<LicenseViewBackendOptions, "messageText" | "labels"> = {}): LicenseState {
  const status = (view.lifecycle !== undefined ? BY_LIFECYCLE[view.lifecycle] : undefined) ?? BY_STATUS[view.status] ?? "invalid";
  const text = view.message !== undefined && view.message !== "none" ? options.messageText?.(view.message) : undefined;
  const validUntil = typeof view.validUntil === "number" && Number.isFinite(view.validUntil) ? new Date(view.validUntil).toISOString() : null;
  return {
    status,
    label: options.labels?.[status] ?? DEFAULT_LABELS[status],
    ...(text ? { detail: text } : {}),
    ...(typeof view.tier === "string" && view.tier ? { tier: view.tier } : {}),
    validUntil,
  };
}

/**
 * `LicenseBackend` over a native licence client (`@rightkit/license` 0.3.0 `createLicenseClient`). The step shows the state
 * the native service reports and never decides entitlement itself; errors from the client reach the step unchanged (they are
 * closed, fixed-text `LicenseClientError`s).
 */
export function licenseBackendFromView(client: LicenseViewClient, options: LicenseViewBackendOptions = {}): LicenseBackend {
  const state = (view: LicenseViewLike) => licenseStateFromView(view, options);
  const { requestTrial } = client;
  const { trialEmail } = options;
  return {
    load: async () => state(await client.status()),
    activate: async (key) => state(await client.activate(key)),
    ...(requestTrial && trialEmail ? {
      startTrial: async () => {
        const email = (await trialEmail())?.trim();
        if (!email) throw new Error("An email address is needed to start a trial.");
        const outcome = await requestTrial.call(client, email);
        const current = state(await client.status());
        const text = outcome.message !== undefined && outcome.message !== "none" ? options.messageText?.(outcome.message) : undefined;
        if (!outcome.accepted) throw new Error(text ?? "A trial could not be started.");
        return text ? { ...current, detail: text } : current;
      },
    } : {}),
    ...(options.purchaseUrl ? { purchaseUrl: options.purchaseUrl } : {}),
    ...(options.openExternal ? { openExternal: options.openExternal } : {}),
  };
}

/**
 * The app's `rightkit-oauth` commands as a typed port. Both calls go to the shared owner
 * (`OAuthClient::sign_in_with_browser` through the app's command): the default browser opens, no in-app webview, and no token
 * reaches the renderer. Results are untrusted `unknown`; only `signedIn` and `account` are read.
 */
export interface OAuthSignInPort {
  /** Account already signed in, if any. */
  status(): Promise<unknown>;
  /** Open the default browser for `provider`; resolves when the sign-in finishes. */
  signIn(provider: SignInProvider): Promise<unknown>;
}

function accountOf(value: unknown): { signedIn: boolean; account?: string } {
  if (!value || typeof value !== "object" || Array.isArray(value)) return { signedIn: false };
  const v = value as { signedIn?: unknown; account?: unknown };
  const account = typeof v.account === "string" && v.account.trim() ? v.account.trim().slice(0, 320) : undefined;
  return v.signedIn === true ? { signedIn: true, ...(account ? { account } : {}) } : { signedIn: false };
}

/** `SignInBackend` over `port`. A sign-in that resolves without `signedIn: true` (closed browser tab, cancel) rejects, so the step shows its retry message. */
export function signInBackendFromOAuth(port: OAuthSignInPort, providers: SignInBackend["providers"]): SignInBackend {
  return {
    providers,
    load: async () => accountOf(await port.status()),
    start: async (provider) => {
      const result = accountOf(await port.signIn(provider));
      if (!result.signedIn) throw new Error("Sign-in did not finish. Try again.");
      return result;
    },
  };
}
