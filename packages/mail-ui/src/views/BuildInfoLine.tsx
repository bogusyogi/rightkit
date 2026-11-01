import { useEffect, useState } from "react";
import { buildInfo, BuildInfo } from "../lib/ipc.js";

/** One compact line naming this build: version, channel, short commit, date. */
export function BuildInfoLine() {
  const [info, setInfo] = useState<BuildInfo | null>(null);
  useEffect(() => {
    let cancelled = false;
    void buildInfo()
      .then((b) => {
        if (!cancelled) setInfo(b);
      })
      .catch(() => {
        if (!cancelled) setInfo(null);
      });
    return () => {
      cancelled = true;
    };
  }, []);
  if (!info) return null;
  const date = info.build_time.split("T")[0] ?? "";
  return (
    <p className="hint" data-testid="build-info-line" title={info.commit}>
      Build: v{info.version} · {info.channel}
      {info.overrides ? " (overrides)" : ""} · <code>{info.commit_short}</code>
      {info.dirty ? "+dirty" : ""} · {date}
    </p>
  );
}
