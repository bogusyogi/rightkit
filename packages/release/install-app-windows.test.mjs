import assert from "node:assert/strict";
import test from "node:test";

import { installAppWindows, InstallAppWindowsError, windowsInstallArguments } from "./install-app-windows.mjs";
import { parseArgs } from "./install-app.mjs";

test("the Windows installer is pinned to the one fixed folder", () => {
  assert.deepEqual(windowsInstallArguments("C:\\Users\\a\\AppData\\Local\\Programs\\Pulse"), ["/S", "/D=C:\\Users\\a\\AppData\\Local\\Programs\\Pulse"]);
});

test("install-app --installer needs the fixed folder and the executable name, and excludes the macOS inputs", () => {
  const ok = parseArgs(["--installer", "C:\\x\\Setup.exe", "--dest", "C:\\Apps\\Pulse", "--exe", "Pulse.exe"]);
  assert.equal(ok.installer, "C:\\x\\Setup.exe");
  assert.equal(ok.dest, "C:\\Apps\\Pulse");
  assert.throws(() => parseArgs(["--installer", "C:\\x\\Setup.exe", "--exe", "Pulse.exe"]), /requires --dest/);
  assert.throws(() => parseArgs(["--installer", "C:\\x\\Setup.exe", "--dest", "C:\\Apps\\Pulse"]), /requires --dest/);
  assert.throws(() => parseArgs(["--installer", "C:\\x\\Setup.exe", "--dest", "C:\\A", "--exe", "P.exe", "--dev"]), /cannot be combined/);
});

test("the Windows installer mode refuses other platforms and unsafe folders", () => {
  assert.throws(() => installAppWindows({ installer: "C:\\x\\Setup.exe", dest: "C:\\Apps\\Pulse", exe: "Pulse.exe" }, { platform: "darwin" }), (error) => error instanceof InstallAppWindowsError && error.code === "unsupported");
  assert.throws(() => installAppWindows({ installer: "C:\\x\\Setup.exe", dest: "C:\\", exe: "Pulse.exe" }, { platform: "win32" }), /not a drive root/);
  assert.throws(() => installAppWindows({ installer: "C:\\x\\Setup.exe", dest: "C:\\Apps\\Pulse", exe: "..\\Pulse.exe" }, { platform: "win32" }), /bare executable name/);
});
