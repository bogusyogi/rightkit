import { afterEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import {
  parseRemoteCandidates,
  remoteDeviceRename,
  remoteHostInvoke,
  remoteHostPair,
  remotePairingBegin,
  remoteStatus,
  remoteRelativeTime,
  shortRemoteFingerprint,
} from "./remoteClient";

afterEach(() => {
  invoke.mockReset();
});

describe("remote client", () => {
  it("reads status and keeps the status command payload-free", async () => {
    invoke.mockResolvedValueOnce({
      running: true,
      port: 4242,
      tlsActive: true,
      fingerprint: "ab".repeat(32),
      pairingActive: false,
      devices: 1,
    });

    await expect(remoteStatus()).resolves.toMatchObject({ port: 4242, tlsActive: true });
    expect(invoke).toHaveBeenCalledWith("remote_status");
  });

  it("starts pairing with the one-time code command", async () => {
    invoke.mockResolvedValueOnce({ code: "12345678", expiresAt: 1, qrPayload: "{}" });

    await expect(remotePairingBegin()).resolves.toMatchObject({ code: "12345678" });
    expect(invoke).toHaveBeenCalledWith("remote_pairing_begin");
  });

  it("uses id payloads for device mutations", async () => {
    invoke.mockResolvedValue(undefined);

    await remoteDeviceRename("dev-1", "James's iPhone");

    expect(invoke).toHaveBeenCalledWith("remote_device_rename", {
      id: "dev-1",
      name: "James's iPhone",
    });
  });

  it("wraps desktop host pairing and host commands", async () => {
    invoke.mockResolvedValueOnce({ id: "remote:fp", displayName: "Bender" });
    invoke.mockResolvedValueOnce({ ok: true });

    await remoteHostPair({
      host: "bender.local:48213",
      fingerprint: "f".repeat(64),
      code: "12345678",
      candidates: ["192.168.1.10:48213"],
    });
    await remoteHostInvoke("remote:fp", "git_status", { path: "/repo" });

    expect(invoke).toHaveBeenNthCalledWith(1, "remote_host_pair", {
      host: "bender.local:48213",
      fingerprint: "f".repeat(64),
      code: "12345678",
      candidates: ["192.168.1.10:48213"],
    });
    expect(invoke).toHaveBeenNthCalledWith(2, "remote_host_invoke", {
      id: "remote:fp",
      cmd: "git_status",
      args: { path: "/repo" },
    });
  });

  it("preserves the donor's candidate normalization and fingerprint display", () => {
    expect(parseRemoteCandidates("bender.local:1, 192.168.1.10:1\nbender.local:1")).toEqual([
      "bender.local:1",
      "192.168.1.10:1",
    ]);
    expect(shortRemoteFingerprint("abcdef0123456789deadbeef")).toBe("abcdef01…deadbeef");
    expect(remoteRelativeTime(0, 0)).toBe("just now");
    expect(remoteRelativeTime(0, 60_000)).toBe("1m ago");
  });
});
