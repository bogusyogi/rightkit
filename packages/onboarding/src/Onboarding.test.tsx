// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Onboarding, isLegalAccepted } from "./Onboarding.js";
import { signInStep } from "./steps.js";
import type { LegalAcceptanceRecord, LegalReceiptCreateRequest, LegalReceiptPort, ModelEntry, ModelsBackend, OnboardingProps, SettingsBackend } from "./types.js";

afterEach(() => cleanup());

const theme = { bg: "#fff", surface: "#fff", border: "#ddd", text: "#000", muted: "#666", accent: "#70c", accentContrast: "#fff", fontDisplay: "system-ui", fontBody: "system-ui" };
const legal = { appName: "AcmeRight", trustLine: "Trust.", documents: [{ title: "Terms", url: "https://example.test/terms" }], agreementStatement: "I agree.", theme };

function memorySettings(initial: Record<string, unknown> = {}) {
  const values: Record<string, unknown> = { ...initial };
  const backend: SettingsBackend = {
    load: async () => ({ values: { ...values }, revision: 1 }),
    patch: async (patch) => { Object.assign(values, patch); return { values: { ...values }, revision: 2 }; },
  };
  return { backend, values };
}

const record = (version: string) => ({ version, documentsHash: "h", acceptedAt: "2026-01-01T00:00:00.000Z", eligibilityBasis: "individual" });
const accepted = (extra: Record<string, unknown> = {}) => ({ "onboarding.state": { version: 1, currentStep: 1, completed: ["legal"], skipped: false, legalAccepted: true, legalAcceptance: record("1"), ...extra } });

function models(entries: ModelEntry[], extra: Partial<ModelsBackend> = {}): ModelsBackend {
  return { list: async () => entries, install: async () => {}, ...extra };
}

function setup(settingsValues: Record<string, unknown>, props: Partial<OnboardingProps> = {}) {
  const s = memorySettings(settingsValues);
  const onComplete = vi.fn();
  const view = render(<Onboarding settings={s.backend} legal={legal} onComplete={onComplete} {...props} />);
  return { ...s, onComplete, ...view };
}

async function agree(user: ReturnType<typeof userEvent.setup>) {
  await user.click((await screen.findAllByRole("radio"))[0]!);
  await user.click(screen.getByRole("checkbox"));
  await user.click(screen.getByRole("button", { name: /agree/i }));
}

describe("Back after acceptance", () => {
  it("keeps window layout by default and propagates contained layout through the legal gate", async () => {
    const settings = memorySettings();
    const { container, rerender } = render(<Onboarding settings={settings.backend} legal={legal} onComplete={vi.fn()} />);
    const gate = await screen.findByTestId("onboarding-legal");
    expect(gate).not.toHaveClass("rk-onboarding--contained");
    expect((container.querySelector(".rk-legal-gate") as HTMLElement).style.minHeight).toBe("100vh");

    rerender(<Onboarding settings={settings.backend} legal={legal} layout="contained" onComplete={vi.fn()} />);
    expect(await screen.findByTestId("onboarding-legal")).toHaveClass("rk-onboarding--contained");
    expect((container.querySelector(".rk-legal-gate") as HTMLElement).style.minHeight).toBe("");
  });

  it("is disabled on the first step after the agreement and never renders a blank Agreement step", async () => {
    setup(accepted(), { steps: [{ id: "a", label: "A", title: "Step A", render: () => <p>A body</p> }, { id: "b", label: "B", title: "Step B", render: () => <p>B body</p> }] });
    await screen.findByText("A body");
    expect(screen.getByRole("button", { name: "Back" })).toBeDisabled();
    expect(screen.getByRole("button", { name: /Agreement/ })).toBeDisabled();
  });
  it("clamps a saved step index of 0 to the first app step", async () => {
    setup(accepted({ currentStep: 0 }), { steps: [{ id: "a", label: "A", title: "Step A", render: () => <p>A body</p> }] });
    expect(await screen.findByText("A body")).toBeInTheDocument();
  });
});

describe("step readiness", () => {
  it("disables Continue and Finish until canContinue is satisfied", async () => {
    const user = userEvent.setup();
    let ok = false;
    const { rerender, backend, onComplete } = (() => {
      const s = memorySettings(accepted());
      const step = { id: "a", label: "A", title: "Step A", canContinue: () => ok, blockedHint: "Not yet", render: () => <p>A</p> };
      const r = render(<Onboarding settings={s.backend} legal={legal} steps={[step]} onComplete={vi.fn()} />);
      return { ...r, backend: s.backend, onComplete: vi.fn() };
    })();
    void backend; void onComplete;
    const finish = await screen.findByRole("button", { name: "Finish setup" });
    expect(finish).toBeDisabled();
    expect(screen.getByText("Not yet")).toBeInTheDocument();
    await user.click(finish);
    ok = true;
    rerender(<Onboarding settings={backend} legal={legal} steps={[{ id: "a", label: "A", title: "Step A", canContinue: () => ok, render: () => <p>A</p> }]} onComplete={vi.fn()} />);
    await waitFor(() => expect(screen.getByRole("button", { name: "Finish setup" })).toBeEnabled());
  });

  it("requires a downloaded model on the models step when requireModel is set", async () => {
    const user = userEvent.setup();
    const { onComplete } = setup(accepted(), { models: models([{ id: "m", label: "Model", state: "missing" }]), requireModel: true });
    const finish = await screen.findByRole("button", { name: "Finish setup" });
    expect(finish).toBeDisabled();
    await user.click(finish);
    expect(onComplete).not.toHaveBeenCalled();
  });

  it("enables Finish once a model is installed", async () => {
    const user = userEvent.setup();
    const { onComplete } = setup(accepted(), { models: models([{ id: "m", label: "Model", state: "installed" }]), requireModel: true });
    const finish = await screen.findByRole("button", { name: "Finish setup" });
    await waitFor(() => expect(finish).toBeEnabled());
    await user.click(finish);
    await waitFor(() => expect(onComplete).toHaveBeenCalled());
  });

  it("does not gate the models step by default", async () => {
    setup(accepted(), { models: models([{ id: "m", label: "Model", state: "missing" }]) });
    expect(await screen.findByRole("button", { name: "Finish setup" })).toBeEnabled();
  });
});

describe("models cancel", () => {
  const downloading: ModelEntry[] = [{ id: "m", label: "Model", state: "downloading", progress: 0.3 }];
  it("hides Pause when the backend has no cancel", async () => {
    setup(accepted(), { models: models(downloading) });
    await screen.findByText("Model");
    expect(screen.queryByRole("button", { name: "Pause" })).toBeNull();
  });
  it("shows Pause and calls cancel when supplied", async () => {
    const user = userEvent.setup();
    const cancel = vi.fn(async () => {});
    setup(accepted(), { models: models(downloading, { cancel }) });
    await user.click(await screen.findByRole("button", { name: "Pause" }));
    expect(cancel).toHaveBeenCalledWith("m");
  });
});

describe("models seams", () => {
  it("auto-starts the first missing download and renders intro copy", async () => {
    const install = vi.fn(async () => {});
    setup(accepted(), { models: models([{ id: "m", label: "Model", state: "missing" }], { install }), autoStartDownload: true, modelsIntro: <p>Custom intro</p> });
    expect(await screen.findByText("Custom intro")).toBeInTheDocument();
    await waitFor(() => expect(install).toHaveBeenCalledTimes(1));
    expect(install).toHaveBeenCalledWith("m");
  });
  it("does not auto-start by default", async () => {
    const install = vi.fn(async () => {});
    setup(accepted(), { models: models([{ id: "m", label: "Model", state: "missing" }], { install }) });
    await screen.findByText("Model");
    expect(install).not.toHaveBeenCalled();
  });
});

describe("models auto-start hooks", () => {
  const entries: ModelEntry[] = [{ id: "small", label: "Small", state: "missing" }, { id: "big", label: "Big", state: "missing" }, { id: "have", label: "Have", state: "installed" }];
  it("default autoStartDownload is unchanged: starts nothing when a model is installed", async () => {
    const install = vi.fn(async () => {});
    setup(accepted(), { models: models(entries, { install }), autoStartDownload: true });
    await screen.findByText("Have");
    await new Promise((r) => setTimeout(r, 30));
    expect(install).not.toHaveBeenCalled();
  });
  it("shouldAutoStart picks the model the app chooses, without the installed guard, and only that one", async () => {
    const install = vi.fn(async () => {});
    setup(accepted(), { models: models(entries, { install }), shouldAutoStart: (e) => e.id === "big" });
    await waitFor(() => expect(install).toHaveBeenCalledTimes(1));
    expect(install).toHaveBeenCalledWith("big");
  });
  it("shouldAutoStart starts nothing when no entry matches", async () => {
    const install = vi.fn(async () => {});
    setup(accepted(), { models: models(entries, { install }), shouldAutoStart: () => false });
    await screen.findByText("Small");
    await new Promise((r) => setTimeout(r, 30));
    expect(install).not.toHaveBeenCalled();
  });
  it("onModelsStepEnter receives the loaded list, starts through ctx.start, and replaces the built-in rules", async () => {
    const install = vi.fn(async () => {});
    const enter = vi.fn(async (ctx: { models: readonly ModelEntry[]; start(id: string): Promise<void> }) => { await ctx.start(ctx.models.find((m) => m.id === "small")!.id); });
    setup(accepted(), { models: models(entries, { install }), autoStartDownload: true, shouldAutoStart: () => true, onModelsStepEnter: enter });
    await waitFor(() => expect(install).toHaveBeenCalledTimes(1));
    expect(install).toHaveBeenCalledWith("small");
    expect(enter).toHaveBeenCalledTimes(1);
    expect(enter.mock.calls[0]![0].models.map((m) => m.id)).toEqual(["small", "big", "have"]);
  });
  it("shows an error thrown by onModelsStepEnter", async () => {
    setup(accepted(), { models: models(entries), onModelsStepEnter: () => { throw new Error("no gpu"); } });
    expect(await screen.findByText("no gpu")).toBeInTheDocument();
  });
  it("does not start twice across re-renders or Back/Next", async () => {
    const user = userEvent.setup();
    const install = vi.fn(async () => {});
    const list = [{ id: "m", label: "Model", state: "missing" as const }];
    const props = { models: models(list, { install }), shouldAutoStart: () => true, steps: [{ id: "x", label: "X", title: "Step X", render: () => <p>X body</p> }] };
    const s = memorySettings(accepted());
    const ui = (extra: Partial<OnboardingProps> = {}) => <Onboarding settings={s.backend} legal={legal} onComplete={() => {}} {...props} {...extra} />;
    const view = render(ui());
    await waitFor(() => expect(install).toHaveBeenCalledTimes(1));
    view.rerender(ui({ steps: [{ id: "x", label: "X", title: "Step X", render: () => <p>X body</p> }] }));
    await user.click(await screen.findByRole("button", { name: "Continue" }));
    await screen.findByText("X body");
    await user.click(screen.getByRole("button", { name: "Back" }));
    await screen.findByText("Model");
    await new Promise((r) => setTimeout(r, 30));
    expect(install).toHaveBeenCalledTimes(1);
  });
  it("onModelsStepEnter also fires once across Back/Next", async () => {
    const user = userEvent.setup();
    const enter = vi.fn();
    setup(accepted(), { models: models([{ id: "m", label: "Model", state: "missing" }]), onModelsStepEnter: enter, steps: [{ id: "x", label: "X", title: "Step X", render: () => <p>X body</p> }] });
    await waitFor(() => expect(enter).toHaveBeenCalledTimes(1));
    await user.click(await screen.findByRole("button", { name: "Continue" }));
    await screen.findByText("X body");
    await user.click(screen.getByRole("button", { name: "Back" }));
    await screen.findByText("Model");
    await new Promise((r) => setTimeout(r, 30));
    expect(enter).toHaveBeenCalledTimes(1);
  });
});

describe("legal acceptance", () => {
  it("stores a full acceptance record with UTC timestamp", async () => {
    const user = userEvent.setup();
    const { values } = setup({}, { requiredVersion: "2026-10", steps: [{ id: "a", label: "A", title: "A", render: () => <p>A body</p> }] });
    await agree(user);
    await screen.findByText("A body");
    const stored = values["onboarding.state"] as { legalAcceptance: Record<string, unknown>; legalAccepted: boolean };
    expect(stored.legalAccepted).toBe(true);
    expect(Object.keys(stored.legalAcceptance).sort()).toEqual(["acceptedAt", "documentsHash", "eligibilityBasis", "version"]);
    expect(stored.legalAcceptance.version).toBe("2026-10");
    expect(stored.legalAcceptance.eligibilityBasis).toBe("individual");
    expect(String(stored.legalAcceptance.documentsHash)).toMatch(/^(sha256|fnv1a):/);
    expect(String(stored.legalAcceptance.acceptedAt)).toMatch(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/);
  });
  it("uses first-run on a fresh install", async () => {
    setup({}, {});
    expect((await screen.findByTestId("onboarding-legal")).dataset.variant).toBe("first-run");
  });
  it("re-prompts with the re-acceptance variant when the required version changes", async () => {
    setup(accepted({ legalAcceptance: record("1") }), { requiredVersion: "2" });
    expect((await screen.findByTestId("onboarding-legal")).dataset.variant).toBe("re-acceptance");
  });
  it("re-prompts when the persisted acceptance timestamp is not canonical UTC", async () => {
    setup(accepted({ legalAcceptance: { ...record("2"), acceptedAt: "2026-01-01" } }), { requiredVersion: "2" });
    expect((await screen.findByTestId("onboarding-legal")).dataset.variant).toBe("first-run");
  });
  it("gates persisted acceptance with an invalid receipt even without version or hash requirements", async () => {
    setup(accepted({ legalAcceptance: { ...record("2"), acceptedAt: "invalid" } }));
    expect(await screen.findByTestId("onboarding-legal")).toBeInTheDocument();
  });
  it("does not re-prompt when the stored version matches", async () => {
    setup(accepted({ legalAcceptance: record("2") }), { requiredVersion: "2", steps: [{ id: "a", label: "A", title: "A", render: () => <p>A body</p> }] });
    expect(await screen.findByText("A body")).toBeInTheDocument();
  });
  it("treats legacy boolean acceptance as needing re-acceptance only when requiredVersion is set", async () => {
    setup(accepted(), { requiredVersion: "2" });
    expect((await screen.findByTestId("onboarding-legal")).dataset.variant).toBe("re-acceptance");
    cleanup();
    setup(accepted(), { steps: [{ id: "a", label: "A", title: "A", render: () => <p>A body</p> }] });
    expect(await screen.findByText("A body")).toBeInTheDocument();
  });
  it("honours a custom isAccepted and keeps progress after re-acceptance", async () => {
    const user = userEvent.setup();
    const { values } = setup(accepted({ currentStep: 2, legalAcceptance: record("1") }), { isAccepted: (s) => s.legalAcceptance?.version === "9", requiredVersion: "9" });
    await agree(user);
    await waitFor(() => expect((values["onboarding.state"] as { legalAcceptance: { version: string } }).legalAcceptance.version).toBe("9"));
    expect((values["onboarding.state"] as { currentStep: number }).currentStep).toBe(2);
  });
  it("runs beforeLegalGate before showing the gate", async () => {
    const order: string[] = [];
    let release!: () => void;
    const before = vi.fn(() => new Promise<void>((r) => { order.push("before"); release = r; }));
    setup({}, { beforeLegalGate: before });
    await waitFor(() => expect(before).toHaveBeenCalled());
    expect(screen.queryByTestId("onboarding-legal")).toBeNull();
    release();
    expect(await screen.findByTestId("onboarding-legal")).toBeInTheDocument();
  });
});

describe("other seams", () => {
  const step = { id: "a", label: "A", title: "A", render: () => <p>A body</p> };
  it("shows Set up later by default and hides it with allowSkip=false", async () => {
    setup(accepted(), { steps: [step] });
    expect(await screen.findByRole("button", { name: "Set up later" })).toBeInTheDocument();
    cleanup();
    setup(accepted(), { steps: [step], allowSkip: false });
    await screen.findByText("A body");
    expect(screen.queryByRole("button", { name: "Set up later" })).toBeNull();
  });
  it("renders a titlebar slot and opt-in drag region", async () => {
    setup(accepted(), { steps: [step], titlebar: <span>Title</span>, dragRegion: true });
    const bar = await screen.findByTestId("onboarding-titlebar");
    expect(bar).toHaveAttribute("data-tauri-drag-region");
    expect(bar).toHaveTextContent("Title");
    cleanup();
    setup(accepted(), { steps: [step] });
    await screen.findByText("A body");
    expect(screen.queryByTestId("onboarding-titlebar")).toBeNull();
  });
});

describe("legal stage drag region", () => {
  const step = { id: "a", label: "A", title: "A", render: () => <p>A body</p> };
  it("is not rendered on the legal gate unless legalDragRegion is set", async () => {
    setup({}, { steps: [step], dragRegion: true });
    await screen.findByTestId("onboarding-legal");
    expect(screen.queryByTestId("onboarding-titlebar")).toBeNull();
    cleanup();
    setup({}, { steps: [step], dragRegion: true, legalDragRegion: true });
    await screen.findByTestId("onboarding-legal");
    expect(await screen.findByTestId("onboarding-titlebar")).toHaveAttribute("data-tauri-drag-region");
    cleanup();
    setup({}, { steps: [step], legalDragRegion: true });
    await screen.findByTestId("onboarding-legal");
    expect(screen.getByTestId("onboarding-titlebar")).toHaveAttribute("data-tauri-drag-region");
  });
  it("keeps a titlebar slot on the legal stage without a drag attribute", async () => {
    setup({}, { steps: [step], titlebar: <span>Title</span>, dragRegion: true });
    await screen.findByTestId("onboarding-legal");
    const bar = screen.getByTestId("onboarding-titlebar");
    expect(bar).toHaveTextContent("Title");
    expect(bar).not.toHaveAttribute("data-tauri-drag-region");
  });
  it("still renders the drag region on the setup steps", async () => {
    setup(accepted(), { steps: [step], dragRegion: true });
    expect(await screen.findByTestId("onboarding-titlebar")).toHaveAttribute("data-tauri-drag-region");
  });
});

describe("legalReceipt seam", () => {
  const step = { id: "a", label: "A", title: "A", render: () => <p>A body</p> };
  const rec = (version: string): LegalAcceptanceRecord => ({ version, documentsHash: "h", acceptedAt: "2026-01-01T00:00:00.000Z", eligibilityBasis: "individual" });
  const port = (initial: LegalAcceptanceRecord | null, over: Partial<LegalReceiptPort> = {}) => {
    let stored = initial;
    const write = vi.fn(async (r: LegalAcceptanceRecord) => { stored = r; });
    const p: LegalReceiptPort = { read: async () => stored, write, ...over };
    return { p, write, get stored() { return stored; } };
  };

  it("skips the gate when the app receipt satisfies requiredVersion, even with empty settings", async () => {
    const r = port(rec("v2"));
    setup({}, { steps: [step], requiredVersion: "v2", legalReceipt: r.p });
    expect(await screen.findByText("A body")).toBeInTheDocument();
    expect(screen.queryByTestId("onboarding-legal")).toBeNull();
  });
  it("re-prompts when the app receipt is for another version, even if settings say accepted", async () => {
    const r = port(rec("v1"));
    setup(accepted({ legalAcceptance: record("v2") }), { steps: [step], requiredVersion: "v2", legalReceipt: r.p });
    const gate = await screen.findByTestId("onboarding-legal");
    expect(gate).toHaveAttribute("data-variant", "re-acceptance");
  });
  it("shows the first-run gate when the receipt is null, overriding accepted settings", async () => {
    const r = port(null);
    setup(accepted(), { steps: [step], legalReceipt: r.p });
    expect(await screen.findByTestId("onboarding-legal")).toHaveAttribute("data-variant", "first-run");
  });
  it("treats a throwing read as no acceptance and an invalid record as no acceptance", async () => {
    setup({}, { steps: [step], legalReceipt: { read: () => { throw new Error("boom"); }, write: vi.fn() } });
    expect(await screen.findByTestId("onboarding-legal")).toBeInTheDocument();
    cleanup();
    setup({}, { steps: [step], legalReceipt: { read: async () => ({ version: "v" } as never), write: vi.fn() } });
    expect(await screen.findByTestId("onboarding-legal")).toBeInTheDocument();
  });
  it("writes the record to the receipt on agree, then continues", async () => {
    const user = userEvent.setup();
    const r = port(null);
    const { values } = setup({}, { steps: [step], requiredVersion: "v3", documentsHash: "sha-x", legalReceipt: r.p });
    await agree(user);
    expect(await screen.findByText("A body")).toBeInTheDocument();
    expect(r.write).toHaveBeenCalledTimes(1);
    expect(r.write.mock.calls[0]![0]).toMatchObject({ version: "v3", documentsHash: "sha-x", eligibilityBasis: "individual" });
    expect(r.write.mock.calls[0]![0].acceptedAt).toMatch(/Z$/);
    expect((values["onboarding.state"] as { legalAcceptance: LegalAcceptanceRecord }).legalAcceptance.version).toBe("v3");
  });
  it("does not accept when the receipt write fails; the error is shown", async () => {
    const user = userEvent.setup();
    const r = port(null, { write: async () => { throw new Error("disk full"); } });
    const { values } = setup({}, { steps: [step], legalReceipt: r.p });
    await agree(user);
    expect(await screen.findByRole("alert")).toHaveTextContent(/save your acceptance/i);
    expect(screen.queryByText("A body")).toBeNull();
    expect(values["onboarding.state"]).toBeUndefined();
  });
  it.each([
    ["default version", undefined, "version"],
    ["computed documents hash", undefined, "documentsHash"],
    ["documentsHash-only version", "sha256:shown", "version"],
    ["documentsHash-only hash", "sha256:shown", "documentsHash"],
  ] as const)("rejects a mismatched %s receipt and retries without unlocking", async (_name, documentsHash, field) => {
    const user = userEvent.setup();
    let mismatch = true;
    const create = vi.fn(async (request: LegalReceiptCreateRequest): Promise<LegalAcceptanceRecord> => ({
      version: request.version,
      documentsHash: request.documentsHash,
      acceptedAt: "2026-01-01T00:00:00.000Z",
      eligibilityBasis: request.eligibilityBasis,
      ...(mismatch ? { [field]: "another-agreement" } : {}),
    }));
    const { values, onComplete } = setup({}, { steps: [step], documentsHash, legalReceipt: { read: async () => null, create } });
    await agree(user);
    expect(await screen.findByRole("alert")).toHaveTextContent(/save your acceptance/i);
    expect(screen.getByTestId("onboarding-legal")).toBeInTheDocument();
    expect(screen.queryByText("A body")).toBeNull();
    expect(values["onboarding.state"]).toBeUndefined();
    expect(onComplete).not.toHaveBeenCalled();
    const request = create.mock.calls[0]![0];
    expect(request.version).toBe("1");
    if (documentsHash) expect(request.documentsHash).toBe(documentsHash);
    else expect(request.documentsHash).toMatch(/^(sha256|fnv1a):/);

    mismatch = false;
    await user.click(screen.getByRole("button", { name: /agree/i }));
    await screen.findByText("A body");
    expect(create).toHaveBeenCalledTimes(2);
    expect(create.mock.calls[1]![0]).toEqual(request);
    expect((values["onboarding.state"] as { legalAcceptance: LegalAcceptanceRecord }).legalAcceptance).toMatchObject({
      version: request.version, documentsHash: request.documentsHash, eligibilityBasis: request.eligibilityBasis,
    });
  });
  it("refuses an echoed document hash that conflicts with the independent required hash", async () => {
    const user = userEvent.setup();
    const create = vi.fn(async (request: LegalReceiptCreateRequest): Promise<LegalAcceptanceRecord> => ({
      version: request.version,
      documentsHash: request.documentsHash,
      acceptedAt: "2026-01-01T00:00:00.000Z",
      eligibilityBasis: request.eligibilityBasis,
    }));
    const { values, onComplete } = setup({}, { documentsHash: "shown-A", requiredDocumentsHash: "required-B", legalReceipt: { read: async () => null, create } });
    await agree(user);
    expect(await screen.findByRole("alert")).toHaveTextContent(/save your acceptance/i);
    expect(create.mock.calls[0]![0].documentsHash).toBe("shown-A");
    expect(screen.getByTestId("onboarding-legal")).toBeInTheDocument();
    expect(values["onboarding.state"]).toBeUndefined();
    expect(onComplete).not.toHaveBeenCalled();
    // A refused response must not enter the retry cache.
    await user.click(screen.getByRole("button", { name: /agree/i }));
    await waitFor(() => expect(create).toHaveBeenCalledTimes(2));
    expect(values["onboarding.state"]).toBeUndefined();
    expect(onComplete).not.toHaveBeenCalled();
  });
  it("maps a ScrapeRight-style four-field receipt both ways", async () => {
    type Receipt = { legalAcceptanceVersion: string | null; legalAcceptedAt: string | null; legalEligibilityBasis: "individual" | "enterprise" | null; legalMaterialSha256: string | null };
    let store: Receipt | null = { legalAcceptanceVersion: "2026-09", legalAcceptedAt: "2026-09-01T10:00:00.000Z", legalEligibilityBasis: "enterprise", legalMaterialSha256: "abc" };
    const mapped: LegalReceiptPort = {
      read: () => (store && store.legalAcceptanceVersion && store.legalAcceptedAt && store.legalEligibilityBasis ? { version: store.legalAcceptanceVersion, acceptedAt: store.legalAcceptedAt, eligibilityBasis: store.legalEligibilityBasis, documentsHash: store.legalMaterialSha256 ?? "" } : null),
      write: (r) => { store = { legalAcceptanceVersion: r.version, legalAcceptedAt: r.acceptedAt, legalEligibilityBasis: r.eligibilityBasis, legalMaterialSha256: r.documentsHash }; },
    };
    setup({}, { steps: [step], requiredVersion: "2026-09", legalReceipt: mapped });
    expect(await screen.findByText("A body")).toBeInTheDocument();
    cleanup();
    store = null;
    const user = userEvent.setup();
    setup({}, { steps: [step], requiredVersion: "2026-09", documentsHash: "abc", legalReceipt: mapped });
    await agree(user);
    await screen.findByText("A body");
    expect(store).toMatchObject({ legalAcceptanceVersion: "2026-09", legalEligibilityBasis: "individual", legalMaterialSha256: "abc" });
  });
});

describe("isLegalAccepted", () => {
  const stored = (version: string, documentsHash: string) => ({
    legalAccepted: true,
    legalAcceptance: { version, documentsHash, acceptedAt: "2026-10-12T00:00:00.000Z" },
  }) as unknown as Parameters<typeof isLegalAccepted>[0];

  it("re-prompts when the text changed and the version did not", () => {
    expect(isLegalAccepted(stored("2026-09", "sha256:old"), "2026-09", undefined, "sha256:new")).toBe(false);
    expect(isLegalAccepted(stored("2026-09", "sha256:new"), "2026-09", undefined, "sha256:new")).toBe(true);
  });

  it("still requires the version when both are given, and the hash alone when only it is given", () => {
    expect(isLegalAccepted(stored("2026-08", "sha256:new"), "2026-09", undefined, "sha256:new")).toBe(false);
    expect(isLegalAccepted(stored("2026-08", "sha256:new"), undefined, undefined, "sha256:new")).toBe(true);
  });

  it("never accepts a bare legacy flag, even with no version or hash requirement", () => {
    const legacy = { legalAccepted: true } as unknown as Parameters<typeof isLegalAccepted>[0];
    expect(isLegalAccepted(legacy)).toBe(false);
    expect(isLegalAccepted({ ...legacy, legalAcceptance: record("1") } as never)).toBe(true);
  });

  it("compares nothing new when no required hash is given", () => {
    expect(isLegalAccepted(stored("2026-09", "sha256:old"), "2026-09")).toBe(true);
  });

  it("never accepts a record whose acceptedAt is not canonical UTC", () => {
    const withAt = (acceptedAt: string) => ({
      legalAccepted: true,
      legalAcceptance: { version: "2026-09", documentsHash: "sha256:new", acceptedAt, eligibilityBasis: "individual" },
    }) as unknown as Parameters<typeof isLegalAccepted>[0];
    for (const bad of ["2026-10-12", "2026-10-12T00:00:00Z", "2026-10-12T00:00:00.000+00:00", "not-a-date", ""]) {
      expect(isLegalAccepted(withAt(bad), "2026-09", undefined, "sha256:new")).toBe(false);
      expect(isLegalAccepted(withAt(bad))).toBe(false);
    }
    expect(isLegalAccepted(withAt("2026-10-12T00:00:00.000Z"), "2026-09", undefined, "sha256:new")).toBe(true);
  });
});

describe("signInStep (Orthic-Labs/rightkit#279)", () => {
  it("says it opens the browser and calls the shared owner per provider", async () => {
    const user = userEvent.setup();
    const start = vi.fn(async () => ({ signedIn: true, account: "a@example.com" }));
    const step = signInStep({ providers: [{ id: "google" }, { id: "apple" }], load: async () => ({ signedIn: false }), start });
    render(<>{step.render({} as never)}</>);
    expect(screen.getByText(/Opens your browser/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Sign in with Google" }));
    await waitFor(() => expect(start).toHaveBeenCalledWith("google"));
    await waitFor(() => expect(screen.getByText("a@example.com")).toBeInTheDocument());
  });
});

describe("legalMode (Orthic-Labs/rightkit#285)", () => {
  const stepA = { id: "a", label: "A", title: "Step A", render: () => <p>A body</p> };
  const stepB = { id: "b", label: "B", title: "Step B", render: () => <p>B body</p> };

  it("none: no gate, no Agreement step, starts on the first app step and needs no `legal`", async () => {
    const s = memorySettings({});
    render(<Onboarding settings={s.backend} legalMode="none" appName="Ember" steps={[stepA, stepB]} onComplete={vi.fn()} />);
    expect(await screen.findByText("A body")).toBeInTheDocument();
    expect(screen.queryByTestId("onboarding-legal")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Agreement/ })).not.toBeInTheDocument();
    expect(screen.getByText("Ember")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Back" })).toBeDisabled();
    await userEvent.setup().click(screen.getByRole("button", { name: "Continue" }));
    expect(await screen.findByText("B body")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Back" })).toBeEnabled();
  });

  it("none: completes through the last step", async () => {
    const s = memorySettings({});
    const onComplete = vi.fn();
    render(<Onboarding settings={s.backend} legalMode="none" steps={[stepA]} onComplete={onComplete} />);
    await userEvent.setup().click(await screen.findByRole("button", { name: "Finish setup" }));
    await waitFor(() => expect(onComplete).toHaveBeenCalledTimes(1));
  });

  it("notice: no gate; the first step lists the documents as links", async () => {
    const s = memorySettings({});
    render(<Onboarding settings={s.backend} legal={legal} legalMode="notice" steps={[stepA]} onComplete={vi.fn()} />);
    const link = await screen.findByRole("link", { name: "Terms" });
    expect(link).toHaveAttribute("href", "https://example.test/terms");
    expect(screen.queryByTestId("onboarding-legal")).not.toBeInTheDocument();
    expect(screen.getByText("AcmeRight")).toBeInTheDocument();
    await userEvent.setup().click(screen.getByRole("button", { name: "Continue" }));
    expect(await screen.findByText("A body")).toBeInTheDocument();
  });

  it("gate without `legal` is a programming error", () => {
    const s = memorySettings({});
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    expect(() => render(<Onboarding settings={s.backend} onComplete={vi.fn()} />)).toThrow(/legal/);
    spy.mockRestore();
  });
});

describe("step hooks, results, skip and labels", () => {
  it("keeps the step when onContinue returns false, and advances when it returns void", async () => {
    const user = userEvent.setup();
    let allow = false;
    const onContinue = vi.fn(() => allow ? undefined : false);
    const { onComplete } = setup(accepted({ currentStep: 0 }), {
      steps: [{ id: "keys", label: "Keys", title: "Keys", onContinue, render: () => <p>keys body</p> }, { id: "done", label: "Done", title: "", render: () => <p>done body</p> }],
    });
    await user.click(await screen.findByRole("button", { name: "Continue" }));
    await waitFor(() => expect(onContinue).toHaveBeenCalledTimes(1));
    expect(screen.getByText("keys body")).toBeInTheDocument();
    allow = true;
    await user.click(screen.getByRole("button", { name: "Continue" }));
    expect(await screen.findByText("done body")).toBeInTheDocument();
    expect(onComplete).not.toHaveBeenCalled();
  });

  it("keeps the step and shows the message when onContinue throws", async () => {
    const user = userEvent.setup();
    setup(accepted({ currentStep: 0 }), {
      steps: [{ id: "keys", label: "Keys", title: "Keys", onContinue: async () => { throw new Error("Could not save key"); }, render: () => <p>keys body</p> }],
    });
    await user.click(await screen.findByRole("button", { name: "Finish setup" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Could not save key");
    expect(screen.getByText("keys body")).toBeInTheDocument();
  });

  it("passes completed, skipped and setResult values to onComplete", async () => {
    const user = userEvent.setup();
    const { onComplete } = setup(accepted({ currentStep: 0 }), {
      steps: [
        { id: "a", label: "A", title: "A", skippable: true, render: () => <p>a body</p> },
        { id: "b", label: "B", title: "B", render: (ctx) => { ctx.setResult("pro"); return <p>b body</p>; } },
      ],
    });
    await user.click(await screen.findByRole("button", { name: "Skip" }));
    await user.click(await screen.findByRole("button", { name: "Finish setup" }));
    await waitFor(() => expect(onComplete).toHaveBeenCalledTimes(1));
    // "legal" is in the persisted completed list because the agreement was accepted (fixture); "a" was skipped, not completed.
    expect(onComplete.mock.calls[0]![0]).toEqual({ completed: ["legal", "b"], skipped: ["a"], steps: { b: "pro" } });
  });

  it("hides Skip on steps that are not skippable and on the last step", async () => {
    setup(accepted({ currentStep: 0 }), { steps: [{ id: "a", label: "A", title: "A", render: () => <p>a</p> }] });
    expect(await screen.findByRole("button", { name: "Finish setup" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Skip" })).toBeNull();
  });

  it("uses per-step continueLabel and the labels prop for chrome text", async () => {
    setup(accepted({ currentStep: 0 }), {
      labels: { skip: "Later", back: "Previous", continue: "Next", loading: "Please wait" },
      steps: [{ id: "a", label: "A", title: "A", continueLabel: "Open AcmeRight", render: () => <p>a</p> }, { id: "b", label: "B", title: "B", render: () => <p>b</p> }],
    });
    expect(await screen.findByRole("button", { name: "Open AcmeRight" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Later" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Previous" })).toBeInTheDocument();
  });

  it("hides the heading for an untitled step", async () => {
    setup(accepted({ currentStep: 0 }), { steps: [{ id: "w", label: "Welcome", title: "", render: () => <p>welcome body</p> }] });
    expect(await screen.findByText("welcome body")).toBeInTheDocument();
    expect(screen.queryByRole("heading", { level: 1 })).toBeNull();
  });
});

describe("rail navigation, per-step skip and skipped-step bookkeeping", () => {
  it("disables rail navigation while onContinue is awaited", async () => {
    const user = userEvent.setup();
    let release!: () => void;
    const hold = new Promise<void>((resolve) => { release = resolve; });
    setup(accepted({ currentStep: 0 }), {
      steps: [
        { id: "a", label: "A", title: "A", skippable: true, onContinue: () => hold, render: () => <p>a body</p> },
        { id: "b", label: "B", title: "B", render: () => <p>b body</p> },
      ],
    });
    await user.click(await screen.findByRole("button", { name: "Continue" }));
    const rail = screen.getByRole("navigation", { name: "Setup progress" });
    expect(within(rail).getByRole("button", { name: /A$/ })).toBeDisabled();
    expect(within(rail).getByRole("button", { name: /B$/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Skip" })).toBeDisabled();
    release();
    expect(await screen.findByText("b body")).toBeInTheDocument();
  });

  it("offers per-step Skip when canContinue is false, and finishes from a skipped final step", async () => {
    const user = userEvent.setup();
    const { onComplete } = setup(accepted({ currentStep: 0 }), {
      steps: [
        { id: "a", label: "A", title: "A", skippable: true, canContinue: () => false, blockedHint: "Add a key first", render: () => <p>a body</p> },
        { id: "b", label: "B", title: "B", skippable: true, render: () => <p>b body</p> },
      ],
    });
    await user.click(await screen.findByRole("button", { name: "Skip" }));
    expect(await screen.findByText("b body")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Skip" }));
    await waitFor(() => expect(onComplete).toHaveBeenCalledTimes(1));
    expect(onComplete.mock.calls[0]![0]).toEqual({ completed: ["legal"], skipped: ["a", "b"], steps: {} });
  });

  it("lists a step only as skipped when it was completed, revisited and then skipped", async () => {
    const user = userEvent.setup();
    const { onComplete } = setup(accepted({ currentStep: 0 }), {
      steps: [
        { id: "a", label: "A", title: "A", skippable: true, render: () => <p>a body</p> },
        { id: "b", label: "B", title: "B", render: () => <p>b body</p> },
      ],
    });
    await user.click(await screen.findByRole("button", { name: "Continue" }));
    expect(await screen.findByText("b body")).toBeInTheDocument();
    const rail = screen.getByRole("navigation", { name: "Setup progress" });
    await user.click(within(rail).getByRole("button", { name: /A$/ }));
    expect(await screen.findByText("a body")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Skip" }));
    expect(await screen.findByText("b body")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Finish setup" }));
    await waitFor(() => expect(onComplete).toHaveBeenCalledTimes(1));
    expect(onComplete.mock.calls[0]![0]).toEqual({ completed: ["legal", "b"], skipped: ["a"], steps: {} });
  });
});
