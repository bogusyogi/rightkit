// @vitest-environment happy-dom
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import RemoteDevicesPanel from "@rightkit/agent-ui-settings/components/remote/RemoteDevicesPanel";
import type { RemoteClient } from "@rightkit/agent-ui-core/lib/remote/remoteClient";

function client(): RemoteClient {
  return {
    remoteStatus: vi.fn().mockResolvedValue({
      running: true,
      port: 4242,
      tlsActive: true,
      fingerprint: "ab".repeat(32),
      pairingActive: false,
      devices: 1,
    }),
    remotePairingBegin: vi.fn().mockResolvedValue({
      code: "12345678",
      expiresAt: Date.now() + 120_000,
      qrPayload: '{"v":1}',
    }),
    remotePairingCancel: vi.fn().mockResolvedValue(undefined),
    remoteDevicesList: vi.fn().mockResolvedValue([
      {
        id: "dev-1",
        name: "Ada's phone",
        platform: "ios",
        createdAt: 0,
        lastSeenAt: Date.now(),
        revoked: false,
        connected: true,
      },
    ]),
    remoteDeviceRevoke: vi.fn().mockResolvedValue(undefined),
    remoteDeviceRename: vi.fn().mockResolvedValue(undefined),
    remoteHostsList: vi.fn().mockResolvedValue([]),
    remoteHostPair: vi.fn(),
    remoteHostProjectSnapshot: vi.fn(),
    remoteHostForget: vi.fn(),
    remoteHostRename: vi.fn(),
    remoteHostReconnect: vi.fn(),
    remoteHostInvoke: vi.fn(),
  };
}

describe("RemoteDevicesPanel", () => {
  it("projects gateway status, certificate identity, and connected devices", async () => {
    render(<RemoteDevicesPanel client={client()} />);

    await waitFor(() => expect(screen.getByTestId("remote-device-dev-1")).toBeTruthy());
    expect(screen.getByTestId("remote-gateway-status").textContent).toContain("4242");
    expect(screen.getByTestId("remote-device-dev-1").textContent).toContain("connected");
    expect(screen.getByTestId("remote-device-dev-1").textContent).toContain("ios");
  });

  it("keeps pairing disabled until the gateway is secure and running", async () => {
    const remote = client();
    vi.mocked(remote.remoteStatus).mockResolvedValue({
      running: false,
      port: null,
      tlsActive: false,
      fingerprint: null,
      pairingActive: false,
      devices: 0,
    });
    vi.mocked(remote.remoteDevicesList).mockResolvedValue([]);
    render(<RemoteDevicesPanel client={remote} />);

    await waitFor(() => expect(screen.getByText(/Start the server before pairing/)).toBeTruthy());
    expect(screen.queryByRole("button", { name: "Pair a device" })).toBeNull();
  });

  it("starts and cancels pairing while displaying the one-time payload", async () => {
    const remote = client();
    render(<RemoteDevicesPanel client={remote} />);

    await waitFor(() => screen.getByRole("button", { name: "Pair a device" }));
    fireEvent.click(screen.getByRole("button", { name: "Pair a device" }));
    await waitFor(() => expect(screen.getByTestId("remote-pairing-code").textContent).toContain("12345678"));
    expect(screen.getByText('{"v":1}')).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Cancel pairing" }));
    await waitFor(() => expect(remote.remotePairingCancel).toHaveBeenCalledOnce());
  });

  it("renames and revokes a paired device through the typed client", async () => {
    const remote = client();
    render(<RemoteDevicesPanel client={remote} />);

    await waitFor(() => screen.getByTestId("remote-device-dev-1"));
    fireEvent.click(screen.getByRole("button", { name: "Rename" }));
    fireEvent.change(screen.getByRole("textbox", { name: "New name for Ada's phone" }), {
      target: { value: "Ada's iPad" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save name" }));
    await waitFor(() => expect(remote.remoteDeviceRename).toHaveBeenCalledWith("dev-1", "Ada's iPad"));

    fireEvent.click(screen.getByRole("button", { name: "Revoke" }));
    await waitFor(() => expect(remote.remoteDeviceRevoke).toHaveBeenCalledWith("dev-1"));
  });
});
