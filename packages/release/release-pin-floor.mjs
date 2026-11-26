// An app's own @rightkit/release child (stage-sidecar, web-build, package) is the code that makes its non-compiling cargo calls. One older
// than the direct route (DIRECT_ROUTE_RELEASE_FLOOR, cargo-target-shim.mjs, GATE-CONTRACT GC-A11) carries them to the broker, which refuses
// them, and the compile stage fails after Class A passed (rightkit#1491). This judges the app's pin and the version installed in the
// snapshot BEFORE any compile, naming both and the fix. Pure over what it is given; nothing is read or run here.
import { DIRECT_ROUTE_RELEASE_FLOOR } from "./cargo-target-shim.mjs";

export const RELEASE_PIN_BELOW_FLOOR = "RK_RELEASE_PIN_BELOW_FLOOR";
const PACKAGE = "@rightkit/release";
const DEPENDENCY_TABLES = ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"];

const triple = (text) => { const m = /^(\d+)\.(\d+)\.(\d+)(?:[-+].*)?$/.exec(String(text ?? "").trim()); return m ? [Number(m[1]), Number(m[2]), Number(m[3])] : null; };

/** -1, 0 or 1 comparing two X.Y.Z versions (a prerelease or build suffix is ignored); null when either is not one. */
export function compareVersions(a, b) {
  const x = triple(a), y = triple(b);
  if (!x || !y) return null;
  for (let i = 0; i < 3; i += 1) if (x[i] !== y[i]) return x[i] < y[i] ? -1 : 1;
  return 0;
}

/** The exact version a dependency spec pins (`0.6.30`, `=0.6.30`), or null for a range, tag, alias or workspace spec (those are judged by the installed version). */
export function exactPinOf(spec) {
  const m = /^=?\s*(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)$/.exec(String(spec ?? "").trim());
  return m ? m[1] : null;
}

/**
 * The refusal for an app whose @rightkit/release is below `floor`, or null. `packageJsonText` is the app's committed package.json and
 * `installedVersion` the version found in its snapshot's node_modules (null when none is installed). An app that does not depend on
 * @rightkit/release is not judged; a pin that is a range is judged by the installed version only.
 */
export function releasePinFloorProblem({ packageJsonText, installedVersion = null, floor = DIRECT_ROUTE_RELEASE_FLOOR, where = "package.json" }) {
  let manifest;
  try { manifest = JSON.parse(packageJsonText); } catch { return null; }
  const specs = DEPENDENCY_TABLES.map((t) => manifest?.[t]?.[PACKAGE]).filter((v) => typeof v === "string");
  if (!specs.length) return null;
  const fix = `set ${PACKAGE} to =${floor} or newer in ${where} (every app moves to the same pinned version), reinstall, and run again`;
  for (const spec of specs) {
    const pin = exactPinOf(spec);
    if (pin !== null && compareVersions(pin, floor) === -1) return { code: RELEASE_PIN_BELOW_FLOOR, message: `${RELEASE_PIN_BELOW_FLOOR}: ${where} pins ${PACKAGE}@${pin}, below the host gate's floor ${floor} for compile stages: its own right-release child would carry non-compiling cargo calls to the broker, which refuses them (GC-A11); ${fix}` };
  }
  if (installedVersion !== null && compareVersions(installedVersion, floor) === -1) return { code: RELEASE_PIN_BELOW_FLOOR, message: `${RELEASE_PIN_BELOW_FLOOR}: the installed ${PACKAGE} is ${installedVersion} (${where} asks ${specs.join(", ")}), below the host gate's floor ${floor} for compile stages: its own right-release child would carry non-compiling cargo calls to the broker, which refuses them (GC-A11); ${fix}` };
  return null;
}

/**
 * One informational line, or null: the app pins an exact @rightkit/release older than the host gate that is running. Inside a gate stage
 * the host gate's own copy runs whatever the app pins (build-launch.mjs runs this package's own right-release for `pnpm exec right-release`),
 * so this is never a refusal. A command typed by hand in the app runs the pinned copy, which can judge a config by older rules
 * (Pulse, 2026-11-26: its 0.6.82 copy refused a switch class the host gate 0.6.88 accepts). A range pin or no pin says nothing.
 */
export function releasePinBehindHostNote({ packageJsonText, hostVersion, where = "package.json" }) {
  let manifest;
  try { manifest = JSON.parse(packageJsonText); } catch { return null; }
  const pins = DEPENDENCY_TABLES.map((t) => manifest?.[t]?.[PACKAGE]).filter((v) => typeof v === "string").map(exactPinOf).filter((pin) => pin !== null);
  const older = pins.filter((pin) => compareVersions(pin, hostVersion) === -1).sort((a, b) => compareVersions(a, b));
  if (!older.length) return null;
  return `${where} pins ${PACKAGE}@${older[0]}; the host gate is ${hostVersion}. The gate's stages run the host gate's copy; a command typed by hand in the app runs the pinned ${older[0]}, whose rules can differ`;
}
