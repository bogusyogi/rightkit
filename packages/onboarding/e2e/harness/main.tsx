import { StrictMode, useLayoutEffect } from "react";
import { createRoot } from "react-dom/client";
import { Onboarding, type LicenseBackend, type ModelsBackend, type PermissionsBackend, type SettingsBackend } from "../../src/index.js";
import "../../../app-shell/shell.css";
import "../../onboarding.css";

const state: { values: Record<string, unknown>; revision: number } = { values: {}, revision: 1 };
const query = new URLSearchParams(location.search);
const failSave = query.get("failsave") === "1";
const SEEDS: Record<string, Record<string, unknown>> = {
  "gate-step2": { version: 1, currentStep: 2, completed: ["legal"], skipped: false, legalAccepted: false },
  last: { version: 1, currentStep: 3, completed: ["legal", "permissions", "models"], skipped: false, legalAccepted: true },
};
const seed = query.get("seed");
if (seed && SEEDS[seed]) localStorage.setItem("rk-onboarding-settings", JSON.stringify({ values: { "onboarding.state": SEEDS[seed] }, revision: 1 }));
const readState = () => { try { const raw = localStorage.getItem("rk-onboarding-settings"); return raw ? JSON.parse(raw) as typeof state : state; } catch { return state; } };
const settings: SettingsBackend = { load: async () => { const current = readState(); return { values: { ...current.values }, revision: current.revision }; }, patch: async (patch) => { if (failSave) throw new Error("disk full: settings not saved"); const current = readState(); for (const [key, value] of Object.entries(patch)) value === null ? delete current.values[key] : current.values[key] = value; current.revision += 1; localStorage.setItem("rk-onboarding-settings", JSON.stringify(current)); return { values: { ...current.values }, revision: current.revision }; } };
const permissions: PermissionsBackend = { list: async () => [{ id: "microphone", label: "Microphone", description: "Used for voice input.", state: "needed" }], request: async (id) => ({ id, label: "Microphone", description: "Used for voice input.", state: "ready" }) };
const models: ModelsBackend = { list: async () => [{ id: "small", label: "Speech model", description: "Fast local recognition", sizeBytes: 120_000_000, state: "missing" }], install: async () => {}, cancel: async () => {} };
const license: LicenseBackend = { load: async () => ({ status: "none", detail: "No licence is active." }), startTrial: async () => ({ status: "trial", label: "Trial", detail: "Trial active." }) };
function App() {
  useLayoutEffect(() => {
    (window as unknown as { __ready: boolean }).__ready = true;
  }, []);
  return <Onboarding settings={settings} permissions={permissions} models={models} license={license} legal={{ appName: "AcmeRight", trustLine: "Your data stays under your control.", documents: [{ title: "Terms of Service", url: "https://example.test/terms" }, { title: "Privacy Policy", url: "https://example.test/privacy" }], agreementStatement: "I agree to these terms for this device.", theme: { bg: "#fffcf5", surface: "#ffffff", border: "#ddd5c8", text: "#231f1a", muted: "#6b655c", accent: "#7040c4", accentContrast: "#ffffff", fontDisplay: "system-ui", fontBody: "system-ui" }, onQuit: () => {}, }} onComplete={() => { (window as unknown as { __done?: boolean }).__done = true; }} onSkip={() => { (window as unknown as { __skipped?: boolean }).__skipped = true; }} />;
}
createRoot(document.getElementById("root")!).render(<StrictMode><App /></StrictMode>);
