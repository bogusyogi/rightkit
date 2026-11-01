import { useCallback, useLayoutEffect, useRef, useState } from "react";
import type React from "react";
import {
  composerMaxHeight,
  growComposerTextarea,
  insertedTextBetween,
  isComposerComposing,
  readComposerSelection,
  writeComposerValue,
  type ComposerChange,
  type ComposerSelection,
} from "@rightkit/agent-ui-core/lib/composer/editorProjection";

export interface ComposerEditorHandle {
  focus(options?: FocusOptions): void;
  blur(): void;
  isFocused(): boolean;
  getValue(): string;
  getSelection(): ComposerSelection;
  setSelection(start: number, end?: number): void;
  selectAll(): void;
  insertText(text: string): void;
  replaceRange(from: number, to: number, text: string, caret?: number): void;
  /** Native textareas cannot provide CodeMirror viewport coordinates. */
  caretCoords(position?: number): { top: number; bottom: number; left: number } | null;
  getScrollDOM(): HTMLElement | null;
}

export interface UseComposerEditorProjectionOptions {
  value: string;
  onChange: (change: ComposerChange) => void;
  onSelectionChange?: (selection: ComposerSelection) => void;
  onKeyDown?: (event: React.KeyboardEvent<HTMLTextAreaElement>) => boolean | void;
  onFocus?: () => void;
  onBlur?: () => void;
  onPaste?: (event: React.ClipboardEvent<HTMLTextAreaElement>) => void;
  maxRows?: number;
  lineHeightPx?: number;
}

export interface ComposerEditorProjection {
  textareaRef: React.RefObject<HTMLTextAreaElement | null>;
  rows: number;
  maxHeight: string;
  handlers: {
    onChange: (event: React.ChangeEvent<HTMLTextAreaElement>) => void;
    onSelect: () => void;
    onKeyDown: (event: React.KeyboardEvent<HTMLTextAreaElement>) => void;
    onPaste: (event: React.ClipboardEvent<HTMLTextAreaElement>) => void;
    onCompositionStart: () => void;
    onCompositionEnd: () => void;
    onFocus: () => void;
    onBlur: () => void;
  };
  handle: ComposerEditorHandle;
}

/**
 * React seam for OpenChamber's controlled ComposerEditor. It deliberately
 * keeps the value in the caller's store while writing external changes
 * imperatively, which preserves the donor's composition guard and caret.
 */
export function useComposerEditorProjection({
  value,
  onChange,
  onSelectionChange,
  onKeyDown,
  onFocus,
  onBlur,
  onPaste,
  maxRows = 8,
  lineHeightPx = 22,
}: UseComposerEditorProjectionOptions): ComposerEditorProjection {
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const composingRef = useRef(false);
  const previousValueRef = useRef(value);
  const handlersRef = useRef({ onChange, onSelectionChange, onKeyDown, onFocus, onBlur, onPaste });
  handlersRef.current = { onChange, onSelectionChange, onKeyDown, onFocus, onBlur, onPaste };
  const [rows, setRows] = useState(1);

  useLayoutEffect(() => {
    const input = textareaRef.current;
    if (!input) return;
    writeComposerValue(input, value, composingRef.current);
    // During composition the parent value is intentionally stale; retaining
    // it here would make the next input look like an unrelated replacement.
    if (!composingRef.current) previousValueRef.current = value;
    setRows(growComposerTextarea(input, value, maxRows, lineHeightPx));
  }, [lineHeightPx, maxRows, value]);

  const handleChange = useCallback((event: React.ChangeEvent<HTMLTextAreaElement>) => {
    const input = event.currentTarget;
    const next = input.value;
    const change: ComposerChange = {
      value: next,
      selection: readComposerSelection(input),
      fromPaste: Boolean((event.nativeEvent as InputEvent).inputType?.includes("insertFromPaste")),
      insertedText: insertedTextBetween(previousValueRef.current, next),
    };
    previousValueRef.current = next;
    handlersRef.current.onChange(change);
    setRows(growComposerTextarea(input, next, maxRows, lineHeightPx));
  }, [lineHeightPx, maxRows]);

  const handleKeyDown = useCallback((event: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === "Enter" && isComposerComposing(event.nativeEvent)) return;
    if (handlersRef.current.onKeyDown?.(event)) event.preventDefault();
  }, []);

  const handleCompositionStart = useCallback(() => {
    composingRef.current = true;
  }, []);

  // biome-ignore lint/correctness/useExhaustiveDependencies: controlled value changes refresh IME reconciliation callback.
  const handleCompositionEnd = useCallback(() => {
    composingRef.current = false;
    const input = textareaRef.current;
    if (!input) return;
    // The final IME value is already in the native control. Do not overwrite
    // it with a stale parent echo; the next value change performs normal
    // controlled reconciliation.
    previousValueRef.current = input.value;
    setRows(growComposerTextarea(input, input.value, maxRows, lineHeightPx));
  }, [lineHeightPx, maxRows, value]);

  const handle = useImperativeHandleValue(textareaRef);

  return {
    textareaRef,
    rows,
    maxHeight: composerMaxHeight(maxRows, lineHeightPx),
    handlers: {
      onChange: handleChange,
      onSelect: () => {
        const input = textareaRef.current;
        if (input) handlersRef.current.onSelectionChange?.(readComposerSelection(input));
      },
      onKeyDown: handleKeyDown,
      onPaste: (event) => handlersRef.current.onPaste?.(event),
      onCompositionStart: handleCompositionStart,
      onCompositionEnd: handleCompositionEnd,
      onFocus: () => handlersRef.current.onFocus?.(),
      onBlur: () => handlersRef.current.onBlur?.(),
    },
    handle,
  };
}

function useImperativeHandleValue(
  textareaRef: React.RefObject<HTMLTextAreaElement | null>,
): ComposerEditorHandle {
  const handleRef = useRef<ComposerEditorHandle | null>(null);
  if (!handleRef.current) {
    handleRef.current = {
      focus(options) {
        textareaRef.current?.focus(options);
      },
      blur() {
        textareaRef.current?.blur();
      },
      isFocused() {
        return Boolean(textareaRef.current && document.activeElement === textareaRef.current);
      },
      getValue() {
        return textareaRef.current?.value ?? "";
      },
      getSelection() {
        const input = textareaRef.current;
        return input ? readComposerSelection(input) : { start: 0, end: 0 };
      },
      setSelection(start, end = start) {
        const input = textareaRef.current;
        if (!input) return;
        const max = input.value.length;
        input.setSelectionRange(Math.max(0, Math.min(start, max)), Math.max(0, Math.min(end, max)));
      },
      selectAll() {
        textareaRef.current?.select();
      },
      insertText(text) {
        const input = textareaRef.current;
        if (!input || !text) return;
        const start = input.selectionStart ?? input.value.length;
        const end = input.selectionEnd ?? start;
        input.setRangeText(text, start, end, "end");
        input.dispatchEvent(new Event("input", { bubbles: true }));
      },
      replaceRange(from, to, text, caret) {
        const input = textareaRef.current;
        if (!input) return;
        input.setRangeText(text, from, to, "preserve");
        const nextCaret = caret ?? from + text.length;
        input.setSelectionRange(nextCaret, nextCaret);
        input.dispatchEvent(new Event("input", { bubbles: true }));
      },
      caretCoords() {
        return null;
      },
      getScrollDOM() {
        return textareaRef.current;
      },
    };
  }
  return handleRef.current;
}
