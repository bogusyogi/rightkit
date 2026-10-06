import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { installLogs } from "./lib/logs";
import "./styles.css";

installLogs();
ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
