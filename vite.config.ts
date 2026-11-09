import { defineConfig } from "vite";

// Tauri espera un puerto fijo en desarrollo.
export default defineConfig({
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: { target: "es2021", outDir: "dist" },
});
