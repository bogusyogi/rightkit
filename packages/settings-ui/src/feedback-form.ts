// State of an inline feedback form: closed, typing, sending, sent, failed. The host passes the send call and every word;
// the markup is the host's. Imports only `react`.
import { useCallback, useState } from "react";

export type FeedbackStatus = "idle" | "sending" | "sent" | "error";

export interface UseFeedbackFormOptions {
  /**
   * Sends the trimmed, non-empty note (the host's command). Resolves with the report id, or a falsy value when the
   * endpoint returns none.
   */
  submit(note: string): Promise<string | null | undefined>;
  /** Length cap for the text field, returned as `maxLength` for the host to put on its input. Default 4000. */
  maxChars?: number;
  /** Error text when the rejection is neither a string nor an `Error`. Default `"send failed"`. */
  fallbackError?: string;
}

export interface FeedbackForm {
  /** The form is expanded. */
  open: boolean;
  setOpen(open: boolean): void;
  message: string;
  setMessage(message: string): void;
  /** Id the endpoint returned for the sent note; null when none. */
  reportId: string | null;
  status: FeedbackStatus;
  /** Text of the last failure; null otherwise. */
  error: string | null;
  /** The cap to put on the text field. */
  maxLength: number;
  /** A note is typed and none is in flight: enables the send button. */
  canSubmit: boolean;
  /** Sends the note. Does nothing for an empty (after trim) note or while one is in flight. */
  submit(): Promise<void>;
  /** Closes the form and clears the note, id, status and error. */
  reset(): void;
}

/** A rejection as text: a string as is, an `Error`'s message, else `fallback`. */
export function feedbackErrorText(err: unknown, fallback = "send failed"): string {
  return typeof err === "string" ? err : err instanceof Error ? err.message : fallback;
}

/**
 * The state machine behind a "send feedback" card. `submit` trims the note, refuses an empty one and a second send while
 * one is in flight, and ends in `sent` (with the report id) or `error` (with the text). `reset` returns to a closed, empty form.
 */
export function useFeedbackForm(options: UseFeedbackFormOptions): FeedbackForm {
  const { submit: send, maxChars = 4000, fallbackError = "send failed" } = options;
  const [open, setOpen] = useState(false);
  const [message, setMessage] = useState("");
  const [reportId, setReportId] = useState<string | null>(null);
  const [status, setStatus] = useState<FeedbackStatus>("idle");
  const [error, setError] = useState<string | null>(null);

  const reset = useCallback(() => {
    setOpen(false);
    setMessage("");
    setReportId(null);
    setStatus("idle");
    setError(null);
  }, []);

  const submit = async () => {
    const note = message.trim();
    if (!note || status === "sending") return;
    setStatus("sending");
    setError(null);
    try {
      const id = await send(note);
      setReportId(id || null);
      setStatus("sent");
    } catch (err) {
      setError(feedbackErrorText(err, fallbackError));
      setStatus("error");
    }
  };

  return {
    open,
    setOpen,
    message,
    setMessage,
    reportId,
    status,
    error,
    maxLength: maxChars,
    canSubmit: message.trim().length > 0 && status !== "sending",
    submit,
    reset,
  };
}
