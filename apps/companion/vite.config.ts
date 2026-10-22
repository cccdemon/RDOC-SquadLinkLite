import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

// Tauri expects a fixed port and no screen clearing.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  // Tauri ships a modern webview (WebView2 / WKWebView / webkit2gtk) on every
  // target, so emit modern JS and skip down-leveling. esbuild 0.28+ errors when
  // asked to lower some syntax to Vite's default "modules" target — esnext
  // avoids those transforms entirely.
  build: { target: "esnext" },
  // Unit tests for the pure helpers in src/lib (`pnpm test`). They run under
  // plain node — no DOM, no Tauri IPC — so anything that needs either stays in
  // App.tsx and out of these modules.
  test: { environment: "node", include: ["src/**/*.test.ts"] },
});
