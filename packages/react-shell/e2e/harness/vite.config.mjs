import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
export default {
  root: here,
  base: "./",
  oxc: { jsx: { runtime: "automatic" } },
  build: { rolldownOptions: { input: { index: join(here, "index.html"), variants: join(here, "variants.html"), hooks: join(here, "hooks.html") } }, outDir: process.env.RK_HARNESS_OUT ?? join(here, "dist"), emptyOutDir: true, minify: false },
  // RK_HARNESS_DEV=1 bundles React's development build, the only one that double-invokes effects under StrictMode.
  ...(process.env.RK_HARNESS_DEV ? { define: { "process.env.NODE_ENV": '"development"' } } : {}),
  resolve: { dedupe: ["react", "react-dom"] },
};
