import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, test, vi } from "vitest";
import { RecoveryScreen } from "./RecoveryScreen";

describe("RecoveryScreen", () => {
  test("shows retry and both choices for an unreachable remote", () => {
    render(<RecoveryScreen variant="remote-unreachable" hostLabel="Build host" onRetry={vi.fn()} onChooseRemote={vi.fn()} />);
    expect(screen.getByTestId("connection-recovery")).toBeTruthy();
    expect(screen.getByRole("button", { name: /retry connection/i })).toBeTruthy();
    expect(screen.getByRole("button", { name: /use local/i })).toBeTruthy();
    expect(screen.getByRole("button", { name: /use remote/i })).toBeTruthy();
  });

  test("routes remote choice through the supplied callback", () => {
    const onChooseRemote = vi.fn();
    render(<RecoveryScreen variant="remote-wrong-service" onChooseRemote={onChooseRemote} />);
    fireEvent.click(screen.getByRole("button", { name: /use remote/i }));
    expect(onChooseRemote).toHaveBeenCalledOnce();
  });

  test("uses the local setup callback for an unavailable local engine", () => {
    const onEnterLocalSetup = vi.fn();
    render(<RecoveryScreen variant="local-unavailable" onEnterLocalSetup={onEnterLocalSetup} />);
    fireEvent.click(screen.getByRole("button", { name: /set up local/i }));
    expect(onEnterLocalSetup).toHaveBeenCalledOnce();
  });

  test("shows an explicit remote form only after the remote choice", () => {
    const onSubmitRemote = vi.fn();
    render(<RecoveryScreen variant="remote-missing" showRemoteForm onSubmitRemote={onSubmitRemote} />);
    fireEvent.change(screen.getByLabelText(/server address/i), { target: { value: "https://daemon.example" } });
    fireEvent.submit(screen.getByRole("form", { name: /remote connection/i }));
    expect(onSubmitRemote).toHaveBeenCalledWith("https://daemon.example", "");
  });
});
