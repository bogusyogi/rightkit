import { forwardRef, useImperativeHandle } from "react";
import type React from "react";
import {
  useComposerEditorProjection,
  type ComposerEditorHandle,
} from "@rightkit/agent-ui-hooks/hooks/useComposerEditorProjection";
import type {
  ComposerChange,
  ComposerSelection,
} from "@rightkit/agent-ui-core/lib/composer/editorProjection";

export interface RichComposerEditorProps {
  value: string;
  onChange: (change: ComposerChange) => void;
  onSelectionChange?: (selection: ComposerSelection) => void;
  /** Return true to consume the key, as in OpenChamber's editor contract. */
  onKeyDown?: (event: React.KeyboardEvent<HTMLTextAreaElement>) => boolean | undefined;
  onFocus?: () => void;
  onBlur?: () => void;
  onPaste?: (event: React.ClipboardEvent<HTMLTextAreaElement>) => void;
  placeholder?: string;
  editable?: boolean;
  spellCheck?: boolean;
  autoCorrect?: boolean;
  autoCapitalize?: "none" | "sentences";
  fillContainer?: boolean;
  maxRows?: number;
  className?: string;
  contentClassName?: string;
  "aria-label"?: string;
  "data-testid"?: string;
}

/**
 * the host app native-textarea adaptation of OpenChamber's ComposerEditor.
 * Plain text remains the wire format; this atom adds the donor's caret API,
 * IME-safe controlled writeback, and grow-then-scroll behavior without taking
 * ownership of submission, mentions, or daemon state.
 */
export const RichComposerEditor = forwardRef<ComposerEditorHandle, RichComposerEditorProps>(
  function RichComposerEditor(
    {
      value,
      onChange,
      onSelectionChange,
      onKeyDown,
      onFocus,
      onBlur,
      onPaste,
      placeholder,
      editable = true,
      spellCheck = false,
      autoCorrect = false,
      autoCapitalize = "none",
      fillContainer = false,
      maxRows = 8,
      className,
      contentClassName,
      ...accessibility
    },
    ref,
  ) {
    const projection = useComposerEditorProjection({
      value,
      onChange,
      onSelectionChange,
      onKeyDown,
      onFocus,
      onBlur,
      onPaste,
      maxRows,
    });

    useImperativeHandle(ref, () => projection.handle, [projection.handle]);

    return (
      <textarea
        ref={projection.textareaRef}
        defaultValue={value}
        rows={projection.rows}
        readOnly={!editable}
        spellCheck={spellCheck}
        autoCorrect={autoCorrect ? "on" : "off"}
        autoCapitalize={autoCapitalize}
        placeholder={placeholder}
        aria-label={accessibility["aria-label"]}
        data-testid={accessibility["data-testid"]}
        onChange={projection.handlers.onChange}
        onSelect={projection.handlers.onSelect}
        onKeyDown={projection.handlers.onKeyDown}
        onPaste={projection.handlers.onPaste}
        onCompositionStart={projection.handlers.onCompositionStart}
        onCompositionEnd={projection.handlers.onCompositionEnd}
        onFocus={projection.handlers.onFocus}
        onBlur={projection.handlers.onBlur}
        className={["cr-rich-composer-editor", fillContainer ? "h-full min-h-0 flex-1" : "", className, contentClassName]
          .filter(Boolean)
          .join(" ")}
        style={{ maxHeight: projection.maxHeight, overflowY: projection.rows >= maxRows ? "auto" : "hidden" }}
      />
    );
  },
);

export type { ComposerEditorHandle } from "@rightkit/agent-ui-hooks/hooks/useComposerEditorProjection";
export default RichComposerEditor;
