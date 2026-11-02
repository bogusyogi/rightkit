// Dictation — how your speech becomes the right text: one compact spoken-
// formatting line up top, then a segmented switch (Vocabulary | Snippets | Text
// replacements) showing one list at a time. Recording control (start/stop/send/
// cancel, incl. the zephyr triggers) lives in Controls; standalone action
// commands live in Commands.

import PageShell from "../components/PageShell.js";
import VocabularyPage from "./Vocabulary.js";

export default function DictationPage() {
  return (
    <PageShell tab="dictation" fixedHeight={false}>
      <div className="flex w-full min-w-0 flex-col gap-[var(--space-4)]">
        {/* Informational, always on. Neutral: ember is reserved for the active
            row, switches, primary buttons, and recording. */}
        <p
          data-setting-id="dictation.spoken-formatting"
          className="m-0 text-[length:var(--size-sm)] leading-[1.5] text-[var(--muted)]"
        >
          Spoken formatting is always on. Say “new line”, “question mark”, “hashtag” and 30+ more.{" "}
          <a
            href="https://heardright.app/formatting"
            target="_blank"
            rel="noreferrer"
            className="whitespace-nowrap text-[var(--accent-cool)] underline decoration-[color-mix(in_srgb,var(--accent-cool),transparent_60%)] underline-offset-2 hover:decoration-[var(--accent-cool)]"
          >
            See the full list →
          </a>
        </p>

        <VocabularyPage />
      </div>
    </PageShell>
  );
}
