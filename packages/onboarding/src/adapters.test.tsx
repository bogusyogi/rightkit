// @vitest-environment jsdom
// PK10-3: `LicenseBackend` over a native `LicenseView`, and the sign-in step wired to the app's rightkit-oauth commands.
import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Onboarding } from "./Onboarding.js";
import { licenseBackendFromView, licenseStateFromView, signInBackendFromOAuth, type LicenseViewClient, type LicenseViewLike } from "./adapters.js";
import { licenseStep } from "./steps.js";
import type { OnboardingStepContext, SettingsBackend } from "./types.js";

afterEach(() => cleanup());

const messages: Record<string, string> = { none: "", trialRequested: "Check your email to start the trial.", trialRefused: "A trial could not be started for this email.", keyNotAccepted: "That licence key was not accepted.", networkUnavailable: "Could not reach the licence server." };
const messageText = (code: string) => messages[code] ?? "";
const view = (extra: Partial<LicenseViewLike> = {}): LicenseViewLike => ({ status: "none", lifecycle: "unlicensed", tier: null, validUntil: null, message: "none", ...extra });

describe("licenseStateFromView", () => {
  it("maps every lifecycle to a step status, the lifecycle winning over the coarse status", () => {
    const statusOf = (lifecycle: string, status = "active") => licenseStateFromView(view({ lifecycle, status })).status;
    expect(statusOf("unlicensed", "none")).toBe("none");
    expect(statusOf("trial")).toBe("trial");
    expect(statusOf("expired_trial", "expired")).toBe("expired");
    for (const lifecycle of ["active", "grace", "lifetime", "lifetime_provisional", "verification_required"]) expect(statusOf(lifecycle)).toBe("active");
    expect(statusOf("expired", "expired")).toBe("expired");
    for (const lifecycle of ["migration_required", "clock_repair_required", "revoked", "invalid"]) expect(statusOf(lifecycle, "invalid")).toBe("invalid");
  });

  it("falls back to the coarse status, and treats anything unknown as invalid rather than active", () => {
    expect(licenseStateFromView({ status: "grace" }).status).toBe("active");
    expect(licenseStateFromView({ status: "revoked" }).status).toBe("invalid");
    expect(licenseStateFromView({ status: "unverifiable" }).status).toBe("invalid");
    expect(licenseStateFromView({ status: "surprise", lifecycle: "surprise" }).status).toBe("invalid");
  });

  it("carries tier, ISO validity, mapped detail text and overridable labels; no detail for the none message", () => {
    const state = licenseStateFromView(view({ status: "active", lifecycle: "active", tier: "pro", validUntil: Date.UTC(2027, 0, 2, 3, 4, 5), message: "trialRequested" }), { messageText, labels: { active: "Pro licence" } });
    expect(state).toEqual({ status: "active", label: "Pro licence", detail: "Check your email to start the trial.", tier: "pro", validUntil: "2027-01-02T03:04:05.000Z" });
    const none = licenseStateFromView(view(), { messageText });
    expect(none).toEqual({ status: "none", label: "No licence", validUntil: null });
    expect(licenseStateFromView(view({ validUntil: Number.NaN })).validUntil).toBeNull();
  });
});

describe("licenseBackendFromView", () => {
  function client(over: Partial<LicenseViewClient> = {}) {
    const status = vi.fn(async () => view());
    const activate = vi.fn(async (_key: string) => view({ status: "active", lifecycle: "active", tier: "plus" }));
    const requestTrial = vi.fn(async (_email: string) => ({ accepted: true, message: "trialRequested" }));
    return { status, activate, requestTrial, client: { status, activate, requestTrial, ...over } as LicenseViewClient };
  }

  it("loads and activates through the native client and returns mapped states", async () => {
    const c = client();
    const backend = licenseBackendFromView(c.client, { messageText });
    expect((await backend.load()).status).toBe("none");
    const activated = await backend.activate!("KEY-1");
    expect(c.activate).toHaveBeenCalledWith("KEY-1");
    expect(activated).toMatchObject({ status: "active", tier: "plus" });
  });

  it("lets a native failure reach the step unchanged", async () => {
    const c = client({ activate: vi.fn(async () => { throw new Error("That licence key was not accepted."); }) });
    await expect(licenseBackendFromView(c.client).activate!("bad")).rejects.toThrow("That licence key was not accepted.");
  });

  it("offers a trial only when the client can request one and the app supplies the email", async () => {
    const c = client();
    expect(licenseBackendFromView(c.client).startTrial).toBeUndefined();
    expect(licenseBackendFromView({ status: c.status, activate: c.activate }, { trialEmail: () => "a@example.com" }).startTrial).toBeUndefined();
    const backend = licenseBackendFromView(c.client, { messageText, trialEmail: async () => "  a@example.com " });
    const state = await backend.startTrial!();
    expect(c.requestTrial).toHaveBeenCalledWith("a@example.com");
    expect(state.status).toBe("none");
    expect(state.detail).toBe("Check your email to start the trial.");
  });

  it("refuses the trial with the native message, or without an email, and never calls requestTrial then", async () => {
    const refused = client({ requestTrial: vi.fn(async () => ({ accepted: false, message: "trialRefused" })) });
    await expect(licenseBackendFromView(refused.client, { messageText, trialEmail: () => "a@example.com" }).startTrial!()).rejects.toThrow("A trial could not be started for this email.");
    const none = client();
    await expect(licenseBackendFromView(none.client, { trialEmail: () => " " }).startTrial!()).rejects.toThrow(/email address/);
    expect(none.requestTrial).not.toHaveBeenCalled();
  });

  it("passes the plans link through and renders in the licence step", async () => {
    const user = userEvent.setup();
    const openExternal = vi.fn();
    const c = client({ status: vi.fn(async () => view({ status: "active", lifecycle: "trial", tier: "pro", message: "trialRequested" })) });
    const backend = licenseBackendFromView(c.client, { messageText, purchaseUrl: "https://example.test/plans", openExternal, trialEmail: () => "a@example.com" });
    render(<>{licenseStep(backend).render({} as OnboardingStepContext)}</>);
    expect(await screen.findByText("Trial")).toBeInTheDocument();
    expect(screen.getByText("Check your email to start the trial.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "View plans" }));
    expect(openExternal).toHaveBeenCalledWith("https://example.test/plans");
    await user.click(screen.getByRole("button", { name: "Start trial" }));
    await waitFor(() => expect(c.requestTrial).toHaveBeenCalledWith("a@example.com"));
  });
});

describe("signInBackendFromOAuth", () => {
  const providers = [{ id: "google" as const }, { id: "apple" as const }];

  it("reads the account from the app's status command and keeps only signedIn and account", async () => {
    const port = { status: vi.fn(async () => ({ signedIn: true, account: " a@example.com ", accessToken: "SECRET" })), signIn: vi.fn() };
    const backend = signInBackendFromOAuth(port, providers);
    expect(backend.providers).toBe(providers);
    expect(await backend.load()).toEqual({ signedIn: true, account: "a@example.com" });
    expect(await signInBackendFromOAuth({ status: async () => "garbage", signIn: vi.fn() }, providers).load()).toEqual({ signedIn: false });
    expect(await signInBackendFromOAuth({ status: async () => ({ signedIn: "yes" }), signIn: vi.fn() }, providers).load()).toEqual({ signedIn: false });
  });

  it("starts the shared owner for the chosen provider and rejects a sign-in that did not finish", async () => {
    const signIn = vi.fn(async (_provider: string): Promise<unknown> => ({ signedIn: true, account: "a@example.com", refreshToken: "SECRET" }));
    const backend = signInBackendFromOAuth({ status: async () => ({ signedIn: false }), signIn }, providers);
    expect(await backend.start("apple")).toEqual({ signedIn: true, account: "a@example.com" });
    expect(signIn).toHaveBeenCalledWith("apple");
    signIn.mockResolvedValueOnce({ signedIn: false });
    await expect(backend.start("google")).rejects.toThrow(/did not finish/);
    signIn.mockRejectedValueOnce(new Error("The browser was closed."));
    await expect(backend.start("google")).rejects.toThrow("The browser was closed.");
  });
});

describe("Onboarding signIn prop", () => {
  const legal = { appName: "AcmeRight", trustLine: "Trust.", documents: [{ title: "Terms", url: "https://example.test/terms" }], agreementStatement: "I agree.", theme: { bg: "#fff", surface: "#fff", border: "#ddd", text: "#000", muted: "#666", accent: "#70c", accentContrast: "#fff", fontDisplay: "system-ui", fontBody: "system-ui" } };
  const record = { version: "1", documentsHash: "h", acceptedAt: "2026-01-01T00:00:00.000Z", eligibilityBasis: "individual" };
  const settings = (step: number): SettingsBackend => {
    const values: Record<string, unknown> = { "onboarding.state": { version: 1, currentStep: step, completed: ["legal"], skipped: false, legalAccepted: true, legalAcceptance: record } };
    return { load: async () => ({ values: { ...values }, revision: 1 }), patch: async (patch) => { Object.assign(values, patch); return { values: { ...values }, revision: 2 }; } };
  };

  it("adds the sign-in step after the licence step, signs in, and exposes the backend as ctx.signIn to every step", async () => {
    const user = userEvent.setup();
    const signIn = signInBackendFromOAuth({ status: async () => ({ signedIn: false }), signIn: vi.fn(async () => ({ signedIn: true, account: "a@example.com" })) }, [{ id: "google" }]);
    const seen: Array<unknown> = [];
    const license = licenseBackendFromView({ status: async () => view(), activate: async () => view() });
    render(<Onboarding settings={settings(2)} legal={legal} license={license} signIn={signIn} steps={[{ id: "after", label: "After", title: "After", render: (ctx) => { seen.push(ctx.signIn); return <p>After body</p>; } }]} onComplete={vi.fn()} />);
    // Agreement, Licence, Sign in, After: the saved step 2 is the sign-in step.
    expect(await screen.findByRole("heading", { name: "Sign in" })).toBeInTheDocument();
    expect(screen.getByText(/Opens your browser/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Sign in with Google" }));
    expect(await screen.findByText("a@example.com")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Continue" }));
    expect(await screen.findByText("After body")).toBeInTheDocument();
    expect(seen.at(-1)).toBe(signIn);
  });

  it("shows no sign-in step without the prop, and ctx.signIn is undefined", async () => {
    const seen: Array<unknown> = [];
    render(<Onboarding settings={settings(1)} legal={legal} steps={[{ id: "a", label: "A", title: "A", render: (ctx) => { seen.push(ctx.signIn); return <p>A body</p>; } }]} onComplete={vi.fn()} />);
    expect(await screen.findByText("A body")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Sign in/ })).toBeNull();
    expect(seen.at(-1)).toBeUndefined();
  });
});
