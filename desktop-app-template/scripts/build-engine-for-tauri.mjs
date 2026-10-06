// Builds the engine sidecar through managed RightKit Cargo and stages it where Tauri's
// `bundle.externalBin` expects it: src-tauri/binaries/<name>-<target-triple>[.exe].
// Managed builds put output under an external target root, so the executable is resolved from
// cargo's own JSON artifact stream rather than a guessed path.
import { copyFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { resolveCommand } from "@rightkit/release/development.mjs";

const ENGINE = "rightapp-engine";
const root = new URL("..", import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1");

function rightkit(args, options = {}) {
  const launch = resolveCommand("rightkit", args, { root, env: process.env });
  return spawnSync(launch.command, launch.args, {
    cwd: root, env: process.env, shell: false, windowsHide: true, ...options,
  });
}

let target = process.env.TAURI_ENV_TARGET_TRIPLE;
if (!target) {
  const v = rightkit(["rustc", "-vV"], { encoding: "utf8" });
  if (v.status !== 0) throw new Error(`rightkit rustc -vV failed (${v.status})`);
  target = v.stdout.match(/^host:\s*(.+)$/m)?.[1]?.trim();
  if (!target) throw new Error("Could not determine Rust host triple");
}

const profile = process.env.RIGHTKIT_BUILD_MODE === "dev"
  ? ["--profile", process.env.RIGHTKIT_CARGO_PROFILE || "release-iterate"]
  : ["--release"];
const result = rightkit(
  ["cargo", "build", ...profile, "--locked", "--target", target,
    "--message-format=json-render-diagnostics",
    "--manifest-path", join(root, "Cargo.toml"), "-p", ENGINE],
  { encoding: "utf8", maxBuffer: 512 * 1024 * 1024, stdio: ["inherit", "pipe", "inherit"] },
);
if (result.status !== 0) throw new Error(`${ENGINE} build failed (${result.status})`);

let source = null;
for (const line of (result.stdout ?? "").split("\n")) {
  if (!line.startsWith("{")) continue;
  try {
    const m = JSON.parse(line);
    if (m.reason === "compiler-artifact" && m.executable && m.target?.name === ENGINE) source = m.executable;
  } catch { /* non-JSON line */ }
}
if (!source) throw new Error(`${ENGINE} build produced no executable`);

const dir = join(root, "src-tauri", "binaries");
mkdirSync(dir, { recursive: true });
const dest = join(dir, `${ENGINE}-${target}${process.platform === "win32" ? ".exe" : ""}`);
copyFileSync(source, dest);
console.log(`staged ${source} -> ${dest}`);
