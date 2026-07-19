import { createElement } from "react";
import { createRoot } from "react-dom/client";
import { LegalGate } from "../src/index.js";
import { findVariant, PREVIEW_VARIANTS } from "./variants.js";

const params = new URLSearchParams(window.location.search);
const id = params.get("id");
const root = document.getElementById("root");
if (!root) throw new Error("preview root element missing");

const variant = id ? findVariant(id) : undefined;

if (!variant) {
  root.innerHTML = `<pre style="font:14px monospace;padding:24px;color:#eee;background:#111">Pass ?id=&lt;appKey&gt;-&lt;mode&gt;\n\nAvailable:\n${PREVIEW_VARIANTS.map((v) => v.id).join("\n")}</pre>`;
} else {
  document.title = `${variant.props.appName} — legal gate (${variant.mode})`;
  createRoot(root).render(createElement(LegalGate, variant.props));
}
