// Pre-build gate `suite-api-literal`: an app must not carry a Right Suite API host literal. The owner (RightKit) supplies
// the host, the updater endpoint and the artifact allowlists through @rightkit/app-update/suite-api and the native shell.
// Source of truth: contracts/right-suite-api.v1.json, read here through its generated projection.
import { existsSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import {
  RIGHT_SUITE_API_CONTRACT,
  RIGHT_SUITE_API_HOST,
  RIGHT_SUITE_API_ORIGIN,
  RIGHT_SUITE_GATED_ARTIFACT_HOST,
  RIGHT_SUITE_LEGACY_HOSTS,
  RIGHT_SUITE_PUBLIC_ARTIFACT_HOST,
  RIGHT_SUITE_UPDATER_MANIFEST_PATH,
} from "./suite-api.generated.mjs";

/** Every host the contract names: the API host, its legacy hosts, and both artifact hosts. */
export const SUITE_API_CONTRACT_HOSTS = Object.freeze([
  RIGHT_SUITE_API_HOST,
  ...RIGHT_SUITE_LEGACY_HOSTS,
  RIGHT_SUITE_GATED_ARTIFACT_HOST,
  RIGHT_SUITE_PUBLIC_ARTIFACT_HOST,
]);

const escapeRegExp = (text) => text.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&");
// A host literal is refused as a whole name: a neighbouring label or hyphen does not make a different host.
const HOST_PATTERNS = SUITE_API_CONTRACT_HOSTS.map((host) => ({
  host,
  pattern: new RegExp(`(?<![A-Za-z0-9-])${escapeRegExp(host)}(?![A-Za-z0-9-])`, "giu"),
}));

/** The one updater endpoint a pending app may keep in its tauri conf (contract pendingNativeUpdater). */
export function pendingUpdaterEndpoint(appKey) {
  return `${RIGHT_SUITE_API_ORIGIN}${RIGHT_SUITE_UPDATER_MANIFEST_PATH.split("{app}").join(appKey)}`;
}

function trackedFiles(root) {
  const result = spawnSync("git", ["ls-files", "-z"], { cwd: root, encoding: "utf8", windowsHide: true });
  if (result.status !== 0) throw new Error(`suite-api-literal: cannot list tracked files under ${root}: ${(result.stderr ?? "").trim()}`);
  return result.stdout.split("\0").filter(Boolean).map((file) => file.split(path.sep).join("/"));
}

const isExcluded = (file) => /\.md$/iu.test(file) || file === "docs" || file.startsWith("docs/");

/**
 * Scan the app's tracked, non-Markdown files for Right Suite API host literals.
 * `appKey` is the app's identity (right-release config `app`). Returns findings `{ file, line, host, kind, detail }`;
 * `file` is relative to `root` with forward slashes. Empty array means the app carries no literal.
 *
 * Refused: (a) any contract host in any tracked file; (b) `plugins.updater.endpoints` in a `tauri*.conf.json` of an app the
 * contract lists under `apps`; (c) a host in a Tauri capability JSON scope (`capabilities/*.json`).
 * Exception: for an app in `pendingNativeUpdater`, its conf may keep `plugins.updater.endpoints` when that array is exactly
 * the one contract updater endpoint for the app. Only that exact string is exempt from the host scan.
 */
export function suiteApiLiteralFindings({ root, appKey, contract = RIGHT_SUITE_API_CONTRACT }) {
  const findings = [];
  const inContract = typeof appKey === "string" && Object.hasOwn(contract.apps ?? {}, appKey);
  const pending = typeof appKey === "string" && (contract.pendingNativeUpdater ?? []).includes(appKey);
  const allowedEndpoint = typeof appKey === "string" ? pendingUpdaterEndpoint(appKey) : null;

  for (const file of trackedFiles(root)) {
    if (isExcluded(file)) continue;
    const absolute = path.join(root, file);
    if (!existsSync(absolute) || !statSync(absolute).isFile()) continue;
    const buffer = readFileSync(absolute);
    if (buffer.includes(0)) continue;
    let text = buffer.toString("utf8");
    const base = path.posix.basename(file);
    const isTauriConf = /^tauri.*\.conf\.json$/iu.test(base);
    const isCapability = /(^|\/)capabilities\/[^/]+\.json$/iu.test(file);

    if (isTauriConf && inContract && text.includes("endpoints")) {
      let endpoints;
      let parsed = true;
      try { endpoints = JSON.parse(text)?.plugins?.updater?.endpoints; } catch { parsed = false; }
      if (!parsed) {
        findings.push({ file, line: 1, host: "(unparseable)", kind: "updater-endpoints", detail: "tauri conf cannot be parsed, so its updater endpoints cannot be verified" });
      } else if (endpoints !== undefined) {
        const exact = pending && Array.isArray(endpoints) && endpoints.length === 1 && endpoints[0] === allowedEndpoint;
        if (exact) {
          // Remove only the exact contract string; line count is unchanged because the string has no newline.
          text = text.split(allowedEndpoint).join("");
        } else {
          const line = text.split("\n").findIndex((candidate) => candidate.includes("endpoints")) + 1 || 1;
          findings.push({ file, line, host: "plugins.updater.endpoints", kind: "updater-endpoints", detail: "updater endpoints must be absent; the shell supplies the endpoint from @rightkit/app-update/suite-api" });
        }
      }
    }

    const lines = text.split("\n");
    lines.forEach((content, index) => {
      for (const { host, pattern } of HOST_PATTERNS) {
        pattern.lastIndex = 0;
        if (!pattern.test(content)) continue;
        pattern.lastIndex = 0;
        const kind = isCapability && file.endsWith(".json") ? "capability-scope" : "host-literal";
        findings.push({ file, line: index + 1, host, kind, detail: kind === "capability-scope" ? "capability scope names a Right Suite API host" : "Right Suite API host literal" });
      }
    });
  }
  return findings;
}

/** The gate's refusal text: one line per finding, `file:line` and the host, so the author can fix each site. */
export function suiteApiLiteralDetail(appKey, findings) {
  return `${appKey ?? "app"} carries Right Suite API host literals (contract right-suite-api.v1); use @rightkit/app-update/suite-api or the native update host:\n${findings
    .map((finding) => `- ${finding.file}:${finding.line}: ${finding.host} (${finding.kind})`)
    .join("\n")}`;
}
