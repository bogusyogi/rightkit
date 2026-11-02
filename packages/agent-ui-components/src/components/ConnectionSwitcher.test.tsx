// @vitest-environment happy-dom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  createDirectConnection,
  listInferenceCatalog,
  listInferenceConnections,
  probeInferenceConnection,
} from "@rightkit/agent-ui-core/lib/daemon/inference";
import type { DirectConnection } from "@rightkit/agent-ui-core/lib/types/inference";
import { useConnectionProfilesStore } from "@rightkit/agent-ui-core/state/connectionProfiles";
import ConnectionSwitcher from "./ConnectionSwitcher.js";

vi.mock("@rightkit/agent-ui-core/lib/daemon/inference", () => ({
  createDirectConnection: vi.fn(),
  deleteDirectConnection: vi.fn(),
  listInferenceCatalog: vi.fn(),
  listInferenceConnections: vi.fn(),
  probeInferenceConnection: vi.fn(),
  setDirectConnectionSecret: vi.fn(),
  updateDirectConnection: vi.fn(),
}));

const config = { url: "http://127.0.0.1:5057", token: "test" };

function connection(overrides: Partial<DirectConnection> = {}): DirectConnection {
  return {
    id: "homelab",
    name: "Homelab",
    protocol: "openai_compatible",
    normalizedBaseUrl: "https://homelab.example/v1",
    authKind: "bearer",
    authHeaderName: null,
    publicHeaders: {},
    manualModelIds: [],
    trustPrivateNetwork: false,
    credentialConfigured: true,
    credentialRevision: 1,
    configurationRevision: 1,
    createdAt: "2026-08-29T00:00:00Z",
    updatedAt: "2026-08-29T00:00:00Z",
    ...overrides,
  };
}

const group = {
  authority: "direct" as const,
  connectionId: "homelab",
  name: "Homelab",
  protocol: "openai_compatible" as const,
  state: "ready" as const,
  models: [
    {
      id: "model-a",
      object: "model",
      created: null,
      ownedBy: "test",
      root: null,
      parent: null,
      permission: null,
      capabilities: ["tools"],
      contextLength: 8192,
      maxInputTokens: null,
      maxOutputTokens: null,
      selectable: true,
      unavailableReason: null,
    },
  ],
};

beforeEach(() => {
  useConnectionProfilesStore.getState().reset();
  vi.clearAllMocks();
  vi.mocked(listInferenceConnections).mockResolvedValue([connection()]);
  vi.mocked(listInferenceCatalog).mockResolvedValue([group]);
  vi.mocked(probeInferenceConnection).mockResolvedValue(group);
});

afterEach(() => {
  cleanup();
  useConnectionProfilesStore.getState().reset();
});

describe("ConnectionSwitcher", () => {
  it("lists the active connection and switches with an authority-bound selection", async () => {
    const onSelectionChange = vi.fn();
    render(<ConnectionSwitcher config={config} onSelectionChange={onSelectionChange} />);

    expect(await screen.findByText("Homelab")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Choose connection profile" }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /Homelab/ }));

    expect(onSelectionChange).toHaveBeenCalledWith({
      schemaVersion: 1,
      authority: "direct",
      connectionId: "homelab",
      protocol: "openai_compatible",
      modelId: "model-a",
      routePolicyId: null,
    });
  });

  it("opens the add editor and keeps save disabled until name and URL are present", async () => {
    render(<ConnectionSwitcher config={config} />);
    await screen.findByText("Homelab");
    fireEvent.click(screen.getByRole("button", { name: "Choose connection profile" }));
    fireEvent.click(screen.getByText("Add connection"));

    const saveButton = screen.getByRole("button", { name: "Save connection" });
    expect(saveButton).toBeDisabled();
    fireEvent.change(screen.getByLabelText("Connection name"), { target: { value: "Spark" } });
    fireEvent.change(screen.getByLabelText("Connection URL"), {
      target: { value: "https://spark.example/v1" },
    });
    expect(saveButton).not.toBeDisabled();

    vi.mocked(createDirectConnection).mockResolvedValue(
      connection({ id: "spark", name: "Spark", normalizedBaseUrl: "https://spark.example/v1" }),
    );
    fireEvent.click(saveButton);
    await waitFor(() => expect(createDirectConnection).toHaveBeenCalledWith(
      config,
      expect.objectContaining({ name: "Spark", normalizedBaseUrl: "https://spark.example/v1" }),
    ));
  });

  it("probes a connection through the daemon connector", async () => {
    render(<ConnectionSwitcher config={config} />);
    await screen.findByText("Homelab");
    fireEvent.click(screen.getByRole("button", { name: "Choose connection profile" }));
    fireEvent.click(screen.getByRole("button", { name: "Probe Homelab" }));

    await waitFor(() => expect(probeInferenceConnection).toHaveBeenCalledWith(config, "direct", "homelab"));
  });

  it("keeps settings discoverable for a single connection", async () => {
    const onOpenConnectionSettings = vi.fn();
    render(<ConnectionSwitcher config={config} onOpenConnectionSettings={onOpenConnectionSettings} />);
    await screen.findByText("Homelab");
    fireEvent.click(screen.getByRole("button", { name: "Choose connection profile" }));
    fireEvent.click(screen.getByRole("button", { name: "Manage connections" }));

    expect(onOpenConnectionSettings).toHaveBeenCalledTimes(1);
  });
});
