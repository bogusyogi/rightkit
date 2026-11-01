import { useCallback } from 'react';
import { errorCode, errorDetails } from '../lib/api.js';
import { useConfirm } from '../lib/confirm.js';

/** Engine error code for a local reference image that has not been approved for upload. */
export const FAL_UPLOAD_CONFIRM_CODE = 'upload_confirmation_required';

/** Request field that approves one run's upload. Send the boolean `true` per run; never store or default it. */
export const FAL_UPLOAD_CONFIRM_FIELD = 'confirm_fal_upload';

export const FAL_UPLOAD_CONFIRM_TEXT = 'This uploads this image to your fal storage so NanoBanana can read it.';

export function needsFalUploadConfirmation(error: unknown): boolean {
  return errorCode(error) === FAL_UPLOAD_CONFIRM_CODE;
}

const fileNameOf = (path: string): string => path.split(/[\\/]/).filter(Boolean).pop() || 'image';

/**
 * File names for the upload prompt: the engine's `details.paths` when it sent them, else the caller's
 * `fallback` (names the caller already knows from the request). Names are never parsed from the message.
 */
export function falUploadNames(error: unknown, fallback: string[] = []): string[] {
  const paths = errorDetails(error)?.paths;
  const names = Array.isArray(paths)
    ? paths.filter((p): p is string => typeof p === 'string' && p.trim() !== '').map(fileNameOf)
    : [];
  return names.length ? names : fallback;
}

/** Local image count from the engine's `details.count`, or null when absent. */
export function falUploadCount(error: unknown): number | null {
  const count = errorDetails(error)?.count;
  return typeof count === 'number' && Number.isInteger(count) && count > 0 ? count : null;
}

/**
 * Per-run consent for sending a local reference image to the person's own fal storage.
 * `confirm()` resolves true only when Confirm is pressed for this call; nothing is remembered.
 */
export function useFalUploadConfirm(): { confirm: (names?: string[], count?: number) => Promise<boolean> } {
  const generic = useConfirm();
  const confirm = useCallback(
    (names: string[] = [], count?: number) =>
      generic({
        title: 'Upload this image?',
        paragraphs: [FAL_UPLOAD_CONFIRM_TEXT],
        subject: names.length
          ? names.join(', ')
          : count
            ? `${count} local image${count === 1 ? '' : 's'}`
            : undefined,
        note: 'It goes to your own fal account under your fal key, nowhere else. This approval covers this run only.',
        confirmLabel: 'Confirm'
      }),
    [generic]
  );
  return { confirm };
}
