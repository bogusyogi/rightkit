// @vitest-environment happy-dom
import { renderToStaticMarkup } from "react-dom/server";
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import {
  getCliDelegationPolicy,
  listCliProviders,
  mutateCliProvider,
} from "@rightkit/agent-ui-core/lib/daemon/cli-providers";

import ProvidersTab from "./ProvidersTab";

vi.mock("@rightkit/agent-ui-core/lib/daemon/cli-providers", () => ({
  listCliProviders: vi.fn(),
  mutateCliProvider: vi.fn(),
  getCliDelegationPolicy: vi.fn(),
  setCliDelegationPolicy: vi.fn(),
}));
vi.mock("@rightkit/agent-ui-core/lib/accounts/accounts", () => ({
  fetchAccounts: vi.fn().mockResolvedValue([]),
  addApiKeyAccount: vi.fn(),
  removeApiKeyAccount: vi.fn(),
}));
vi.mock("@rightkit/agent-ui-core/lib/daemon/inference", () => ({
  listInferenceConnections: vi.fn().mockResolvedValue([]),
  listInferenceCatalog: vi.fn().mockResolvedValue([]),
  getInferenceLoad: vi.fn().mockResolvedValue(null),
  createDirectConnection: vi.fn(),
  deleteDirectConnection: vi.fn(),
  probeInferenceConnection: vi.fn(),
}));

const snapshot = {
  providers: ["claude", "codex", "grok", "cursor", "cline", "commandcode"].map((providerId) => ({
    providerId,
    displayName: providerId,
    installed: true,
    executable: "/usr/bin/cli",
    binaryVersion: "1",
    executableSha256: "a".repeat(64),
    transport: "acp",
    auth: {
      mode: "native_cli_account",
      signedIn: providerId === "claude",
      detectedSignedIn: providerId === "claude" || providerId === "codex",
      launchReady: providerId === "claude",
      source: "managed_cli",
      state: providerId === "claude" ? "configured" : "needs_auth",
      accountLabel: providerId === "claude" ? "default" : null,
    },
    harness: {
      eligibility: providerId === "claude" ? "harness_verification_required" : "login_required",
      attestationId: null,
      isolationMode: "exclusive_coderight_gateway",
    },
    models: { available: true, items: [], source: "native" },
    repairActions: [],
  })),
};

function baseProps() {
  return {
    settings: null,
    config: { url: "http://127.0.0.1:5057", token: "test" },
    onLoginCliProvider: vi.fn(),
  };
}

describe("ProvidersTab", () => {
  it("renders native CLI sign-in controls without API-key fields", () => {
    const html = renderToStaticMarkup(<ProvidersTab {...baseProps()} />);

    expect(html).toContain("Claude Code");
    expect(html).toContain("Codex");
    expect(html).toContain("Sign in with Claude");
    expect(html).toContain("Sign in with Codex");
    // API keys live only in the separate "API key accounts" form; vendor cards never ask for one.
    const cards = html.slice(html.indexOf('data-testid="provider-card-'));
    expect(cards).not.toMatch(/\bAPI key\b/);
    expect(cards).not.toContain('type="password"');
  });

  it("reports launch readiness separately from ambient sign-in", async () => {
    vi.mocked(listCliProviders).mockResolvedValue(snapshot as never);
    vi.mocked(mutateCliProvider).mockResolvedValue(snapshot as never);
    render(<ProvidersTab {...baseProps()} />);
    expect(
      await screen.findByText(
        "1 of 6 launch-ready · 2 ambient sign-ins detected · 5 need routing or verification",
      ),
    ).toBeTruthy();
    expect(screen.getByText("Signed in outside App")).toBeTruthy();
    fireEvent.click(screen.getByTestId("provider-card-codex-sign-in"));
    expect(mutateCliProvider).toHaveBeenCalledWith(expect.anything(), "codex", "login");
  });

  it("keeps provider discovery when a deleted session policy is unavailable", async () => {
    vi.mocked(listCliProviders).mockResolvedValue(snapshot as never);
    vi.mocked(getCliDelegationPolicy).mockRejectedValue(new Error("session not found"));
    render(<ProvidersTab {...baseProps()} sessionId="deleted-session" />);

    expect(
      await screen.findByText(
        "1 of 6 launch-ready · 2 ambient sign-ins detected · 5 need routing or verification",
      ),
    ).toBeTruthy();
    expect(screen.getByText("Harness attestations unavailable: session not found")).toBeTruthy();
  });

  it("renders the six launchable providers and never Kiro", () => {
    const html = renderToStaticMarkup(<ProvidersTab {...baseProps()} />);
    for (const name of [
      "Claude Code",
      "Codex CLI",
      "Grok Build CLI",
      "Cursor Agent CLI",
      "Cline CLI",
      "Command Code CLI",
    ]) {
      expect(html).toContain(name);
    }
    expect(html).not.toContain("Kiro");
  });
});

describe("ProvidersTab progressive disclosure", () => {
  it("collapses the model-provider mapping behind an advanced disclosure by default", () => {
    render(
      <ProvidersTab
        {...baseProps()}
        settings={{
          workspaceRoot: "",
          defaultModel: "",
          primaryOrchestratorModelId: "",
          roleModelMode: "single",
          roleModelSplitAvailable: false,
          models: [{ id: "m1", displayName: "M1", provider: "claude" } as never],
          agents: [],
          budget: {} as never,
          benchmarks: {} as never,
          remote: {} as never,
          routing: {} as never,
          keepDaemonResident: false,
        }}
      />,
    );
    // React.StrictMode renders the same tree twice under happy-dom; both copies
    // share the same accessible name, so use getAllByRole and grab the LAST
    // committed button (the live one bound to the click handler).
    const toggles = screen.getAllByRole("button", {
      name: "Advanced: model → provider mapping",
    });
    expect(toggles.length).toBeGreaterThan(0);
    const toggle = toggles[toggles.length - 1] as HTMLElement;
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByText("claude")).toBeNull();

    fireEvent.click(toggle);

    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    // shortLabelForProvider("claude") renders the standalone "Claude" label
    // inside the mapping; provider cards render "Claude Code", so an exact
    // match picks the mapping entry rather than the card heading. StrictMode
    // duplicates the tree, so use getAllByText and confirm at least one copy.
    expect(screen.getAllByText("Claude", { exact: true }).length).toBeGreaterThan(0);
  });
});
