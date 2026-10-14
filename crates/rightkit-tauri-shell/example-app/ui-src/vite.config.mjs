// Builds ui/appshell.js (+ appshell.css) from main.tsx using the workspace's own app-shell, platform-ui
// and tauri-shell packages. Dev-time only; the committed output is what the example serves.
import path from "node:path";
const pk = path.resolve(import.meta.dirname, "../../../../packages");
export default {
  esbuild: { jsx: "automatic" },
  root: import.meta.dirname,
  define: { "process.env.NODE_ENV": '"production"' },
  resolve: {
    alias: {
      "@rightkit/app-shell/react": path.join(pk, "app-shell/dist/react.js"),
      "@rightkit/app-shell": path.join(pk, "app-shell/dist/index.js"),
      "@rightkit/platform-ui/react": path.join(pk, "platform-ui/dist/react.js"),
      "@rightkit/platform-ui": path.join(pk, "platform-ui/dist/index.js"),
      "@rightkit/tauri-shell": path.join(pk, "tauri-shell/dist/index.js"),
      react: path.join(pk, "app-shell/node_modules/react"),
      "react-dom": path.join(pk, "app-shell/node_modules/react-dom"),
    },
  },
  build: {
    outDir: path.resolve(import.meta.dirname, "../ui"),
    emptyOutDir: false,
    cssCodeSplit: false,
    lib: { entry: path.join(import.meta.dirname, "main.tsx"), formats: ["es"], fileName: () => "appshell.js", cssFileName: "appshell" },
  },
};
