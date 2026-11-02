import { afterEach, describe, expect, it, vi } from "vitest";
import { listCliProviders, mutateCliProvider } from "./cli-providers.js";

const config = { url: "http://127.0.0.1:5057", token: "test" };
const snapshot = {
  providers: [
    {
      providerId: "codex",
      displayName: "Codex CLI",
      installed: true,
      executable: "/usr/bin/codex",
      binaryVersion: "1",
      executableSha256: "digest",
      transport: "codex_app_server",
      auth: {
        mode: "native_cli_account",
        signedIn: true,
        detectedSignedIn: true,
        launchReady: true,
        source: "managed_cli",
        state: "configured",
        accountLabel: "default",
      },
      harness: {
        eligibility: "harness_verification_required",
        attestationId: null,
        isolationMode: "exclusive_coderight_gateway",
      },
      models: { available: true, items: [], source: "native" },
      repairActions: [],
    },
  ],
};

afterEach(() => vi.unstubAllGlobals());

describe("CLI provider daemon client", () => {
  it("fails closed on an invalid discovery body", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(new Response(JSON.stringify({ nope: true }), { status: 200 })),
    );
    await expect(listCliProviders(config)).rejects.toThrow("invalid provider snapshot");
  });

  it.each([
    [
      "provider auth",
      {
        ...snapshot,
        providers: [
          { ...snapshot.providers[0], auth: { ...snapshot.providers[0].auth, launchReady: "yes" } },
        ],
      },
    ],
  ])("rejects malformed %s v3 fields", async (_field, body) => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(JSON.stringify(body), { status: 200 })));
    await expect(listCliProviders(config)).rejects.toThrow("invalid provider snapshot");
  });

  it("refreshes live state after a mutation acknowledgement", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(new Response(JSON.stringify({ accepted: true }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(snapshot), { status: 200 }));
    vi.stubGlobal("fetch", fetch);
    await expect(mutateCliProvider(config, "codex", "verify-harness")).resolves.toEqual(snapshot);
    expect(fetch.mock.calls[0][0].toString()).toContain("/cli-providers/codex/verify-harness");
  });
});
