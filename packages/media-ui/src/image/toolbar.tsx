// Minimal brand-neutral toolbar primitives replacing app-specific shells.
export function TBtn({
  round,
  active,
  disabled,
  bold,
  width,
  onRun,
  title,
  label,
}: {
  round?: boolean;
  active?: boolean;
  disabled?: boolean;
  bold?: boolean;
  width?: number;
  onRun: () => void;
  title: string;
  label: string;
}) {
  return (
    <button
      type="button"
      title={title}
      aria-label={title}
      aria-pressed={active}
      disabled={disabled}
      // Same activation contract as ViewRight's TBtn: any mouse button acts on
      // mousedown with preventDefault so focus (and any selection) stays where it was.
      onMouseDown={
        disabled
          ? undefined
          : (event) => {
              event.preventDefault();
              event.stopPropagation();
              onRun();
            }
      }
      // A pointer click (detail > 0) always follows the mousedown above, which already
      // ran onRun. Keyboard Enter/Space and assistive-tech click() report detail === 0
      // and have no mousedown, so they run here.
      onClick={
        disabled
          ? undefined
          : (event) => {
              if (event.detail > 0) return;
              onRun();
            }
      }
      className={round ? "rk-iv-btn rk-iv-btn-round" : "rk-iv-btn"}
      style={{
        width,
        fontWeight: bold ? 700 : undefined,
      }}
      data-active={active || undefined}
    >
      {label}
    </button>
  );
}

export function Sep() {
  return <span className="rk-iv-sep" aria-hidden="true" />;
}
