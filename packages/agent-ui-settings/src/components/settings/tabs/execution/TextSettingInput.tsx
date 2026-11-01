export function TextSettingInput({
  label,
  value,
  onChange,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <label className="flex flex-col gap-1 min-w-0">
      <span className="text-[10px] uppercase tracking-[0.08em] text-text-3">{label}</span>
      <input
        className="h-8 min-w-0 rounded-lg border border-line bg-panel px-2 text-[12px] text-text outline-none focus:border-accent/60"
        type="text"
        value={value}
        onChange={(event) => onChange(event.target.value)}
      />
    </label>
  );
}

export function TextSettingTextarea({
  label,
  value,
  onChange,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <label className="flex flex-col gap-1 min-w-0">
      <span className="text-[10px] uppercase tracking-[0.08em] text-text-3">{label}</span>
      <textarea
        className="min-h-20 min-w-0 rounded-lg border border-line bg-panel px-2 py-2 text-[12px] text-text outline-none focus:border-accent/60"
        value={value}
        onChange={(event) => onChange(event.target.value)}
      />
    </label>
  );
}

export function ResourceLimitInput({
  label,
  value,
  onChange,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <label className="flex flex-col gap-1 min-w-0">
      <span className="text-[10px] uppercase tracking-[0.08em] text-text-3">{label}</span>
      <input
        className="h-8 min-w-0 rounded-lg border border-line bg-panel px-2 text-[12px] text-text outline-none focus:border-accent/60"
        type="number"
        min="1"
        step="1"
        inputMode="numeric"
        value={value}
        onChange={(event) => onChange(event.target.value)}
      />
    </label>
  );
}
