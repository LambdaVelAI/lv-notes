import { defineConfig } from "vite";

// Tauri expects a fixed port and its own env prefix.
export default defineConfig({
  clearScreen: false,
  // The browser preview loads the core's MyST fixtures from the repository.
  server: { port: 1420, strictPort: true, watch: { ignored: ["**/src-tauri/**"] }, fs: { allow: ["../.."] } },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: { target: "safari16", outDir: "dist" },
});
