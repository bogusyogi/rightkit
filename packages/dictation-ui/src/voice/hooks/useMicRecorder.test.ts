import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, renderHook } from "@testing-library/react";
import { defaultMicErrorCopy, useMicRecorder } from "./useMicRecorder";

const copy = defaultMicErrorCopy();

describe("useMicRecorder", () => {
  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it("reports unsupported recording before requesting permission", async () => {
    vi.stubGlobal("MediaRecorder", undefined);
    const request = vi.fn(() => true);
    const { result } = renderHook(() => useMicRecorder(copy, request));

    await expect(result.current.handle.start()).rejects.toThrow(copy.microphoneUnsupported);
    expect(request).not.toHaveBeenCalled();
  });

  it("honors the narrow permission adapter and does not open a denied microphone", async () => {
    vi.stubGlobal("MediaRecorder", class {});
    const request = vi.fn(() => false);
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: { getUserMedia: vi.fn() },
    });
    const { result } = renderHook(() => useMicRecorder(copy, request));

    await expect(result.current.handle.start()).rejects.toThrow(copy.microphoneAccessDenied);
    expect(request).toHaveBeenCalledTimes(1);
  });

  it("cancels an open capture and resets the visible meter state", () => {
    const { result } = renderHook(() => useMicRecorder(copy));
    act(() => result.current.handle.cancel());
    expect(result.current.recording).toBe(false);
    expect(result.current.level).toBe(0);
  });
});
