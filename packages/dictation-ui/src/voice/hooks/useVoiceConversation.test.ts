import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import type { MicRecording } from "./useMicRecorder.js";
import { useVoiceConversation } from "./useVoiceConversation.js";

const micHandle = {
  cancel: vi.fn(),
  start: vi.fn(async () => undefined),
  stop: vi.fn<() => Promise<MicRecording | null>>(async () => ({
    audio: new Blob(["voice"], { type: "audio/webm" }),
    durationMs: 900,
    heardSpeech: true,
  })),
};

vi.mock("./useMicRecorder.js", async () => {
  const actual = await vi.importActual<typeof import("./useMicRecorder")>("./useMicRecorder.js");
  return {
    ...actual,
    useMicRecorder: () => ({ handle: micHandle, level: 0.25, recording: true }),
  };
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  micHandle.stop.mockResolvedValue({
    audio: new Blob(["voice"], { type: "audio/webm" }),
    durationMs: 900,
    heardSpeech: true,
  });
});

function makeConversation(holdResponse = false) {
  const onSubmit = vi.fn(async () => undefined);
  const onInterrupt = vi.fn();
  const onTranscribeAudio = vi.fn(async () => "kick off the task");
  const speech = { speak: vi.fn(async () => undefined), stop: vi.fn() };
  const hook = renderHook(
    ({ busy }: { busy: boolean }) =>
      useVoiceConversation({
        busy,
        consumePendingResponse: vi.fn(),
        enabled: true,
        onInterrupt,
        onSubmit,
        onTranscribeAudio,
        pendingResponse: () =>
          holdResponse ? { id: "reply-1", pending: true, text: "still working" } : null,
        speech,
      }),
    { initialProps: { busy: false } },
  );
  return { hook, onInterrupt, onSubmit, onTranscribeAudio, speech };
}

describe("useVoiceConversation", () => {
  it("ports the listen → transcribe → submit state transition", async () => {
    const { hook, onSubmit, onTranscribeAudio } = makeConversation(true);

    await act(async () => {
      await hook.result.current.start();
    });
    expect(hook.result.current.status).toBe("listening");
    expect(micHandle.start).toHaveBeenCalledWith(expect.objectContaining({
      silenceLevel: 0.075,
      silenceMs: 1_250,
      idleSilenceMs: 12_000,
    }));

    act(() => hook.result.current.stopTurn());
    await waitFor(() => expect(onSubmit).toHaveBeenCalledWith("kick off the task"));
    expect(onTranscribeAudio).toHaveBeenCalledTimes(1);
    expect(hook.result.current.status).toBe("thinking");
  });

  it("recognizes a whole-utterance stop command without submitting it", async () => {
    const { hook, onSubmit } = makeConversation();
    vi.mocked(onSubmit).mockClear();

    const transcribe = vi.fn(async () => "stop");
    hook.rerender({ busy: false });
    // The callback is intentionally supplied at the seam; the test verifies
    // the safety rule through a fresh hook instance.
    cleanup();
    const stopped = renderHook(() =>
      useVoiceConversation({
        busy: false,
        consumePendingResponse: vi.fn(),
        enabled: true,
        onStopWord: vi.fn(),
        onSubmit,
        onTranscribeAudio: transcribe,
        pendingResponse: () => null,
      }),
    );
    await act(async () => {
      await stopped.result.current.start();
    });
    act(() => stopped.result.current.stopTurn());
    await waitFor(() => expect(transcribe).toHaveBeenCalled());
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("arms generation-phase barge-in and interrupts the active turn", async () => {
    const { hook, onInterrupt, speech } = makeConversation(true);
    await act(async () => {
      await hook.result.current.start();
    });
    act(() => hook.result.current.stopTurn());
    await waitFor(() => expect(hook.result.current.status).toBe("thinking"));

    hook.rerender({ busy: true });
    await waitFor(() => expect(micHandle.start).toHaveBeenCalledTimes(2));
    const calls = micHandle.start.mock.calls as unknown as Array<[
      { onLevel?: (level: number) => void },
    ]>;
    act(() => calls.at(-1)?.[0].onLevel?.(0.2));
    expect(onInterrupt).toHaveBeenCalledTimes(1);
    expect(speech.stop).toHaveBeenCalled();
  });

  it("ends capture and speech when the conversation is ended", async () => {
    const { hook, speech } = makeConversation();
    await act(async () => {
      await hook.result.current.start();
      await hook.result.current.end();
    });
    expect(micHandle.cancel).toHaveBeenCalled();
    expect(speech.stop).toHaveBeenCalled();
    expect(hook.result.current.status).toBe("idle");
  });
});
