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
      onClick={onRun}
      className="rk-iv-btn"
      style={{
        width,
        borderRadius: round ? 999 : undefined,
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
