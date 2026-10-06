import { useState } from "react";
import { AppShell } from "./shell/AppShell";
import { invoke } from "./lib/host";
import { detectPlatform } from "./lib/platform";

// PLACEHOLDER for `@rightkit/media-ui` (player / image viewer): add it when the app shows media:
//   pnpm add @rightkit/media-ui@<exact>, import "@rightkit/media-ui/styles.css", mount in a view.

export default function App() {
  const [doctor, setDoctor] = useState<string>("");
  const platform = detectPlatform();
  return (
    <AppShell title="RightApp">
      <p>Platform: <span data-testid="platform">{platform}</span></p>
      <button
        type="button"
        className="rounded bg-brand-action px-3 py-1 text-white hover:bg-brand-action-hover"
        onClick={async () => setDoctor(JSON.stringify(await invoke("engine_call", { cmd: "doctor", input: {} })))}
      >
        Run engine doctor
      </button>
      <pre data-testid="doctor">{doctor}</pre>
    </AppShell>
  );
}
