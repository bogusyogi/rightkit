import { describe, expect, it } from "vitest";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { mkRepo, manifest, ok, fail, fakeRun } from "./support.js";
import {
  launchdLabel,
  renderLaunchdPlist,
  runSchedule,
  validateInterval,
  isoDuration,
  renderTaskXml,
  windowsCreateArgs,
  windowsTaskArguments,
  windowsTaskName,
  type ScheduleSpec,
} from "../src/schedule.js";

const spec = (overrides: Partial<ScheduleSpec> = {}): ScheduleSpec => ({
  repo: "/work/rightkit",
  intervalMinutes: 1440,
  nodePath: "/usr/local/bin/node",
  binPath: "/usr/local/lib/node_modules/@rightkit/outdated/dist/bin.js",
  pathEnv: "/usr/local/bin:/usr/bin:/bin",
  home: "/Users/dev",
  platform: "darwin",
  ...overrides,
});

describe("launchd plist", () => {
  it("runs `right-update run --repo <repo> --quiet` on an interval with logs under ~/Library/Logs/RightKit/update", () => {
    const plist = renderLaunchdPlist(spec());
    const label = launchdLabel("/work/rightkit");
    expect(label).toMatch(/^com\.rightkit\.update\.rightkit-[0-9a-f]{8}$/);
    expect(plist).toContain(`<key>Label</key>\n  <string>${label}</string>`);
    expect(plist).toContain(
      [
        "    <string>/usr/local/bin/node</string>",
        "    <string>/usr/local/lib/node_modules/@rightkit/outdated/dist/bin.js</string>",
        "    <string>run</string>",
        "    <string>--repo</string>",
        "    <string>/work/rightkit</string>",
        "    <string>--quiet</string>",
      ].join("\n"),
    );
    expect(plist).toContain("<key>StartInterval</key>\n  <integer>86400</integer>");
    expect(plist).toContain("<key>RunAtLoad</key>\n  <false/>");
    const slug = label.replace("com.rightkit.update.", "");
    expect(plist).toContain(`<string>/Users/dev/Library/Logs/RightKit/update/${slug}.log</string>`);
    expect(plist).toContain("<key>PATH</key>\n    <string>/usr/local/bin:/usr/bin:/bin</string>");
    expect(plist).toContain("<key>WorkingDirectory</key>\n  <string>/work/rightkit</string>");
  });

  it("uses the interval in seconds and escapes XML in paths", () => {
    const plist = renderLaunchdPlist(spec({ repo: "/work/a&b <c>", intervalMinutes: 30 }));
    expect(plist).toContain("<integer>1800</integer>");
    expect(plist).toContain("/work/a&amp;b &lt;c&gt;");
    expect(plist).not.toContain("a&b");
  });

  it("gives different repos different labels, and the same repo a stable label", () => {
    expect(launchdLabel("/work/one/app")).not.toBe(launchdLabel("/work/two/app"));
    expect(launchdLabel("/work/one/app")).toBe(launchdLabel("/work/one/app"));
  });
});

describe("Task Scheduler definition", () => {
  const win = (overrides: Partial<ScheduleSpec> = {}) =>
    spec({ platform: "win32", repo: "C:\\work\\rk", nodePath: "C:\\Program Files\\nodejs\\node.exe", binPath: "C:\\Users\\dev\\AppData\\Roaming\\npm\\node_modules\\@rightkit\\update\\dist\\bin.js", home: "C:\\Users\\dev", localAppData: "C:\\Users\\dev\\AppData\\Local", userName: "PC\\dev", ...overrides });

  it("wraps the run in a hidden, non-interactive PowerShell and appends to the log", () => {
    const args = windowsTaskArguments(win());
    expect(args.startsWith("-WindowStyle Hidden -NonInteractive -NoProfile -Command ")).toBe(true);
    expect(args).toContain("& 'C:\\Program Files\\nodejs\\node.exe' 'C:\\Users\\dev\\AppData\\Roaming\\npm\\node_modules\\@rightkit\\update\\dist\\bin.js' run --repo 'C:\\work\\rk' --quiet");
    expect(args).toContain("Out-File -Append -Encoding utf8 -LiteralPath 'C:\\Users\\dev\\AppData\\Local\\RightKit\\Logs\\update\\");
  });

  it("registers a hidden, current-user, limited, interactive-only task from XML (no 261 character /TR limit)", () => {
    const xmlText = renderTaskXml(win({ intervalMinutes: 90 }), new Date("2026-10-11T00:00:00Z"));
    expect(xmlText).toContain("<Interval>PT1H30M</Interval>");
    expect(xmlText).toContain("<StartBoundary>2026-10-11T00:00:00</StartBoundary>");
    expect(xmlText).toContain("<LogonType>InteractiveToken</LogonType>");
    expect(xmlText).toContain("<RunLevel>LeastPrivilege</RunLevel>");
    expect(xmlText).toContain("<Hidden>true</Hidden>");
    expect(xmlText).toContain("<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>");
    expect(xmlText).toContain("<Command>powershell.exe</Command>");
    expect(xmlText).toMatch(/<Arguments>-WindowStyle Hidden -NonInteractive -NoProfile -Command .*--quiet.*<\/Arguments>/);
    expect(xmlText).toContain("<WorkingDirectory>C:\\work\\rk</WorkingDirectory>");
    expect(windowsTaskArguments(win()).length).toBeGreaterThan(261);
  });

  it("escapes XML in the description and arguments", () => {
    const xmlText = renderTaskXml(win({ repo: "C:\\work\\a&b" }));
    expect(xmlText).toContain("C:\\work\\a&amp;b");
    expect(xmlText).not.toContain("a&b");
  });

  it("formats ISO durations", () => {
    expect(isoDuration(15)).toBe("PT15M");
    expect(isoDuration(60)).toBe("PT1H");
    expect(isoDuration(1440)).toBe("P1D");
    expect(isoDuration(1500)).toBe("P1DT1H");
    expect(isoDuration(10080)).toBe("P7D");
  });

  it("builds schtasks /Create from the XML file with a per-repo task name", () => {
    const args = windowsCreateArgs(win(), "C:\\x\\t.xml");
    expect(args).toEqual(["schtasks", "/Create", "/TN", windowsTaskName("C:\\work\\rk"), "/XML", "C:\\x\\t.xml", "/F"]);
    expect(windowsTaskName("C:\\work\\rk")).toMatch(/^\\RightKit\\update-.+-[0-9a-f]{8}$/);
  });

  it("rejects characters that cannot be quoted safely", () => {
    expect(() => windowsTaskArguments(win({ repo: 'C:\\bad"path' }))).toThrow(/cannot be scheduled safely/);
    expect(() => windowsTaskArguments(win({ repo: "C:\\100%\\x" }))).toThrow(/cannot be scheduled safely/);
  });
});

describe("runSchedule", () => {
  function setup() {
    const repo = mkRepo({ ".rightkit/update.json": manifest([]) });
    const home = fs.mkdtempSync(path.join(fs.realpathSync(os.tmpdir()), "right-update-home-"));
    const bin = path.join(home, "bin.js");
    fs.writeFileSync(bin, "// bin");
    return { repo, home, bin };
  }

  it("installs a launchd agent: writes the plist, boots out any old one, then bootstraps it", async () => {
    const { repo, home, bin } = setup();
    const { run, calls } = fakeRun(() => ok());
    const result = await runSchedule("install", { repo, intervalMinutes: 60 }, { run, platform: "darwin", home, nodePath: process.execPath, binPath: bin, env: { PATH: "/usr/bin" }, uid: 501 });
    expect(result.exitCode).toBe(0);
    const label = launchdLabel(repo);
    const plistPath = path.posix.join(home, "Library", "LaunchAgents", `${label}.plist`);
    expect(result.files).toEqual([plistPath]);
    const plist = fs.readFileSync(plistPath, "utf8");
    expect(plist).toContain(`<string>${repo}</string>`);
    expect(plist).toContain("<integer>3600</integer>");
    expect(calls).toEqual([
      ["launchctl", "bootout", `gui/501/${label}`],
      ["launchctl", "bootstrap", "gui/501", plistPath],
    ]);
    expect(fs.existsSync(path.join(home, "Library", "Logs", "RightKit", "update"))).toBe(true);
  });

  it("reports status and removes the agent", async () => {
    const { repo, home, bin } = setup();
    const deps = { platform: "darwin" as const, home, nodePath: process.execPath, binPath: bin, env: { PATH: "/usr/bin" }, uid: 501 };
    expect((await runSchedule("status", { repo }, { ...deps, run: fakeRun(() => ok()).run })).installed).toBe(false);
    await runSchedule("install", { repo }, { ...deps, run: fakeRun(() => ok()).run });
    const status = await runSchedule("status", { repo }, { ...deps, run: fakeRun(() => ok("state = waiting")).run });
    expect(status).toMatchObject({ installed: true, loaded: true, exitCode: 0 });
    const removed = await runSchedule("remove", { repo }, { ...deps, run: fakeRun(() => ok()).run });
    expect(removed.exitCode).toBe(0);
    expect(fs.existsSync(path.join(home, "Library", "LaunchAgents", `${launchdLabel(repo)}.plist`))).toBe(false);
  });

  it("surfaces a launchctl bootstrap failure", async () => {
    const { repo, home, bin } = setup();
    const { run } = fakeRun((argv) => (argv[1] === "bootstrap" ? fail("Bootstrap failed: 5") : ok()));
    const result = await runSchedule("install", { repo }, { run, platform: "darwin", home, nodePath: process.execPath, binPath: bin, env: { PATH: "/usr/bin" }, uid: 501 });
    expect(result.exitCode).toBe(2);
    expect(result.message).toMatch(/bootstrap failed/);
  });

  it("registers a Windows task from a UTF-16 XML file through schtasks, and deletes it", async () => {
    const { repo, home } = setup();
    const { run, calls } = fakeRun(() => ok());
    const local = path.join(home, "Local");
    const deps = { run, platform: "win32" as const, home, nodePath: "C:\\n\\node.exe", binPath: "C:\\b.js", env: { LOCALAPPDATA: local, USERNAME: "dev", USERDOMAIN: "PC" } };
    const created = await runSchedule("install", { repo, intervalMinutes: 15 }, deps);
    expect(created.exitCode).toBe(0);
    expect(calls[0].slice(0, 4)).toEqual(["schtasks", "/Create", "/TN", windowsTaskName(repo)]);
    expect(calls[0]).toContain("/XML");
    const xmlFile = created.files[0];
    expect(xmlFile.replace(/\\/g, "/")).toContain(`${local.replace(/\\/g, "/")}/RightKit/update/`);
    const raw = fs.readFileSync(xmlFile.replace(/\\/g, "/"));
    expect([raw[0], raw[1]]).toEqual([0xff, 0xfe]);
    expect(raw.subarray(2).toString("utf16le")).toContain("<Hidden>true</Hidden>");
    await runSchedule("remove", { repo }, deps);
    expect(calls.at(-1)).toEqual(["schtasks", "/Delete", "/TN", windowsTaskName(repo), "/F"]);
  });

  it("rejects bad intervals, missing manifests and unsupported platforms", async () => {
    const { repo, home, bin } = setup();
    expect(validateInterval(5)).toMatch(/--interval/);
    expect(validateInterval(60)).toBeNull();
    const base = { run: fakeRun(() => ok()).run, home, binPath: bin };
    expect((await runSchedule("install", { repo, intervalMinutes: 5 }, { ...base, platform: "darwin" })).exitCode).toBe(2);
    const noManifest = fs.mkdtempSync(path.join(fs.realpathSync(os.tmpdir()), "right-update-empty-"));
    expect((await runSchedule("install", { repo: noManifest }, { ...base, platform: "darwin" })).message).toMatch(/no \.rightkit\/update\.json/);
    expect((await runSchedule("status", { repo }, { ...base, platform: "linux" })).message).toMatch(/macOS and Windows only/);
  });
});
