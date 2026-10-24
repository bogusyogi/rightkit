import React from "react";
import ReactDOM from "react-dom/client";
import { loadPlatform } from "@rightkit/platform-ui";
import App from "./App";
import { initDevice } from "./lib/device";
import { installLogs } from "./lib/logs";
import "./styles.css";

// getPlatform() returns the browser's report until the native platform loads, and the app reads its
// platform once at render, so resolve the native value first (platform-ui guidance).
async function start() {
  await loadPlatform();
  installLogs();
  void initDevice();
  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
}

void start();
