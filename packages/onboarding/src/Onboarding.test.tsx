// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Onboarding } from "./Onboarding.js";
import type { ModelEntry, ModelsBackend, OnboardingProps, SettingsBackend } from "./types.js";

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
