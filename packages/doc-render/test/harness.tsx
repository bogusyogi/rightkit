import { createRoot, hydrateRoot } from "react-dom/client";
import { App } from "./app.js";
import "../src/styles.css";

const container = document.getElementById("root")!;
if (container.hasAttribute("data-ssr")) hydrateRoot(container, <App />);
else createRoot(container).render(<App />);
(window as unknown as { __harness?: boolean }).__harness = true;
