import * as fs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
import { detectMacSigningSession } from "./macos-signing-session.mjs";
import { ensurePrivateDirectory, privatePath } from "./rightkit-signing-agent.mjs";
import { resolveManagedRoot } from "./artifact-storage.mjs";

const quote = (text) => `'${String(text).replaceAll("'", "'\\''")}'`;
export function codesignShimScript({ route, node = process.execPath,
  entrypoint = fileURLToPath(new URL("./codesign-shim.mjs", import.meta.url)), realCodesign = "/usr/bin/codesign" }) {
  return `#!/bin/sh\nif [ -n "\${SSH_CONNECTION-}\${SSH_TTY-}\${SSH_CLIENT-}" ] || [ ${quote(route)} = agent ]; then
  exec ${quote(node)} ${quote(entrypoint)} "$@"
fi
exec ${quote(realCodesign)} "$@"\n`;
}

export function macosCodesignEnvironment(env = process.env, { platform = process.platform, io = fs,
  detect = detectMacSigningSession, node = process.execPath } = {}) {
  if (platform !== "darwin") return env;
  const route = detect({ env }).guiRunner ? "agent" : "direct";
  const script = codesignShimScript({ route, node });
  const automationScript = `#!/bin/sh\nexec ${quote(node)} ${quote(fileURLToPath(new URL("./osascript-guard.mjs", import.meta.url)))} "$@"\n`;
  // Tauri's script invokes /usr/bin/osascript, bypassing PATH. Its env-bash
  // shebang & app-side `bash bundle_dmg.sh` calls both traverse this wrapper.
  const bashScript = `#!/bin/sh\nlayout_script=\nskip_layout=0\nfor arg in "$@"; do
  case "$arg" in
    */bundle_dmg.sh|bundle_dmg.sh) layout_script="$arg" ;;
    --skip-jenkins) skip_layout=1 ;;
  esac
done
if [ -n "$layout_script" ] && [ "$skip_layout" = 0 ]; then
  exec ${quote(node)} ${quote(fileURLToPath(new URL("./osascript-guard.mjs", import.meta.url)))} --caller "$layout_script"
fi
exec /bin/bash "$@"\n`;
  // Immutable wrappers avoid races between GUI & SSH builds or package versions.
  const identity = createHash("sha256").update(script).update(automationScript).update(bashScript).digest("hex").slice(0, 24);
  const bin = path.join(resolveManagedRoot({ env }), "signing", "codesign-bin", identity);
  ensurePrivateDirectory(bin, { io });
  const file = path.join(bin, "codesign");
  if (io.existsSync(file)) {
    privatePath(file, 0o700, { io });
    if (io.readFileSync(file, "utf8") !== script) throw new Error("Managed codesign shim content changed");
  } else io.writeFileSync(file, script, { mode: 0o700, flag: "wx" });
  const automationFile = path.join(bin, "osascript");
  if (io.existsSync(automationFile)) {
    privatePath(automationFile, 0o700, { io });
    if (io.readFileSync(automationFile, "utf8") !== automationScript) throw new Error("Managed osascript guard content changed");
  } else io.writeFileSync(automationFile, automationScript, { mode: 0o700, flag: "wx" });
  const bashFile = path.join(bin, "bash");
  if (io.existsSync(bashFile)) {
    privatePath(bashFile, 0o700, { io });
    if (io.readFileSync(bashFile, "utf8") !== bashScript) throw new Error("Managed bash guard content changed");
  } else io.writeFileSync(bashFile, bashScript, { mode: 0o700, flag: "wx" });
  // Keep every other shim (especially managed Cargo) in its original order.
  return { ...env, CI: "true", TAURI_BUNDLER_DMG_IGNORE_CI: "false",
    PATH: [bin, ...(env.PATH ?? "").split(path.delimiter).filter((entry) => entry !== bin)].join(path.delimiter) };
}
