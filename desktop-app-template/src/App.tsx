import { useState } from "react";
import { getPlatform } from "@rightkit/platform-ui";
import { AppShell, type NavGroup } from "@rightkit/react-shell/react";
import { getShell, shellBridge } from "./lib/shell";

// One destination. A media app adds @rightkit/media-ui for its player or viewer (README step 10).
const NAV: readonly NavGroup[] = [{ items: [{ id: "home", label: "Home" }] }];

export default function App() {
  const [doctor, setDoctor] = useState<string>("");
  const platform = getPlatform();
  return (
    <AppShell
      title="RightApp"
      groups={NAV}
      activeId="home"
      onNavigate={() => undefined}
      sidebar={false}
      sidebarToggle={false}
      searchMode="none"
      bridge={shellBridge}
      platform={platform}
    >
      <div className="p-4">
        <p>Platform: <span data-testid="platform">{platform}</span></p>
        <button
          type="button"
          className="rounded bg-brand-action px-3 py-1 text-white hover:bg-brand-action-hover"
          onClick={async () => setDoctor(JSON.stringify(await getShell().engine.call("doctor", {})))}
        >
          Run engine doctor
        </button>
        <pre data-testid="doctor">{doctor}</pre>
      </div>
    </AppShell>
  );
}
