export default function RuntimeStat({ label, value }: { label: string; value: number | string }) {
  return (
    <div className="rounded-lg border border-line bg-panel px-3 py-2">
      <div className="text-[10px] font-semibold uppercase text-text-3">{label}</div>
      <div className="mt-1 font-mono text-[18px] font-semibold tabular-nums text-text">{value}</div>
    </div>
  );
}
