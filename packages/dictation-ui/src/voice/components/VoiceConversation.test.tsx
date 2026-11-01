import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import VoiceConversation from "./VoiceConversation.js";

const controller = {
  end: vi.fn(async () => undefined),
  error: null,
  level: 0.5,
  muted: false,
  start: vi.fn(async () => undefined),
  status: "idle" as const,
  stopTurn: vi.fn(),
  toggleMute: vi.fn(),
};

vi.mock("../hooks/useVoiceConversation.js", () => ({
  useVoiceConversation: () => controller,
}));

describe("VoiceConversation", () => {
  it("exposes an accessible conversation toggle and primary action", () => {
    render(
      <VoiceConversation
        busy={false}
        consumePendingResponse={vi.fn()}
        onSubmit={vi.fn()}
        onTranscribeAudio={vi.fn(async () => "hello")}
        pendingResponse={() => null}
      />,
    );

    expect(screen.getByTestId("voice-conversation")).toHaveAttribute("data-status", "idle");
    expect(screen.getByRole("button", { name: "Start voice conversation" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Start voice turn" })).toBeDisabled();
  });

  it("starts the controlled conversation and renders the live microphone level", () => {
    render(
      <VoiceConversation
        busy={false}
        consumePendingResponse={vi.fn()}
        initialEnabled
        onSubmit={vi.fn()}
        onTranscribeAudio={vi.fn(async () => "hello")}
        pendingResponse={() => null}
      />,
    );

    expect(screen.getByRole("progressbar", { name: "Microphone level" })).toHaveAttribute(
      "aria-valuenow",
      "50",
    );
    fireEvent.click(screen.getByRole("button", { name: "Start voice turn" }));
    expect(controller.start).toHaveBeenCalled();
  });

  it("keeps the mute control explicit and stateful", () => {
    render(
      <VoiceConversation
        busy={false}
        consumePendingResponse={vi.fn()}
        initialEnabled
        onSubmit={vi.fn()}
        onTranscribeAudio={vi.fn(async () => "hello")}
        pendingResponse={() => null}
      />,
    );

    const mute = screen.getByRole("button", { name: "Mute microphone" });
    fireEvent.click(mute);
    expect(controller.toggleMute).toHaveBeenCalledTimes(1);
  });
});
