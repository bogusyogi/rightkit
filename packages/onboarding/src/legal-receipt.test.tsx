// @vitest-environment jsdom
// PK10-2: native receipt creation (`LegalReceiptPort.create`), the legacy renderer record, reopen and reset.
import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Onboarding, isLegalAccepted } from "./Onboarding.js";
import { LEGAL_RECEIPT_COMMANDS, createNativeLegalReceiptPort, reopenLegalAcceptance, resetLegalAcceptance } from "./legal-receipt.js";
import type { LegalAcceptanceRecord, LegalReceiptCreateRequest, LegalReceiptPort, OnboardingProps, SettingsBackend } from "./types.js";

afterEach(() => cleanup());

const theme = { bg: "#fff", surface: "#fff", border: "#ddd", text: "#000", muted: "#666", accent: "#70c", accentContrast: "#fff", fontDisplay: "system-ui", fontBody: "system-ui" };
const legal = { appName: "AcmeRight", trustLine: "Trust.", documents: [{ title: "Terms", url: "https://example.test/terms", content: "SECRET BODY TEXT" }], agreementStatement: "I agree.", theme };
const KEY = "onboarding.state";
const step = { id: "a", label: "A", title: "A", render: () => <p>A body</p> };

function memorySettings(initial: Record<string, unknown> = {}) {
  const values: Record<string, unknown> = { ...initial };
  let failPatches = 0;
  const patch = vi.fn(async (next: Record<string, unknown>) => {
    if (failPatches > 0) { failPatches -= 1; throw new Error("settings are read-only"); }
    Object.assign(values, next);
    return { values: { ...values }, revision: 2 };
  });
  const backend: SettingsBackend = { load: async () => ({ values: { ...values }, revision: 1 }), patch };
  return { backend, values, patch, failNext: () => { failPatches = 1; } };
}

const NATIVE_TIME = "2026-10-20T08:30:00.000Z";
const rec = (version: string, documentsHash = "h", acceptedAt = "2026-01-01T00:00:00.000Z"): LegalAcceptanceRecord => ({ version, documentsHash, acceptedAt, eligibilityBasis: "individual" });

/** A native-style port: `create` stamps its own time and stores; there is no `write`. */
function nativePort(initial: LegalAcceptanceRecord | null = null, over: Partial<LegalReceiptPort> = {}) {
  let stored = initial;
  const create = vi.fn(async (request: LegalReceiptCreateRequest): Promise<LegalAcceptanceRecord> => {
    stored = { version: request.version, documentsHash: request.documentsHash, acceptedAt: NATIVE_TIME, eligibilityBasis: request.eligibilityBasis };
    return stored;
  });
  const reset = vi.fn(async () => { stored = null; });
  const port: LegalReceiptPort = { read: async () => stored, create, reset, ...over };
  return { port, create, reset, get stored() { return stored; } };
}

function setup(initial: Record<string, unknown>, props: Partial<OnboardingProps> = {}) {
  const s = memorySettings(initial);
  const onComplete = vi.fn();
  const view = render(<Onboarding settings={s.backend} legal={legal} onComplete={onComplete} {...props} />);
  return { ...s, onComplete, ...view };
}

async function agree(user: ReturnType<typeof userEvent.setup>, basis = 0) {
  await user.click((await screen.findAllByRole("radio"))[basis]!);
  await user.click(screen.getByRole("checkbox"));
  await user.click(screen.getByRole("button", { name: /agree/i }));
}

const savedState = (values: Record<string, unknown>) => values[KEY] as { legalAcceptance?: LegalAcceptanceRecord; legalAccepted: boolean; legalReopened?: boolean; completed: string[]; currentStep: number };

describe("LegalReceiptPort.create (native path)", () => {
  it("creates the receipt natively with the declaration, keeps the native time, and never calls write", async () => {
    const user = userEvent.setup();
    const write = vi.fn();
    const native = nativePort(null, { write });
    const documents = [{ id: "eula", role: "eula", version: "1.0", sha256: "a".repeat(64), treatment: "agree", material: true }];
    const { values } = setup({}, { steps: [step], requiredVersion: "v3", requiredDocumentsHash: "sha256:abc", receiptDocuments: documents, legalReceipt: native.port });
    await agree(user, 1);
    expect(await screen.findByText("A body")).toBeInTheDocument();
    expect(native.create).toHaveBeenCalledTimes(1);
    expect(native.create).toHaveBeenCalledWith({ version: "v3", documentsHash: "sha256:abc", documents, eligibilityBasis: "enterprise", surface: "first_run" });
    // The declaration carries no time at all: the native service owns it.
    expect(Object.keys(native.create.mock.calls[0]![0])).not.toContain("acceptedAt");
    expect(write).not.toHaveBeenCalled();
    expect(savedState(values).legalAcceptance).toEqual({ version: "v3", documentsHash: "sha256:abc", acceptedAt: NATIVE_TIME, eligibilityBasis: "enterprise" });
    expect(native.stored?.acceptedAt).toBe(NATIVE_TIME);
  });

  it("declares the gate's titles and URLs by default and never sends document bodies", async () => {
    const user = userEvent.setup();
    const native = nativePort();
    setup({}, { steps: [step], legalReceipt: native.port });
    await agree(user);
    await screen.findByText("A body");
    const request = native.create.mock.calls[0]![0];
    expect(request.documents).toEqual([{ title: "Terms", url: "https://example.test/terms" }]);
    expect(JSON.stringify(request)).not.toContain("SECRET BODY TEXT");
    expect(request.version).toBe("1");
    expect(request.documentsHash).toMatch(/^(sha256|fnv1a):/);
  });

  it("keeps the gate closed and shows an alert when native creation fails; a retry creates it", async () => {
    const user = userEvent.setup();
    const native = nativePort();
    native.create.mockRejectedValueOnce(new Error("The receipt could not be stored."));
    const { values } = setup({}, { steps: [step], legalReceipt: native.port });
    await agree(user);
    expect(await screen.findByRole("alert")).toHaveTextContent(/save your acceptance/i);
    expect(screen.queryByText("A body")).toBeNull();
    expect(values[KEY]).toBeUndefined();
    await user.click(screen.getByRole("button", { name: /agree/i }));
    expect(await screen.findByText("A body")).toBeInTheDocument();
    expect(native.create).toHaveBeenCalledTimes(2);
  });

  it("does not stamp a second receipt when only the settings save failed", async () => {
    const user = userEvent.setup();
    const native = nativePort();
    const { values, failNext } = setup({}, { steps: [step], legalReceipt: native.port });
    failNext();
    await agree(user);
    expect(await screen.findByRole("alert")).toBeInTheDocument();
    expect(screen.queryByText("A body")).toBeNull();
    await user.click(screen.getByRole("button", { name: /agree/i }));
    expect(await screen.findByText("A body")).toBeInTheDocument();
    expect(native.create).toHaveBeenCalledTimes(1);
    expect(savedState(values).legalAcceptance?.acceptedAt).toBe(NATIVE_TIME);
  });

  it("refuses a record that is malformed, for another version or for another digest, and the gate stays closed", async () => {
    const user = userEvent.setup();
    const cases: Array<[string, Partial<OnboardingProps>, () => unknown]> = [
      ["malformed", {}, () => ({ version: "1" })],
      ["version", { requiredVersion: "v2" }, () => rec("v1", "h")],
      ["digest", { requiredVersion: "v2", requiredDocumentsHash: "sha256:new" }, () => rec("v2", "sha256:old")],
      ["basis", {}, () => ({ ...rec("1"), eligibilityBasis: "enterprise" })],
    ];
    for (const [name, props, answer] of cases) {
      const native = nativePort(null, { create: vi.fn(async () => answer() as LegalAcceptanceRecord) });
      const { values } = setup({}, { steps: [step], legalReceipt: native.port, ...props });
      await agree(user, 0);
      expect(await screen.findByRole("alert"), name).toBeInTheDocument();
      expect(screen.queryByText("A body"), name).toBeNull();
      expect(values[KEY], name).toBeUndefined();
      cleanup();
    }
  });

  it("completes a gate-only setup from the native record", async () => {
    const user = userEvent.setup();
    const native = nativePort();
    const { onComplete } = setup({}, { legalReceipt: native.port });
    await agree(user);
    await waitFor(() => expect(onComplete).toHaveBeenCalledWith({ completed: ["legal"], skipped: [], steps: {} }));
  });

  it("re-acceptance asks the native service again and replaces the stored receipt", async () => {
    const user = userEvent.setup();
    const native = nativePort(rec("v1", "sha256:old"));
    const { values } = setup({}, { steps: [step], requiredVersion: "v1", requiredDocumentsHash: "sha256:new", legalReceipt: native.port });
    expect((await screen.findByTestId("onboarding-legal")).dataset.variant).toBe("re-acceptance");
    await agree(user);
    expect(await screen.findByText("A body")).toBeInTheDocument();
    expect(native.create).toHaveBeenCalledTimes(1);
    expect(native.stored).toMatchObject({ version: "v1", documentsHash: "sha256:new", acceptedAt: NATIVE_TIME });
    expect(savedState(values).legalAcceptance?.documentsHash).toBe("sha256:new");
  });

  it("a legacy port with write still gets the renderer-built record, and a port with neither create nor write cannot accept", async () => {
    const user = userEvent.setup();
    const write = vi.fn(async () => {});
    setup({}, { steps: [step], legalReceipt: { read: async () => null, write } });
    await agree(user);
    expect(await screen.findByText("A body")).toBeInTheDocument();
    expect(write).toHaveBeenCalledTimes(1);
    cleanup();
    const { values } = setup({}, { steps: [step], legalReceipt: { read: async () => null } });
    await agree(user);
    expect(await screen.findByRole("alert")).toBeInTheDocument();
    expect(screen.queryByText("A body")).toBeNull();
    expect(values[KEY]).toBeUndefined();
  });
});

describe("a bare legacy flag never passes", () => {
  it("shows the re-acceptance gate once for a stored legalAccepted without a record, with or without a native port", async () => {
    const legacy = { [KEY]: { version: 1, currentStep: 1, completed: ["legal"], skipped: false, legalAccepted: true } };
    setup(legacy, { steps: [step] });
    expect(await screen.findByTestId("onboarding-legal")).toHaveAttribute("data-variant", "re-acceptance");
    cleanup();
    setup(legacy, { steps: [step], legalReceipt: nativePort(null).port });
    expect(await screen.findByTestId("onboarding-legal")).toBeInTheDocument();
    expect(screen.queryByText("A body")).toBeNull();
  });
});

describe("reopen and reset over the receipt", () => {
  const accepted = (extra: Record<string, unknown> = {}) => ({ [KEY]: { version: 1, currentStep: 2, completed: ["legal", "a"], skipped: false, legalAccepted: true, legalAcceptance: rec("v1", "h"), ...extra } });

  it("reopen makes the gate show as a re-acceptance, keeps progress and the old record, and agreeing clears the flag", async () => {
    const user = userEvent.setup();
    const s = memorySettings(accepted());
    await reopenLegalAcceptance({ settings: s.backend });
    expect(s.patch).toHaveBeenCalledWith({ [KEY]: expect.objectContaining({ legalReopened: true, completed: ["legal", "a"], currentStep: 2 }) }, 1);
    const native = nativePort(rec("v1", "h"));
    render(<Onboarding settings={s.backend} legal={legal} steps={[step, { ...step, id: "b", label: "B", render: () => <p>B body</p> }]} requiredVersion="v1" legalReceipt={native.port} onComplete={vi.fn()} />);
    expect((await screen.findByTestId("onboarding-legal")).dataset.variant).toBe("re-acceptance");
    expect(native.stored).toEqual(rec("v1", "h"));
    await agree(user);
    expect(await screen.findByText("B body")).toBeInTheDocument();
    expect(savedState(s.values)).toMatchObject({ currentStep: 2, completed: ["legal", "a"] });
    expect(savedState(s.values).legalReopened).toBeFalsy();
    expect(native.stored?.acceptedAt).toBe(NATIVE_TIME);
  });

  it("a reopened agreement is not accepted even under a custom isAccepted", () => {
    const state = { version: 1 as const, currentStep: 1, completed: [], skipped: false, legalAccepted: true, legalAcceptance: rec("v1"), legalReopened: true };
    expect(isLegalAccepted(state, "v1", () => true)).toBe(false);
    expect(isLegalAccepted({ ...state, legalReopened: undefined }, "v1")).toBe(true);
  });

  it("reset clears the receipt first, then the settings copy, and the next mount shows the first-run gate", async () => {
    const s = memorySettings(accepted());
    const native = nativePort(rec("v1", "h"));
    const order: string[] = [];
    native.reset.mockImplementation(async () => { order.push("receipt"); });
    s.patch.mockImplementationOnce(async (next) => { order.push("settings"); Object.assign(s.values, next); return { values: { ...s.values }, revision: 2 }; });
    await resetLegalAcceptance({ settings: s.backend, legalReceipt: native.port });
    expect(order).toEqual(["receipt", "settings"]);
    const state = savedState(s.values) as unknown as Record<string, unknown>;
    expect(state.legalAccepted).toBe(false);
    expect(state.legalAcceptance).toBeUndefined();
    expect(state.completed).toEqual(["a"]);
    expect(state.currentStep).toBe(2);
    // The receipt double above is a spy, so clear it as the native side would.
    const cleared = nativePort(null);
    render(<Onboarding settings={s.backend} legal={legal} steps={[step]} legalReceipt={cleared.port} onComplete={vi.fn()} />);
    expect((await screen.findByTestId("onboarding-legal")).dataset.variant).toBe("first-run");
  });

  it("reset fails before touching settings when the receipt cannot be reset, so a receipt that stays accepted is never hidden", async () => {
    const s = memorySettings(accepted());
    await expect(resetLegalAcceptance({ settings: s.backend, legalReceipt: { read: async () => rec("v1") } })).rejects.toThrow(/cannot be reset/);
    expect(s.patch).not.toHaveBeenCalled();
    const failing = nativePort(rec("v1"), { reset: async () => { throw new Error("locked"); } });
    await expect(resetLegalAcceptance({ settings: s.backend, legalReceipt: failing.port })).rejects.toThrow("locked");
    expect(s.patch).not.toHaveBeenCalled();
  });

  it("reset without a receipt port only clears the settings copy", async () => {
    const s = memorySettings(accepted());
    await resetLegalAcceptance({ settings: s.backend });
    expect(savedState(s.values).legalAccepted).toBe(false);
    expect(savedState(s.values).legalAcceptance).toBeUndefined();
  });
});

describe("createNativeLegalReceiptPort", () => {
  const request: LegalReceiptCreateRequest = { version: "v1", documentsHash: "sha256:x", documents: [{ id: "eula", sha256: "b".repeat(64) }], eligibilityBasis: "individual", surface: "first_run" };

  it("calls the registered commands with only the declared fields", async () => {
    const invoke = vi.fn(async (command: string) => (command === LEGAL_RECEIPT_COMMANDS.create ? rec("v1", "sha256:x", NATIVE_TIME) : null));
    const port = createNativeLegalReceiptPort({ invoke });
    expect(await port.read()).toBeNull();
    expect(invoke).toHaveBeenLastCalledWith("rightkit_legal_receipt_read", undefined);
    const created = await port.create!({ ...request, acceptedAt: "2000-01-01T00:00:00.000Z" } as LegalReceiptCreateRequest);
    expect(created.acceptedAt).toBe(NATIVE_TIME);
    expect(invoke).toHaveBeenLastCalledWith("rightkit_legal_receipt_create", { request });
    await port.reset!();
    expect(invoke).toHaveBeenLastCalledWith("rightkit_legal_receipt_reset", undefined);
    expect(port.write).toBeUndefined();
  });

  it("reads an unreadable stored value as no receipt, rejects a malformed created record, and turns host errors into Errors", async () => {
    const stored: unknown[] = [{ version: "1" }, rec("v1")];
    const port = createNativeLegalReceiptPort({ invoke: async (command) => (command === LEGAL_RECEIPT_COMMANDS.read ? stored.shift() : { acceptedAt: "now" }) });
    expect(await port.read()).toBeNull();
    expect(await port.read()).toEqual(rec("v1"));
    await expect(port.create!(request)).rejects.toThrow(/invalid record/);
    const refusing = createNativeLegalReceiptPort({ invoke: async () => { throw { code: "permission_denied", message: "forbidden: content webview" }; } });
    await expect(refusing.create!(request)).rejects.toThrow("forbidden: content webview");
    const opaque = createNativeLegalReceiptPort({ invoke: async () => { throw "boom"; } });
    await expect(opaque.read()).rejects.toThrow(/legal receipt service failed|boom/);
  });

  it("uses the command names the host registered under", async () => {
    const invoke = vi.fn(async () => null);
    await createNativeLegalReceiptPort({ invoke, commands: { read: "app_legal_read" } }).read();
    expect(invoke).toHaveBeenCalledWith("app_legal_read", undefined);
  });
});
