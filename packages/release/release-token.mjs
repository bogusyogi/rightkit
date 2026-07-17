import { execFile } from "node:child_process";
import { readFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const KEYCHAIN_SERVICE = "rightkit-rightapps-release-token";

export function defaultWindowsReleaseTokenFile(env = process.env) {
  const appData = env.APPDATA || path.join(os.homedir(), "AppData", "Roaming");
  return path.join(appData, "RightKit", "rightapps-release.token");
}

async function readTokenFile(file) {
  const token = (await readFile(file, "utf8")).trim();
  if (!token) throw new Error(`release token file is empty: ${file}`);
  return token;
}

export async function loadReleaseToken({ env = process.env, platform = process.platform } = {}) {
  const direct = env.RIGHTAPPS_RELEASE_TOKEN?.trim();
  if (direct) return direct;

  if (env.RIGHTAPPS_RELEASE_TOKEN_FILE) {
    try {
      return await readTokenFile(path.resolve(env.RIGHTAPPS_RELEASE_TOKEN_FILE));
    } catch (error) {
      throw new Error(`RIGHTAPPS_RELEASE_TOKEN is unavailable (${error.message})`);
    }
  }

  if (platform === "win32") {
    try {
      return await readTokenFile(defaultWindowsReleaseTokenFile(env));
    } catch (error) {
      throw new Error(`RIGHTAPPS_RELEASE_TOKEN is unavailable (${error.message})`);
    }
  }

  if (platform === "darwin") {
    try {
      const { stdout } = await execFileAsync("security", [
        "find-generic-password",
        "-a", env.USER || os.userInfo().username,
        "-s", KEYCHAIN_SERVICE,
        "-w",
      ], { windowsHide: true });
      const token = stdout.trim();
      if (!token) throw new Error("Keychain item is empty");
      return token;
    } catch (error) {
      throw new Error(`RIGHTAPPS_RELEASE_TOKEN is unavailable (macOS Keychain service ${KEYCHAIN_SERVICE}: ${error.message})`);
    }
  }

  throw new Error("RIGHTAPPS_RELEASE_TOKEN or RIGHTAPPS_RELEASE_TOKEN_FILE is required");
}
