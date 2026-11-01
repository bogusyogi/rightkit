import { useEffect, useRef, useState } from "react";

interface RunAnnouncerProps {
  /** Whole transcript; only completed assistant turns are announced. */
  messages: { $kind?: string; content?: string }[];
  /** True while a turn is in flight. */
  isWorking: boolean;
}

const MAX_SPOKEN_CHARS = 400;

/**
 * Speaks run progress to assistive tech.
 *
 * The transcript renders new messages into the DOM without any live region, so
 * a screen-reader user got silence: no indication the agent had started, and no
 * announcement when the answer arrived. They had to poll the pane manually.
 *
 * Only *completed* assistant turns are announced — streaming token-by-token
 * would be an unusable firehose — and long replies are truncated, since the
 * full text remains readable in the transcript itself.
 */
export default function RunAnnouncer({ messages, isWorking }: RunAnnouncerProps) {
  const [announcement, setAnnouncement] = useState("");
  const lastSpokenRef = useRef<string | null>(null);
  const wasWorkingRef = useRef(false);

  useEffect(() => {
    if (isWorking && !wasWorkingRef.current) {
      setAnnouncement("Working…");
    }
    wasWorkingRef.current = isWorking;
  }, [isWorking]);

  useEffect(() => {
    if (isWorking) return;
    const lastAssistant = [...messages]
      .reverse()
      .find((m) => m.$kind === "assistant" && (m.content ?? "").trim().length > 0);
    if (!lastAssistant) return;

    const text = (lastAssistant.content ?? "").trim();
    if (text === lastSpokenRef.current) return;
    lastSpokenRef.current = text;

    const spoken =
      text.length > MAX_SPOKEN_CHARS
        ? `${text.slice(0, MAX_SPOKEN_CHARS)}… response continues in the transcript`
        : text;
    setAnnouncement(`Assistant replied. ${spoken}`);
  }, [messages, isWorking]);

  return (
    <div aria-live="polite" aria-atomic="true" className="sr-only">
      {announcement}
    </div>
  );
}
