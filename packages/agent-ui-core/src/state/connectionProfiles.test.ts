import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  createDirectConnection,
  deleteDirectConnection,
  listInferenceCatalog,
  listInferenceConnections,
  probeInferenceConnection,
  setDirectConnectionSecret,
  updateDirectConnection,
} from "../lib/daemon/inference";
import type { DirectConnection } from "../lib/types/inference";
import { useConnectionProfilesStore } from "./connectionProfiles";

vi.mock("../lib/daemon/inference", () => ({
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
    id: "local",
    name: "This device",
    protocol: "openai_compatible",
    normalizedBaseUrl: "http://127.0.0.1:11434",
    authKind: "none",
    authHeaderName: null,
    publicHeaders: {},
    manualModelIds: [],
    trustPrivateNetwork: false,
    credentialConfigured: false,
    credentialRevision: 0,
    configurationRevision: 1,
    createdAt: "2026-08-29T00:00:00Z",
    updatedAt: "2026-08-29T00:00:00Z",
    ...overrides,
  };
}

const localGroup = {
  authority: "direct" as const,
  connectionId: "local",
  name: "This device",
  protocol: "openai_compatible" as const,
  state: "ready" as const,
  models: [
    {
      id: "local-model",
      object: "model",
      created: null,
      ownedBy: "local",
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
  vi.mocked(listInferenceCatalog).mockResolvedValue([localGroup]);
});

describe("connection profile projection", () => {
  it("keeps local and remote provider profiles distinct", async () => {
    vi.mocked(listInferenceConnections).mockResolvedValue([
      connection(),
      connection({
        id: "homelab",
        name: "Homelab",
        normalizedBaseUrl: "https://homelab.example/v1",
        credentialConfigured: true,
      }),
    ]);
    vi.mocked(listInferenceCatalog).mockResolvedValue([
      localGroup,
      {
        ...localGroup,
        connectionId: "homelab",
        name: "Homelab",
        state: "ready",
      },
      {
        authority: "direct",
        connectionId: "cloud",
        name: "App Cloud",
        protocol: "openai_compatible",
        state: "ready",
        models: [],
      },
    ]);

    await useConnectionProfilesStore.getState().load(config);
    expect(useConnectionProfilesStore.getState().profiles.map((item) => item.kind)).toEqual([
      "local",
      "remote",
      "remote",
    ]);
  });

  it("switches a profile to its first selectable model with an authority-bound selection", async () => {
    await useConnectionProfilesStore.getState().load(config);

    const selection = useConnectionProfilesStore.getState().selectProfile("direct:local");
    expect(selection).toEqual({
      schemaVersion: 1,
      authority: "direct",
      connectionId: "local",
      protocol: "openai_compatible",
      modelId: "local-model",
      routePolicyId: null,
    });
    expect(useConnectionProfilesStore.getState().activeSelection).toEqual(selection);
  });

  it("rejects a stale catalog response after a newer profile load", async () => {
    let resolveOld: (groups: typeof localGroup[]) => void = () => undefined;
    vi.mocked(listInferenceCatalog)
      .mockImplementationOnce(() => new Promise((resolve) => (resolveOld = resolve)))
      .mockResolvedValueOnce([{ ...localGroup, connectionId: "new", name: "New" }]);

    const oldLoad = useConnectionProfilesStore.getState().load(config);
    await useConnectionProfilesStore.getState().load(config);
    resolveOld([localGroup]);
    await oldLoad;

    expect(useConnectionProfilesStore.getState().profiles).toEqual(
      expect.arrayContaining([expect.objectContaining({ connectionId: "new" })]),
    );
  });
});

describe("connection profile management", () => {
  it("stores a new connection secret only through the daemon secret seam", async () => {
    const saved = connection({ id: "spark", name: "Spark", normalizedBaseUrl: "https://spark.example" });
    vi.mocked(createDirectConnection).mockResolvedValue(saved);
    vi.mocked(setDirectConnectionSecret).mockResolvedValue({
      credentialConfigured: true,
      credentialRevision: 1,
    });

    const ok = await useConnectionProfilesStore.getState().save(
      config,
      {
        name: "Spark",
        normalizedBaseUrl: "https://spark.example",
        protocol: "openai_compatible",
        authKind: "bearer",
        authHeaderName: null,
        publicHeaders: {},
        manualModelIds: [],
        trustPrivateNetwork: false,
      },
      "secret-value",
    );

    expect(ok).toBe(true);
    expect(setDirectConnectionSecret).toHaveBeenCalledWith(config, "spark", "secret-value");
    expect(vi.mocked(updateDirectConnection)).not.toHaveBeenCalled();
  });

  it("probes and replaces the current catalog group without losing the profile", async () => {
    await useConnectionProfilesStore.getState().load(config);
    vi.mocked(probeInferenceConnection).mockResolvedValue({ ...localGroup, state: "ready" });

    expect(await useConnectionProfilesStore.getState().probe(config, "direct:local")).toBe(true);
    expect(useConnectionProfilesStore.getState().profiles[0]?.models).toHaveLength(1);
    expect(probeInferenceConnection).toHaveBeenCalledWith(config, "direct", "local");
  });

  it("deletes only direct profiles", async () => {
    await useConnectionProfilesStore.getState().load(config);
    vi.mocked(deleteDirectConnection).mockResolvedValue(undefined);

    expect(await useConnectionProfilesStore.getState().remove(config, "direct:local")).toBe(true);
    expect(deleteDirectConnection).toHaveBeenCalledWith(config, "local");
    expect(await useConnectionProfilesStore.getState().remove(config, "missing")).toBe(false);
  });
});
