export interface RecoverableFileOperationToastProps {
  error: string | null;
  notice: string | null;
  canUndo: boolean;
  onUndo: () => void;
  onDismiss: () => void;
}

/**
 * Small, non-blocking projection of file-operation outcomes. The undo action
 * remains visible until the daemon confirms restoration; errors never hide a
 * recoverable delete receipt.
 */
export function RecoverableFileOperationToast({
  error,
  notice,
  canUndo,
  onUndo,
  onDismiss,
}: RecoverableFileOperationToastProps) {
  if (!error && !notice && !canUndo) return null;

  const message = error ?? notice ?? "File operation can be undone";
  return (
    <div
      className="pointer-events-auto flex items-center gap-2 rounded-lg border border-line bg-panel px-3 py-2 text-[12px] shadow-lg"
      role={error ? "alert" : "status"}
      aria-live={error ? "assertive" : "polite"}
      data-testid="recoverable-file-operation-toast"
    >
      <span className={error ? "text-danger" : "text-text"}>{message}</span>
      {canUndo ? (
        <button
          type="button"
          className="rounded border border-line px-2 py-1 font-medium text-accent hover:bg-panel-2 focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
          onClick={onUndo}
        >
          Undo
        </button>
      ) : null}
      <button
        type="button"
        aria-label="Dismiss file operation message"
        className="rounded px-1 text-text-3 hover:bg-panel-2 hover:text-text focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
        onClick={onDismiss}
      >
        ×
      </button>
    </div>
  );
}

export default RecoverableFileOperationToast;
