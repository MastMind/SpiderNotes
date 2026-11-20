import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_"],
  define: {
    __BUILD_SHA__: JSON.stringify(process.env.CI_SHA || "local"),
  },
  build: {
    target: "es2021",
    outDir: "dist",
  },
});
