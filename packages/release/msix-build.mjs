// Windows MSIX packaging for staged desktop apps (SwiftPM exe + app-local DLLs).
// Order mirrors the Windows rule in docs/rules/release-signing.md: sign raw PE
// files first, pack the exact signed bytes, then sign the package. All signing
// goes through sign-windows.mjs (provisioned Azure Artifact Signing); this module
// never touches credentials and never authenticates anything itself.

import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { findWindowsSdkTool } from "./preflight.mjs";

const fail = (code, message) => Object.assign(new Error(message), { code });
const here = path.dirname(fileURLToPath(import.meta.url));
export const MSIX_SIGNER_SUBJECT = "CN=Damned Ventures LLC";

const xml = (value) => String(value).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&apos;" })[c]);
const PNG = Buffer.from("89504e470d0a1a0a", "hex");

export function validateMsixIdentity(o) {
  const errors = [];
  if (!/^[-.A-Za-z0-9]{3,50}$/.test(o.name ?? "")) errors.push("name must be 3-50 chars of letters, digits, '-' or '.'");
  if (!/^\d+\.\d+\.\d+\.\d+$/.test(o.version ?? "") || o.version.split(".").some((n) => Number(n) > 65535)) errors.push("version must be four numeric parts <=65535");
  if (!["x64", "arm64"].includes(o.architecture)) errors.push("architecture must be x64 or arm64");
  if (!/\bCN=/.test(o.publisher ?? "")) errors.push("publisher must be a distinguished name containing CN=");
  else if (!o.publisher.includes(MSIX_SIGNER_SUBJECT)) errors.push(`publisher must contain ${MSIX_SIGNER_SUBJECT} (the provisioned signing subject); confirm the full DN natively`);
  if (!o.displayName || !o.publisherDisplayName) errors.push("displayName and publisherDisplayName are required");
  if (!/\.exe$/i.test(o.executable ?? "") || /[\\/]/.test(o.executable ?? "")) errors.push("executable must be a flat .exe name inside the package");
  if (errors.length) throw fail("MSIX_INVALID_IDENTITY", errors.join("; "));
}

export function generateAppxManifest(o) {
  validateMsixIdentity(o);
  const deps = (o.dependencies ?? []).filter((d) => d.minVersion).map((d) =>
    `    <PackageDependency Name="${xml(d.name)}" MinVersion="${xml(d.minVersion)}" Publisher="${xml(d.publisher ?? "CN=Microsoft Corporation, O=Microsoft Corporation, L=Redmond, S=Washington, C=US")}" />`);
  const a = o.assets ?? {};
  return `<?xml version="1.0" encoding="utf-8"?>
<Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"
         xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10"
         xmlns:rescap="http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities"
         IgnorableNamespaces="uap rescap">
  <Identity Name="${xml(o.name)}" Publisher="${xml(o.publisher)}" Version="${xml(o.version)}" ProcessorArchitecture="${o.architecture}" />
  <Properties>
    <DisplayName>${xml(o.displayName)}</DisplayName>
    <PublisherDisplayName>${xml(o.publisherDisplayName)}</PublisherDisplayName>
    <Logo>Assets\\StoreLogo.png</Logo>
  </Properties>
  <Dependencies>
    <TargetDeviceFamily Name="Windows.Desktop" MinVersion="${xml(o.minWindowsVersion ?? "10.0.19041.0")}" MaxVersionTested="${xml(o.maxWindowsVersionTested ?? "10.0.26100.0")}" />
${deps.join("\n")}${deps.length ? "\n" : ""}  </Dependencies>
  <Resources><Resource Language="${xml(o.language ?? "en-us")}" /></Resources>
  <Applications>
    <Application Id="App" Executable="${xml(o.executable)}" EntryPoint="Windows.FullTrustApplication">
      <uap:VisualElements DisplayName="${xml(o.displayName)}" Description="${xml(o.description ?? o.displayName)}" BackgroundColor="transparent"
        Square150x150Logo="Assets\\Square150x150Logo.png" Square44x44Logo="Assets\\Square44x44Logo.png" />
    </Application>
  </Applications>
  <Capabilities>
    <rescap:Capability Name="runFullTrust" />${(o.capabilities ?? []).map((c) => `\n    <Capability Name="${xml(c)}" />`).join("")}
  </Capabilities>
</Package>
`;
}

const ASSETS = { storeLogo: "StoreLogo.png", square150: "Square150x150Logo.png", square44: "Square44x44Logo.png" };

/** Copy a staged directory plus assets and manifest into a package root. */
export function assembleMsixRoot({ stageDir, root, manifest, assets }) {
  stageDir = path.resolve(stageDir); root = path.resolve(root);
  if (!existsSync(path.join(stageDir, manifest.executable))) throw fail("MSIX_MISSING_EXECUTABLE", `executable not in staged directory: ${manifest.executable}`);
  rmSync(root, { recursive: true, force: true });
  mkdirSync(path.join(root, "Assets"), { recursive: true });
  for (const entry of readdirSync(stageDir, { withFileTypes: true })) {
    if (entry.isFile()) copyFileSync(path.join(stageDir, entry.name), path.join(root, entry.name));
    else throw fail("MSIX_NESTED_STAGE", `staged directory must be flat; found ${entry.name}/ (use include with flat files)`);
  }
  for (const [key, target] of Object.entries(ASSETS)) {
    const source = assets?.[key] && path.resolve(assets[key]);
    if (!source || !existsSync(source)) throw fail("MSIX_MISSING_ASSET", `assets.${key} (PNG) is required: ${assets?.[key] ?? "unset"}`);
    if (!readFileSync(source).subarray(0, 8).equals(PNG)) throw fail("MSIX_MISSING_ASSET", `assets.${key} is not a PNG: ${source}`);
    copyFileSync(source, path.join(root, "Assets", target));
  }
  writeFileSync(path.join(root, "AppxManifest.xml"), generateAppxManifest(manifest));
  return root;
}

/** PE files the package ships; these get Authenticode before packing. */
export function peFilesIn(root) {
  return readdirSync(root).filter((name) => /\.(exe|dll)$/i.test(name)).sort().map((name) => path.join(root, name));
}

/**
 * Pack + sign. `run` is injectable for tests. Steps:
 *  1 sign every PE file in the root (raw bytes), 2 makeappx pack, 3 sign the .msix, 4 verify.
 * Windows-only unless dryRun.
 */
export function buildMsix({ stageDir, out, workDir, manifest, assets, sign = true, dryRun = false, receiptDir },
  { platform = process.platform, run = spawnSync, findTool = findWindowsSdkTool, env = process.env, log = (e) => console.error(JSON.stringify(e)) } = {}) {
  out = path.resolve(out);
  if (!out.endsWith(".msix")) throw fail("MSIX_INVALID_OPTIONS", "out must end with .msix");
  validateMsixIdentity(manifest);
  workDir = path.resolve(workDir ?? `${out}.root`);
  const receipts = path.resolve(receiptDir ?? path.dirname(out));
  const steps = ["assemble", ...(sign ? ["sign-pe"] : []), "pack", ...(sign ? ["sign-msix", "verify-msix"] : [])];
  if (dryRun) { log({ event: "msix-dry-run", out, steps }); return { out, steps, dryRun: true }; }
  if (platform !== "win32") throw fail("MSIX_NATIVE_HOST_REQUIRED", "MSIX packing and signing must run on Windows");
  const makeappx = env.MAKEAPPX_PATH || findTool("makeappx.exe")?.toolPath;
  if (!makeappx) throw fail("MSIX_TOOL_MISSING", "makeappx.exe not found; install the Windows SDK or set MAKEAPPX_PATH");
  const checked = (cmd, args) => {
    const r = run(cmd, args, { encoding: "utf8", windowsHide: true, timeout: 600000 });
    if (r.error || r.status !== 0) throw fail("MSIX_COMMAND_FAILED", `${path.basename(cmd)} ${args[0] ?? ""} failed: ${r.error?.message ?? r.stderr ?? r.stdout ?? r.status}`);
    return r;
  };
  const signer = (files, receipt) => checked(process.execPath, [path.join(here, "sign-windows.mjs"), "--receipt", receipt, ...files]);
  const root = assembleMsixRoot({ stageDir, root: workDir, manifest, assets });
  mkdirSync(receipts, { recursive: true });
  if (sign) signer(peFilesIn(root), path.join(receipts, "msix-pe-sign.receipt.json"));
  mkdirSync(path.dirname(out), { recursive: true });
  rmSync(out, { force: true });
  checked(makeappx, ["pack", "/d", root, "/p", out, "/o"]);
  if (!existsSync(out) || statSync(out).size === 0) throw fail("MSIX_PACK_EMPTY", `makeappx produced no package: ${out}`);
  if (sign) {
    signer([out], path.join(receipts, "msix-sign.receipt.json"));
    checked(process.execPath, [path.join(here, "sign-windows.mjs"), "--verify-only", out]);
  }
  log({ event: "msix-built", out, signed: sign });
  return { out, steps, root, signed: sign };
}
