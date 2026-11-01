import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// Tauri dev server: fixed port (must equal tauri.conf.json build.devUrl), no clearScreen so cargo
// output stays visible.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  server: {
    host: "127.0.0.1",
    port: 5190,
    strictPort: true,
    watch: { ignored: ["**/.cache/**", "**/dist/**", "**/target/**", "**/src-tauri/binaries/**"] },
  },
  build: { outDir: "dist", target: "es2021", emptyOutDir: true },
});
