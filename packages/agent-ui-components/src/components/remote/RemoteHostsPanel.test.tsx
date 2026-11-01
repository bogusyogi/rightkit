// @vitest-environment happy-dom
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import RemoteHostsPanel from "@rightkit/agent-ui-settings/components/remote/RemoteHostsPanel";
import type { RemoteClient } from "@rightkit/agent-ui-core/lib/remote/remoteClient";

function client(): RemoteClient {
  return {
    remoteStatus: vi.fn(),
    remotePairingBegin: vi.fn(),
    remotePairingCancel: vi.fn(),
    remoteDevicesList: vi.fn(),
    remoteDeviceRevoke: vi.fn(),
    remoteDeviceRename: vi.fn(),
    remoteHostsList: vi.fn().mockResolvedValue([
      {
        id: "remote:fp",
        hostId: "host:bender",
        hostname: "bender.local",
        displayName: "Bender",
        fingerprint: "f".repeat(64),
        candidates: ["bender.local:48213", "192.168.1.10:48213"],
        createdAt: 0,
        lastSeenAt: Date.now(),
      },
    ]),
    remoteHostPair: vi.fn().mockResolvedValue({}),
    remoteHostProjectSnapshot: vi.fn().mockResolvedValue({
      hostId: "remote:fp",
      projects: [{ id: "p1" }],
      icons: {},
    }),
    remoteHostForget: vi.fn().mockResolvedValue(undefined),
    remoteHostRename: vi.fn().mockResolvedValue(undefined),
    remoteHostReconnect: vi.fn().mockResolvedValue(undefined),
    remoteHostInvoke: vi.fn(),
  };
}

describe("RemoteHostsPanel", () => {
  it("projects paired host identity, candidates, and last-seen state", async () => {
    render(<RemoteHostsPanel client={client()} />);

    await waitFor(() => expect(screen.getByTestId("remote-host-remote:fp")).toBeTruthy());
    expect(screen.getByTestId("remote-host-remote:fp").textContent).toContain("Bender");
    expect(screen.getByTestId("remote-host-remote:fp").textContent).toContain("bender.local:48213");
    expect(screen.getByTestId("remote-host-remote:fp").textContent).toContain("not checked");
  });

  it("pairs a host with the donor-shaped address, fingerprint, code, and candidates", async () => {
    const remote = client();
    render(<RemoteHostsPanel client={remote} />);

    fireEvent.change(screen.getByRole("textbox", { name: "Remote host address" }), {
      target: { value: "bender.local:48213" },
    });
    fireEvent.change(screen.getByRole("textbox", { name: "Remote host fingerprint" }), {
      target: { value: "f".repeat(64) },
    });
    fireEvent.change(screen.getByRole("textbox", { name: "Remote host pairing code" }), {
      target: { value: "12345678" },
    });
    fireEvent.change(screen.getByRole("textbox", { name: "Remote host candidate addresses" }), {
      target: { value: "192.168.1.10:48213, bender.local:48213" },
    });
    fireEvent.submit(screen.getByTestId("remote-host-pair-form"));

    await waitFor(() => expect(remote.remoteHostPair).toHaveBeenCalledWith({
      host: "bender.local:48213",
      fingerprint: "f".repeat(64),
      code: "12345678",
      candidates: ["192.168.1.10:48213", "bender.local:48213"],
    }));
  });

  it("checks reachability through the project snapshot instead of inventing a ping", async () => {
    const remote = client();
    render(<RemoteHostsPanel client={remote} />);

    await waitFor(() => screen.getByTestId("remote-host-remote:fp"));
    fireEvent.click(screen.getByRole("button", { name: "Check reachability" }));

    await waitFor(() => expect(remote.remoteHostProjectSnapshot).toHaveBeenCalledWith("remote:fp"));
    expect(screen.getByTestId("remote-host-remote:fp").textContent).toContain("reachable");
    expect(screen.getByTestId("remote-host-remote:fp").textContent).toContain("1 project");
  });

  it("routes reconnect, rename, and forget through the paired-host client", async () => {
    const remote = client();
    render(<RemoteHostsPanel client={remote} />);

    await waitFor(() => screen.getByTestId("remote-host-remote:fp"));
    fireEvent.click(screen.getByRole("button", { name: "Reconnect" }));
    await waitFor(() => expect(remote.remoteHostReconnect).toHaveBeenCalledWith("remote:fp"));

    fireEvent.click(screen.getByRole("button", { name: "Rename" }));
    fireEvent.change(screen.getByRole("textbox", { name: "New name for Bender" }), {
      target: { value: "Bender workstation" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save name" }));
    await waitFor(() => expect(remote.remoteHostRename).toHaveBeenCalledWith("remote:fp", "Bender workstation"));

    fireEvent.click(screen.getByRole("button", { name: "Forget" }));
    await waitFor(() => expect(remote.remoteHostForget).toHaveBeenCalledWith("remote:fp"));
  });
});
