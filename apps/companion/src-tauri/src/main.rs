// Prevents an extra console window on Windows in release builds. DO NOT REMOVE.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// Desktop entry point. All app logic lives in the library crate so the same
// `run()` is shared with the mobile (Android/iOS) entry point that Tauri
// generates via `#[tauri::mobile_entry_point]`.
fn main() {
    rdoc_squadlink_lite_lib::run()
}
