import { Cpu, WifiSlash, CloudSlash, CalendarX, type Icon as PhosphorIcon } from "@phosphor-icons/react";

const FEATURE_ICON: Record<"device" | "offline" | "cloud" | "subscription", PhosphorIcon> = {
  device: Cpu,
  offline: WifiSlash,
  cloud: CloudSlash,
  subscription: CalendarX,
};

export function FeatureIcon({ kind }: { kind: "device" | "offline" | "cloud" | "subscription" }) {
  const Icon = FEATURE_ICON[kind];
  return (
    <span className="grid h-[58px] w-[58px] place-items-center">
      <Icon className="h-9 w-9 text-[var(--accent-hover)]" weight="bold" aria-hidden="true" />
    </span>
  );
}

export function SectionIntro({
  eyebrow,
  title,
  copy,
}: {
  eyebrow: string;
  title: string;
  copy: string;
}) {
  return (
    <div className="mx-auto max-w-[860px] text-center">
      {eyebrow ? (
        <p className="m-0 mb-1.5 text-[11px] font-bold uppercase tracking-[0.12em] text-[var(--accent-hover)]">
          {eyebrow}
        </p>
      ) : null}
      <h2 className="m-0 [font-family:var(--font-display)] text-[28px] font-semibold leading-none tracking-[0] text-[var(--paper)]">
        {title}
        <span className="text-[var(--accent-hover)]">.</span>
      </h2>
      <p className="m-0 mx-auto mt-2 max-w-[780px] text-[13px] leading-[1.45] text-[var(--muted-2)] [text-wrap:pretty]">
        {copy}
      </p>
    </div>
  );
}
