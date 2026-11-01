import { useState } from "react";

import { licenseErrorMessage } from "@rightkit/agent-ui-core/state/license";
import { releasePlatform } from "@rightkit/agent-ui-core/state/platform";
import { redeemWindowsLinkCode } from "@rightkit/agent-ui-core/state/proBridge";

/** Windows only: redeem the one-time code an entitled Apple device shows, so
 *  this computer gets its own license without typing a key. Hidden once a
 *  license is active, and everywhere that is not Windows. */
export default function LinkComputerPanel({ active }: { active: boolean }) {
  const platform = releasePlatform();
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [linked, setLinked] = useState(false);

  if (platform !== "windows" || (active && !linked)) return null;

  async function onLink() {
    const trimmed = code.trim();
    if (!trimmed || busy) return;
    setBusy(true);
    setError(null);
    try {
      await redeemWindowsLinkCode(trimmed);
      setCode("");
      setLinked(true);
    } catch (err) {
      setError(licenseErrorMessage(err, "Could not link this computer"));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="rounded-lg border border-line bg-panel p-4" data-testid="link-computer-panel">
      <h3 className="text-[14px] font-semibold text-text">Link from an Apple device</h3>
      <p className="text-[12px] text-text-3">
        Enter the one-time code shown on your Apple device where Pro is active. Codes expire in
        minutes and work once. No license key is shared.
      </p>
      <div className="mt-4 flex flex-col md:flex-row gap-2">
        <input
          aria-label="Link code"
          className="min-w-0 flex-1 h-9 px-3 rounded-lg border border-line bg-bg text-text text-[13px] outline-none focus:border-accent"
          value={code}
          onChange={(event) => setCode(event.target.value.toUpperCase())}
          onKeyDown={(event) => {
            if (event.key === "Enter") void onLink();
          }}
          placeholder="Code from your Apple device"
          autoComplete="off"
          spellCheck={false}
          disabled={busy}
        />
        <button
          type="button"
          className="inline-flex items-center justify-center h-9 px-4 rounded-lg border-0 bg-accent text-on-accent font-semibold text-[13px] cursor-pointer disabled:border disabled:border-line disabled:bg-panel-2 disabled:text-text-3 hover:opacity-90"
          disabled={busy || !code.trim()}
          onClick={() => void onLink()}
        >
          {busy ? "Linking" : "Link this computer"}
        </button>
      </div>
      {linked ? (
        <div className="mt-3 text-[12px] text-text-3">This computer is linked.</div>
      ) : null}
      {error ? <div className="mt-3 text-[12px] text-danger">{error}</div> : null}
    </div>
  );
}
