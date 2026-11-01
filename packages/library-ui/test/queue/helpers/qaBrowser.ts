import { spawnSync } from "node:child_process";
import fs from "node:fs";
import process from "node:process";
import { launchChromeForTesting, QaBrowserSession, type QaPage } from "@rightkit/qa";

const repoRoot = process.cwd();
const qaBrowserScript = `${repoRoot}/scripts/qa-browser.mjs`;
const maxDiagnosticChars = 4_000;
function boundedDiagnostic(value: string) {
  const trimmed = value.trim();
  if (!trimmed) return "no output";
  if (trimmed.length <= maxDiagnosticChars) return trimmed;
  return `…${trimmed.slice(-maxDiagnosticChars)}`;
}

function qaBrowserUrl() {
  if (!fs.existsSync(qaBrowserScript)) {
    throw new Error(`QA browser script was not found: ${qaBrowserScript}`);
  }

  const env = { ...process.env };
  // Preserve direct-script behavior if this helper is imported by a Node test
  // worker in the future; runner-only state must not reach Vite's child tree.
  for (const key of Object.keys(env)) {
    const upperKey = key.toUpperCase();
    if (upperKey.startsWith("NODE_TEST_") || upperKey === "NODE_CHANNEL_FD") delete env[key];
  }

  const result = spawnSync(process.execPath, [qaBrowserScript], {
    cwd: repoRoot,
    encoding: "utf8",
    env,
    windowsHide: true,
  });
  const stdout = typeof result.stdout === "string" ? result.stdout : "";
  const stderr = typeof result.stderr === "string" ? result.stderr : "";
  const diagnostics = `stdout: ${boundedDiagnostic(stdout)}\nstderr: ${boundedDiagnostic(stderr)}`;

  if (result.error || result.status !== 0) {
    const reason = result.error?.message ?? `exit status ${result.status ?? "unknown"}`;
    throw new Error(`QA browser flow failed: ${reason}\n${diagnostics}`);
  }

  const outputLines = stdout
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
  const lastLine = outputLines[outputLines.length - 1];
  if (!lastLine) {
    throw new Error(`QA browser flow returned no URL.\n${diagnostics}`);
  }

  let url: URL;
  try {
    url = new URL(lastLine);
  } catch {
    throw new Error(
      `QA browser flow returned an invalid URL: ${boundedDiagnostic(lastLine)}\n${diagnostics}`,
    );
  }
  if (url.protocol !== "http:" || !url.hostname || !url.port) {
    throw new Error(
      `QA browser flow returned a non-HTTP URL: ${boundedDiagnostic(lastLine)}\n${diagnostics}`,
    );
  }
  return url;
}

export async function withQaPage(state: string, run: (page: QaPage) => Promise<void>) {
  const baseUrl = qaBrowserUrl();
  const chrome = await launchChromeForTesting({
    headless: true,
    args: ["--disable-gpu", "--disable-dev-shm-usage", "--window-size=1440,1000"],
  });

  let browser: QaBrowserSession | undefined;
  try {
    browser = await QaBrowserSession.connect(chrome.wsEndpoint);
    const page = await browser.newPage();
    await page.setViewport({ width: 1440, height: 1000, deviceScaleFactor: 1 });
    // "windows" per src/lib/platform.ts uaFallback — the legacy "Win32" value
    // fell through to UA sniffing and rendered mac chrome on mac dev machines.
    const stateUrl = new URL(baseUrl);
    stateUrl.searchParams.set("state", state);
    stateUrl.searchParams.set("qaPlatform", "windows");
    await page.goto(stateUrl.href, { timeout: 30_000 });
    await page.waitForSelector(`[data-qa-state="${state}"]`, { timeout: 10_000 });
    await run(page);
  } finally {
    await browser?.close();
    await chrome.close();
  }
}
