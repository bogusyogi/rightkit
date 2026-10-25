import React from "react";
import ReactDOM from "react-dom/client";
import { loadPlatform } from "@rightkit/platform-ui";
import { applyStoredThemeSync } from "@rightkit/react-shell";
import "@rightkit/react-shell/shell.css";
import App from "./App";
import { installLogs } from "./lib/logs";
import { shellReady } from "./lib/shell";
import "./styles.css";

// getPlatform() returns the browser's report until the native platform loads, and the app reads its platform once at
// render, so resolve the native value first. The main window starts hidden (tauri.conf.json): shellReady() reveals it.
async function start() {
  applyStoredThemeSync({ storageKey: "rightapp.theme" });
  await loadPlatform();
  installLogs();
  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
  await shellReady();
}

void start();
