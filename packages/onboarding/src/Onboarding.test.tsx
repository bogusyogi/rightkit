// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Onboarding } from "./Onboarding.js";
import type { LegalAcceptanceRecord, LegalReceiptPort, ModelEntry, ModelsBackend, OnboardingProps, SettingsBackend } from "./types.js";

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

const accepted = (extra: Record<string, unknown> = {}) => ({ "onboarding.state": { version: 1, currentStep: 1, completed: ["legal"], skipped: false, legalAccepted: true, ...extra } });
const record = (version: string) => ({ version, documentsHash: "h", acceptedAt: "2026-01-01T00:00:00.000Z", eligibilityBasis: "individual" });

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
