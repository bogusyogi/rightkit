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
  // Immutable wrappers avoid races between GUI & SSH builds or package versions.
  const identity = createHash("sha256").update(script).digest("hex").slice(0, 24);
  const bin = path.join(resolveManagedRoot({ env }), "signing", "codesign-bin", identity);
  ensurePrivateDirectory(bin, { io });
  const file = path.join(bin, "codesign");
  if (io.existsSync(file)) {
    privatePath(file, 0o700, { io });
    if (io.readFileSync(file, "utf8") !== script) throw new Error("Managed codesign shim content changed");
  } else io.writeFileSync(file, script, { mode: 0o700, flag: "wx" });
  // Keep every other shim (especially managed Cargo) in its original order.
  return { ...env, PATH: [bin, ...(env.PATH ?? "").split(path.delimiter).filter((entry) => entry !== bin)].join(path.delimiter) };
}
