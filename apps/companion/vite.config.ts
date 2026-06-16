import { defineConfig } from "vite";
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
});
