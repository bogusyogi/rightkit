// Map a validated targets.win.msix config block onto buildMsix() options.
import path from "node:path";
import { assertValidMsixTarget, updaterKind } from "./msix-config.mjs";

export function msixBuildOptions({ root, target, app, sign = true, dryRun = false, receipts = {} }) {
  assertValidMsixTarget(target, app);
  const m = target.msix;
  const at = (p) => path.resolve(root, p);
  const manifest = {
    ...m.identity,
    capabilities: m.capabilities,
    dependencies: m.runtime?.dependencies,
    minWindowsVersion: m.minWindowsVersion,
    maxWindowsVersionTested: m.maxWindowsVersionTested,
    virtualization: m.virtualization,
  };
  return {
    stageDir: at(m.stageDir),
    out: at(m.out),
    manifest,
    assets: Object.fromEntries(Object.entries(m.assets).map(([k, v]) => [k, at(v)])),
    runtime: m.runtime,
    appInstaller: updaterKind(target) === "appinstaller" ? { ...m.appInstaller, out: at(m.appInstaller.out) } : undefined,
    sign, dryRun,
    peReceipt: receipts.pe, msixReceipt: receipts.msix,
    receiptDir: receipts.dir,
  };
}
