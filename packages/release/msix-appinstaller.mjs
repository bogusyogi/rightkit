// App Installer (.appinstaller) update route for MSIX packages. Trust comes from the
// MSIX Authenticode signature (publisher must match the manifest); no separate updater key.

const xml = (value) => String(value).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&apos;" })[c]);
const fail = (message) => Object.assign(new Error(message), { code: "APPINSTALLER_INVALID" });

export function generateAppInstaller(o) {
  const errors = [];
  for (const key of ["uri", "msixUri"]) { try { if (new URL(o[key]).protocol !== "https:") throw 0; } catch { errors.push(`${key} must be an https URL`); } }
  if (!/^\d+\.\d+\.\d+\.\d+$/.test(o.version ?? "")) errors.push("version must be four numeric parts");
  if (!["x64", "arm64"].includes(o.architecture)) errors.push("architecture must be x64 or arm64");
  if (!o.name || !o.publisher) errors.push("name and publisher are required");
  const hours = o.hoursBetweenUpdateChecks ?? 24;
  if (!Number.isInteger(hours) || hours < 0 || hours > 255) errors.push("hoursBetweenUpdateChecks must be an integer 0-255");
  if (o.updateBlocksActivation && !o.showPrompt) errors.push("updateBlocksActivation requires showPrompt: true (App Installer schema)");
  if (o.automaticBackgroundTask !== undefined && typeof o.automaticBackgroundTask !== "boolean") errors.push("automaticBackgroundTask must be a boolean");
  if (errors.length) throw fail(errors.join("; "));
  return `<?xml version="1.0" encoding="utf-8"?>
<AppInstaller xmlns="http://schemas.microsoft.com/appx/appinstaller/2018" Version="${xml(o.version)}" Uri="${xml(o.uri)}">
  <MainPackage Name="${xml(o.name)}" Publisher="${xml(o.publisher)}" Version="${xml(o.version)}" ProcessorArchitecture="${o.architecture}" Uri="${xml(o.msixUri)}" />
  <UpdateSettings>
    <OnLaunch HoursBetweenUpdateChecks="${hours}" ShowPrompt="${o.showPrompt ? "true" : "false"}" UpdateBlocksActivation="${o.updateBlocksActivation ? "true" : "false"}" />${o.automaticBackgroundTask === false ? "" : `
    <AutomaticBackgroundTask />`}
  </UpdateSettings>
</AppInstaller>
`;
}
