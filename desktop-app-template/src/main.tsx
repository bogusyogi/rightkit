import React from "react";
import ReactDOM from "react-dom/client";
import { loadPlatform } from "@rightkit/platform-ui";
import "@rightkit/app-shell/shell.css";
import App from "./App";
import { installLogs } from "./lib/logs";
import "./styles.css";

// platform-ui 0.2.1+: getPlatform() returns the browser's report until the native platform loads.
// The app reads its platform once at render, so resolve the native value first (0.2.7 guidance).
async function start() {
  await loadPlatform();
  installLogs();
  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
}

void start();
