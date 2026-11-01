import { act, renderHook } from "@testing-library/react";
import type React from "react";
import { describe, expect, it, vi } from "vitest";
import { useComposerEditorProjection } from "./useComposerEditorProjection";

describe("useComposerEditorProjection", () => {
  it("exposes selection and imperative insertion without owning submit", () => {
    const onChange = vi.fn();
    const { result } = renderHook(() => useComposerEditorProjection({ value: "hello", onChange }));
    const input = document.createElement("textarea");
    input.value = "hello";
    document.body.append(input);
    (result.current.textareaRef as { current: HTMLTextAreaElement | null }).current = input;

    act(() => result.current.handle.setSelection(5));
    act(() => result.current.handle.insertText(" world"));

    expect(result.current.handle.getValue()).toBe("hello world");
    expect(result.current.handle.getSelection()).toEqual({ start: 11, end: 11 });
    input.remove();
  });

  it("does not let an IME Enter reach the caller", () => {
    const onKeyDown = vi.fn();
    const { result } = renderHook(() => useComposerEditorProjection({ value: "", onChange: vi.fn(), onKeyDown }));
    const event: React.KeyboardEvent<HTMLTextAreaElement> = {
      altKey: false,
      bubbles: true,
      cancelable: true,
      charCode: 13,
      code: "Enter",
      ctrlKey: false,
      currentTarget: document.createElement("textarea"),
      defaultPrevented: false,
      detail: 0,
      eventPhase: 0,
      getModifierState: () => false,
      isDefaultPrevented: () => false,
      isPropagationStopped: () => false,
      isTrusted: true,
      key: "Enter",
      keyCode: 13,
      locale: "",
      location: 0,
      metaKey: false,
      nativeEvent: new KeyboardEvent("keydown", { key: "Enter", isComposing: true, keyCode: 229 }),
      persist: vi.fn(),
      preventDefault: vi.fn(),
      repeat: false,
      shiftKey: false,
      stopPropagation: vi.fn(),
      target: document.createElement("textarea"),
      timeStamp: 0,
      type: "keydown",
      view: { document, styleMedia: { type: "", matchMedium: () => false } },
      which: 13,
    };
    act(() => result.current.handlers.onKeyDown(event));
    expect(onKeyDown).not.toHaveBeenCalled();
    expect(event.preventDefault).not.toHaveBeenCalled();
  });
});
