import { defineConfig } from "vite";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
const here = dirname(fileURLToPath(import.meta.url));
export default defineConfig({ root: here, oxc: { jsx: { runtime: "automatic" } }, resolve: { dedupe: ["react", "react-dom"] }, build: { outDir: resolve(here, "../dist"), emptyOutDir: true } });
