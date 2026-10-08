import path from "node:path";

// One resolver for the worker that writes receipts & the build that seals them.
export function signingReceiptPath({ appRoot, target, platform, phase, stateRoot }) {
  const configured = platform === "mac"
    ? phase === "signing" ? target.sign?.receipt : target.notarize?.receipt
    : phase === "raw-exe" ? target.sign?.receipt : phase === "installer" ? target.sign?.installerReceipt : undefined;
  if (configured) return path.resolve(appRoot, configured);
  return path.resolve(stateRoot ?? path.join(appRoot, ".right-release", "receipts"), `${platform === "mac" ? "macos" : "windows"}-${phase}.json`);
}
