import { renderToString } from "react-dom/server";
import { App } from "./app.js";

export function renderSsr(): string {
  if (typeof document !== "undefined") throw new Error("SSR harness must run without a DOM");
  return renderToString(<App />);
}
