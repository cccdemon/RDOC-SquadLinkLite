import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import App from "./App";
import Overlay from "./Overlay";
import "./style.css";

// One bundle, two windows: the "overlay" window renders the minimal channel
// overlay; every other window renders the full app.
let isOverlay = false;
try {
  isOverlay = getCurrentWindow().label === "overlay";
} catch {
  /* not in Tauri (dev in a browser) → full app */
}
if (isOverlay) {
  // Transparent, borderless overlay window — kill the app's page background.
  document.documentElement.classList.add("overlay-mode");
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>{isOverlay ? <Overlay /> : <App />}</React.StrictMode>,
);
