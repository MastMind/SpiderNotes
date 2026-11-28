import { readFileSync } from "node:fs";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const pkg = JSON.parse(
  readFileSync(new URL("./package.json", import.meta.url), "utf8")
);

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
    __BUILD_VERSION__: JSON.stringify(pkg.version),
  },
  build: {
    target: "es2021",
    outDir: "dist",
  },
});
