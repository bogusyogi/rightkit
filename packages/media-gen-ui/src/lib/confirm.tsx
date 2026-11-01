import { useCallback } from 'react';
import { useShell } from '@rightkit/react-shell/react';

export interface ConfirmOptions {
  title: string;
  /** Paragraphs shown under the title. */
  paragraphs: string[];
  /** Optional line naming what the action touches, such as file names. */
  subject?: string;
  /** Small trailing note: what the approval covers, or that the action cannot be undone. */
  note?: string;
  confirmLabel: string;
  /** Styles the confirm button as destructive. */
  danger?: boolean;
}

/**
 * Per-action confirmation through the react-shell confirm provider. Resolves true only when the confirm
 * button is pressed for that call; Cancel, the scrim and Escape resolve false. Nothing is remembered.
 */
export function useConfirm(): (options: ConfirmOptions) => Promise<boolean> {
  const { confirm } = useShell();
  return useCallback(
    ({ paragraphs, subject, note, ...rest }: ConfirmOptions) =>
      confirm({
        ...rest,
        description: (
          <>
            {paragraphs.map((text, i) => <p key={i}>{text}</p>)}
            {subject ? <p className="confirm-files">{subject}</p> : null}
            {note ? <p className="confirm-note">{note}</p> : null}
          </>
        )
      }),
    [confirm]
  );
}
