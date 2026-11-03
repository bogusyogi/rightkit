import { cx } from "./ui.js";

export interface SpinnerProps {
  /** Accessible name announced as the status, for example "Loading sessions". Default "Loading". */
  label?: string;
  /** Diameter in px. Default 16. */
  size?: number;
  className?: string;
}

/**
 * An indeterminate busy indicator announced as a `role="status"`. The ring turns, and stops turning under reduced
 * motion (the static ring still shows the wait). Pair it with `useDeferred` so a fast fetch never shows it.
 */
export function Spinner({ label = "Loading", size = 16, className }: SpinnerProps) {
  return (
    <span role="status" aria-label={label} aria-busy="true" className={cx("rk-spinner", className)} style={{ width: size, height: size }}>
      <span aria-hidden="true" className="rk-spinner__ring" />
    </span>
  );
}
