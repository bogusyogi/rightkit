import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
export default {
  root: here,
  base: "./",
  esbuild: { jsx: "automatic" },
  build: { rollupOptions: { input: { index: join(here, "index.html"), variants: join(here, "variants.html") } }, outDir: process.env.RK_HARNESS_OUT ?? join(here, "dist"), emptyOutDir: true, minify: false },
  resolve: { dedupe: ["react", "react-dom"] },
};
