// @vitest-environment jsdom
// PK10-5: the first-run, re-acceptance, permission, model, navigation and layout flows end to end through `Onboarding`.
import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Onboarding } from "./Onboarding.js";
import type { ModelEntry, ModelsBackend, OnboardingProps, OnboardingStep, PermissionStatus, PermissionsBackend, SettingsBackend } from "./types.js";

afterEach(() => cleanup());

const theme = { bg: "#fff", surface: "#fff", border: "#ddd", text: "#000", muted: "#666", accent: "#70c", accentContrast: "#fff", fontDisplay: "system-ui", fontBody: "system-ui" };
const legal = { appName: "AcmeRight", trustLine: "Trust.", documents: [{ title: "Terms", url: "https://example.test/terms" }], agreementStatement: "I agree.", theme };
const KEY = "onboarding.state";

/** Memory settings. `failPatches` makes the next N patches reject, as a read-only or full disk would. */
function memorySettings(initial: Record<string, unknown> = {}) {
  const values: Record<string, unknown> = { ...initial };
  let failPatches = 0;
  const backend: SettingsBackend = {
    load: async () => ({ values: { ...values }, revision: 1 }),
    patch: async (patch) => {
      if (failPatches > 0) { failPatches -= 1; throw new Error("settings are read-only"); }
      Object.assign(values, patch);
      return { values: { ...values }, revision: 2 };
    },
  };
  return { backend, values, failNext: (count = 1) => { failPatches = count; } };
}

const record = (version: string, documentsHash = "h") => ({ version, documentsHash, acceptedAt: "2026-01-01T00:00:00.000Z", eligibilityBasis: "individual" });
const accepted = (extra: Record<string, unknown> = {}) => ({ [KEY]: { version: 1, currentStep: 1, completed: ["legal"], skipped: false, legalAccepted: true, legalAcceptance: record("1"), ...extra } });
const stateOf = (values: Record<string, unknown>) => values[KEY] as { currentStep: number; completed: string[]; skipped: boolean; skippedSteps?: string[]; legalAcceptance?: { version: string; documentsHash: string } };
const step = (id: string, extra: Partial<OnboardingStep> = {}): OnboardingStep => ({ id, label: id.toUpperCase(), title: `Step ${id}`, render: () => <p>{id} body</p>, ...extra });

function setup(initial: Record<string, unknown>, props: Partial<OnboardingProps> = {}) {
  const s = memorySettings(initial);
  const onComplete = vi.fn();
  const onSkip = vi.fn();
  const view = render(<Onboarding settings={s.backend} legal={legal} onComplete={onComplete} onSkip={onSkip} {...props} />);
  return { ...s, onComplete, onSkip, ...view };
}

async function agree(user: ReturnType<typeof userEvent.setup>) {
  await user.click((await screen.findAllByRole("radio"))[0]!);
  await user.click(screen.getByRole("checkbox"));
  await user.click(screen.getByRole("button", { name: /agree/i }));
}

describe("first run and relaunch", () => {
  it("shows the first-run gate, records the agreement, lands on the first step and resumes there after a relaunch", async () => {
    const user = userEvent.setup();
    const steps = [step("a"), step("b"), step("c")];
    const first = setup({}, { steps, requiredVersion: "v1", requiredDocumentsHash: "sha256:one" });
    expect((await screen.findByTestId("onboarding-legal")).dataset.variant).toBe("first-run");
    await agree(user);
    expect(await screen.findByText("a body")).toBeInTheDocument();
    expect(stateOf(first.values)).toMatchObject({ currentStep: 1, completed: ["legal"], legalAcceptance: { version: "v1", documentsHash: "sha256:one" } });
    await user.click(screen.getByRole("button", { name: "Continue" }));
    expect(await screen.findByText("b body")).toBeInTheDocument();
    cleanup();
    // Relaunch with the saved state: no gate, back on the step that was not finished.
    setup(first.values, { steps, requiredVersion: "v1", requiredDocumentsHash: "sha256:one" });
    expect(await screen.findByText("b body")).toBeInTheDocument();
    expect(screen.queryByTestId("onboarding-legal")).toBeNull();
    const rail = screen.getByRole("navigation", { name: "Setup progress" });
    expect(within(rail).getByRole("button", { name: /A$/ })).toHaveClass("is-done");
    expect(within(rail).getByRole("button", { name: /B$/ })).toHaveAttribute("aria-current", "step");
  });

  it("relaunches at an incomplete middle step and an out-of-range saved step clamps to the last", async () => {
    setup(accepted({ currentStep: 2, completed: ["legal", "a"] }), { steps: [step("a"), step("b"), step("c")] });
    expect(await screen.findByText("b body")).toBeInTheDocument();
    cleanup();
    setup(accepted({ currentStep: 99 }), { steps: [step("a"), step("b")] });
    expect(await screen.findByText("b body")).toBeInTheDocument();
  });
});

describe("re-acceptance when the documents change", () => {
  it("re-prompts with the re-acceptance variant when the document digest changed under the same version", async () => {
    const user = userEvent.setup();
    const { values } = setup(accepted({ currentStep: 2, completed: ["legal", "a"], legalAcceptance: record("v1", "sha256:old") }), { steps: [step("a"), step("b")], requiredVersion: "v1", requiredDocumentsHash: "sha256:new" });
    expect((await screen.findByTestId("onboarding-legal")).dataset.variant).toBe("re-acceptance");
    await agree(user);
    expect(await screen.findByText("b body")).toBeInTheDocument();
    // Progress through the other steps is kept; only the acceptance moved.
    expect(stateOf(values)).toMatchObject({ currentStep: 2, completed: ["legal", "a"], legalAcceptance: { version: "v1", documentsHash: "sha256:new" } });
  });

  it("does not re-prompt while the digest still matches", async () => {
    setup(accepted({ legalAcceptance: record("v1", "sha256:same") }), { steps: [step("a")], requiredVersion: "v1", requiredDocumentsHash: "sha256:same" });
    expect(await screen.findByText("a body")).toBeInTheDocument();
    expect(screen.queryByTestId("onboarding-legal")).toBeNull();
  });
});

describe("failed assent save (settings backend)", () => {
  it("keeps the gate closed with an alert, then accepts on retry", async () => {
    const user = userEvent.setup();
    const { values, failNext } = setup({}, { steps: [step("a")] });
    failNext();
    await agree(user);
    expect(await screen.findByRole("alert")).toHaveTextContent(/save your acceptance/i);
    expect(screen.getByTestId("onboarding-legal")).toBeInTheDocument();
    expect(screen.queryByText("a body")).toBeNull();
    expect(values[KEY]).toBeUndefined();
    await user.click(screen.getByRole("button", { name: /agree/i }));
    expect(await screen.findByText("a body")).toBeInTheDocument();
    expect(stateOf(values).completed).toEqual(["legal"]);
  });

  it("does not call onComplete when the agreement is the only step and its save failed", async () => {
    const user = userEvent.setup();
    const { onComplete, failNext } = setup({}, { steps: [] });
    failNext();
    await agree(user);
    await screen.findByRole("alert");
    expect(onComplete).not.toHaveBeenCalled();
  });
});

describe("permissions step: cancel, deny, retry", () => {
  const mic: PermissionStatus = { id: "mic", label: "Microphone", state: "needed" };
  const backend = (request: PermissionsBackend["request"], list: PermissionStatus[] = [mic]): PermissionsBackend => ({ list: async () => list, request });
  const row = (id: string) => document.querySelector<HTMLElement>(`[data-permission-id="${id}"]`)!;

  it("keeps the row open when the user dismisses the system prompt", async () => {
    const user = userEvent.setup();
    const request = vi.fn(async () => ({ ...mic, state: "needed" as const }));
    setup(accepted(), { permissions: backend(request) });
    await user.click(await screen.findByRole("button", { name: "Allow" }));
    await waitFor(() => expect(request).toHaveBeenCalledWith("mic"));
    await waitFor(() => expect(screen.getByRole("button", { name: "Allow" })).toBeEnabled());
    expect(within(row("mic")).getByText("needed")).toBeInTheDocument();
  });

  it("shows a denial, offers Try again, and grants on the retry", async () => {
    const user = userEvent.setup();
    const answers: PermissionStatus["state"][] = ["denied", "ready"];
    const request = vi.fn(async () => ({ ...mic, state: answers.shift()! }));
    setup(accepted(), { permissions: backend(request) });
    await user.click(await screen.findByRole("button", { name: "Allow" }));
    expect(await within(row("mic")).findByText("denied")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Try again" }));
    expect(await within(row("mic")).findByText("ready")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Allow|Try again/ })).toBeNull();
    expect(request).toHaveBeenCalledTimes(2);
  });

  it("shows the error when the request itself fails and lets the user ask again", async () => {
    const user = userEvent.setup();
    let fail = true;
    const request = vi.fn(async () => { if (fail) throw new Error("The system prompt could not be shown."); return { ...mic, state: "ready" as const }; });
    setup(accepted(), { permissions: backend(request) });
    await user.click(await screen.findByRole("button", { name: "Allow" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("The system prompt could not be shown.");
    fail = false;
    await user.click(screen.getByRole("button", { name: "Allow" }));
    expect(await within(row("mic")).findByText("ready")).toBeInTheDocument();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("does not offer a button for a restricted permission and still lets the user continue or skip", async () => {
    const user = userEvent.setup();
    const { onComplete } = setup(accepted(), { permissions: backend(vi.fn(), [{ id: "screen", label: "Screen", state: "restricted" }]) });
    expect(await within(await screen.findByTestId("onboarding-permissions")).findByText("restricted")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Allow|Try again/ })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Skip" }));
    await waitFor(() => expect(onComplete).toHaveBeenCalledWith({ completed: ["legal"], skipped: ["permissions"], steps: {} }));
  });

  it("offers Try again when listing the permissions fails", async () => {
    const user = userEvent.setup();
    let calls = 0;
    const list = vi.fn(async () => { calls += 1; if (calls === 1) throw new Error("Permissions are unavailable."); return [mic]; });
    setup(accepted(), { permissions: { list, request: vi.fn() } });
    expect(await screen.findByRole("alert")).toHaveTextContent("Permissions are unavailable.");
    await user.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("button", { name: "Allow" })).toBeInTheDocument();
    expect(screen.queryByRole("alert")).toBeNull();
  });
});

describe("models step offline", () => {
  const entry = (state: ModelEntry["state"], extra: Partial<ModelEntry> = {}): ModelEntry => ({ id: "m", label: "Model", state, ...extra });

  it("shows the failure when the list cannot load, then loads on Try again", async () => {
    const user = userEvent.setup();
    let online = false;
    const list = vi.fn(async () => { if (!online) throw new Error("You are offline."); return [entry("missing")]; });
    setup(accepted(), { models: { list, install: vi.fn(async () => {}) } });
    expect(await screen.findByRole("alert")).toHaveTextContent("You are offline.");
    expect(screen.queryByText("No models available.")).toBeNull();
    online = true;
    await user.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("button", { name: "Download" })).toBeInTheDocument();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("keeps the model downloadable when the install fails offline and retries it", async () => {
    const user = userEvent.setup();
    let online = false;
    let installed = false;
    const install = vi.fn(async () => { if (!online) throw new Error("Download failed: offline."); installed = true; });
    const list = async () => [entry(installed ? "installed" : "missing")];
    setup(accepted(), { models: { list, install } });
    await user.click(await screen.findByRole("button", { name: "Download" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Download failed: offline.");
    await waitFor(() => expect(screen.getByRole("button", { name: "Download" })).toBeEnabled());
    online = true;
    await user.click(screen.getByRole("button", { name: "Download" }));
    await waitFor(() => expect(screen.getByText("installed")).toBeInTheDocument());
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("retries a model the backend reports in the error state, and requireModel keeps Finish disabled until it installs", async () => {
    const user = userEvent.setup();
    let installed = false;
    const install = vi.fn(async () => { installed = true; });
    const list = async () => [installed ? entry("installed") : entry("error", { error: { code: "network", message: "offline" } })];
    setup(accepted(), { models: { list, install } as ModelsBackend, requireModel: true });
    const finish = await screen.findByRole("button", { name: "Finish setup" });
    expect(finish).toBeDisabled();
    await user.click(await screen.findByRole("button", { name: "Retry" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Finish setup" })).toBeEnabled());
    expect(install).toHaveBeenCalledWith("m");
  });
});

describe("skip, back and finish", () => {
  it("Back returns to the previous step and saves it; the rail never reaches the agreement", async () => {
    const user = userEvent.setup();
    const { values } = setup(accepted({ currentStep: 2, completed: ["legal", "a"] }), { steps: [step("a"), step("b")] });
    expect(await screen.findByText("b body")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Back" }));
    expect(await screen.findByText("a body")).toBeInTheDocument();
    expect(stateOf(values).currentStep).toBe(1);
    expect(screen.getByRole("button", { name: "Back" })).toBeDisabled();
  });

  it("Set up later saves the skipped flag first and then calls onSkip, not onComplete", async () => {
    const user = userEvent.setup();
    const { values, onSkip, onComplete } = setup(accepted(), { steps: [step("a"), step("b")] });
    await user.click(await screen.findByRole("button", { name: "Set up later" }));
    await waitFor(() => expect(onSkip).toHaveBeenCalledTimes(1));
    expect(stateOf(values).skipped).toBe(true);
    expect(onComplete).not.toHaveBeenCalled();
  });

  it("Set up later does not call onSkip when the save fails, and the error is shown", async () => {
    const user = userEvent.setup();
    const { onSkip, failNext } = setup(accepted(), { steps: [step("a")] });
    const later = await screen.findByRole("button", { name: "Set up later" });
    failNext();
    await user.click(later);
    expect(await screen.findByRole("alert")).toHaveTextContent("settings are read-only");
    expect(onSkip).not.toHaveBeenCalled();
  });

  it("Finish calls onComplete once after the state is saved; a failed save keeps the last step and the user can finish on retry", async () => {
    const user = userEvent.setup();
    const { values, onComplete, failNext } = setup(accepted(), { steps: [step("a")] });
    const finish = await screen.findByRole("button", { name: "Finish setup" });
    failNext();
    await user.click(finish);
    expect(await screen.findByRole("alert")).toHaveTextContent("settings are read-only");
    expect(onComplete).not.toHaveBeenCalled();
    expect(screen.getByText("a body")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Finish setup" }));
    await waitFor(() => expect(onComplete).toHaveBeenCalledTimes(1));
    expect(onComplete).toHaveBeenCalledWith({ completed: ["legal", "a"], skipped: [], steps: {} });
    expect(stateOf(values).completed).toEqual(["legal", "a"]);
  });
});

describe("both layouts", () => {
  it("window (default) and contained differ only by the contained class on the setup surface", async () => {
    const steps = [step("a")];
    setup(accepted(), { steps });
    expect(await screen.findByTestId("onboarding")).not.toHaveClass("rk-onboarding--contained");
    cleanup();
    setup(accepted(), { steps, layout: "window" });
    expect(await screen.findByTestId("onboarding")).not.toHaveClass("rk-onboarding--contained");
    cleanup();
    setup(accepted(), { steps, layout: "contained" });
    expect(await screen.findByTestId("onboarding")).toHaveClass("rk-onboarding--contained");
    expect(screen.getByText("a body")).toBeInTheDocument();
  });

  it("contained applies to the loading state and the legal gate as well", async () => {
    const s = memorySettings();
    let release!: () => void;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    const slow: SettingsBackend = { ...s.backend, load: async () => { await gate; return s.backend.load(); } };
    const { container } = render(<Onboarding settings={slow} legal={legal} layout="contained" onComplete={vi.fn()} />);
    expect(container.querySelector("[aria-busy=true]")).toHaveClass("rk-onboarding--contained");
    release();
    expect(await screen.findByTestId("onboarding-legal")).toHaveClass("rk-onboarding--contained");
    expect(container.querySelector(".rk-legal-gate")).not.toBeNull();
    expect((container.querySelector(".rk-legal-gate") as HTMLElement).style.minHeight).toBe("");
  });
});
