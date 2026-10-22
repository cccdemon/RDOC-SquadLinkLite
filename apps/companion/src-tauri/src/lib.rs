//! Tauri shell for subraum. Thin layer over companion-core: commands
//! drive the engine, engine state is forwarded to the webview as "ui" events.
//!
//! Entry point is `run()` — desktop `main.rs` calls it directly; on mobile
//! (Android/iOS) Tauri generates the platform entry that invokes it via
//! `#[tauri::mobile_entry_point]`.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use companion_core::serverless::Serverless;
use companion_core::{start, Engine, EngineConfig, Sink, UiEvent};
use tauri::{AppHandle, Emitter, Manager, State};

mod control;

struct AppState {
    engine: Mutex<Option<Engine>>,
    serverless: Mutex<Option<Arc<Serverless>>>,
    /// Local control API (Stream Deck etc.); None until the server is up.
    control: Mutex<Option<Arc<control::ControlServer>>>,
}

// ── Configurable PTT via Windows Raw Input (keyboard + mouse buttons) ─────────
// Raw Input (RIDEV_INPUTSINK) is delivered straight off the device stack, so it
// keeps firing while a fullscreen / elevated game (e.g. Star Citizen) holds the
// foreground — unlike a WH_KEYBOARD_LL hook, which UIPI blocks for elevated apps.
static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();
// Deep link the app was *launched* with (cold start). The webview drains this
// via `take_pending_deeplink` on mount, since a Rust-side emit at startup races
// ahead of the JS event listener and would be dropped.
static PENDING_DEEPLINK: OnceLock<Mutex<Option<String>>> = OnceLock::new();
// Two independent PTT bindings (slots 0 + 1). Holding EITHER transmits — some
// users want a redundant key / mouse button / gamepad trigger. Default slot 0 =
// F8, slot 1 unset. Codes: keyboard "F8"/"KeyR", "Mouse:Left", gamepad "Pad:N".
static PTT_BINDINGS: OnceLock<Mutex<[Option<String>; 2]>> = OnceLock::new();
static PTT_CAPTURE_SLOT: AtomicI32 = AtomicI32::new(-1); // -1 idle, 0/1 = capturing slot
static PTT_SLOT_DOWN: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];

/// Transmit state merged from every PTT source: the keyboard/mouse/pad
/// bindings (`key`) and the local control API, e.g. Stream Deck (`ctl`).
/// Transmitting while EITHER is held; `emitted` is what the webview last saw.
#[derive(Default)]
struct PttSources {
    key: bool,
    ctl: bool,
    emitted: bool,
}

impl PttSources {
    /// Record the sources that changed (`None` = unchanged). Returns
    /// `Some(combined)` only when the combined state differs from the last
    /// one handed out, so a source releasing while the other still holds
    /// emits nothing.
    fn update(&mut self, key: Option<bool>, ctl: Option<bool>) -> Option<bool> {
        if let Some(k) = key {
            self.key = k;
        }
        if let Some(c) = ctl {
            self.ctl = c;
        }
        let combined = self.key || self.ctl;
        if combined == self.emitted {
            return None;
        }
        self.emitted = combined;
        Some(combined)
    }
}

static PTT_SOURCES: Mutex<PttSources> = Mutex::new(PttSources { key: false, ctl: false, emitted: false });

/// Merge a source change into the combined PTT state and tell the webview
/// when it flips. The lock is held across the emit so edges reach the
/// webview in the order they were merged.
fn publish_ptt(key: Option<bool>, ctl: Option<bool>) {
    let mut src = PTT_SOURCES.lock().unwrap();
    if let Some(combined) = src.update(key, ctl) {
        if let Some(app) = APP_HANDLE.get() {
            let _ = app.emit("ptt", combined);
        }
    }
}

// Channel-cycle hotkeys (slot 0 = prev, slot 1 = next). A press edge emits
// `chan-cycle` (-1 / +1); the webview steps through the session's channel list.
// Default unset — the user binds them in settings.
static CHAN_BINDINGS: OnceLock<Mutex<[Option<String>; 2]>> = OnceLock::new();
static CHAN_CAPTURE_SLOT: AtomicI32 = AtomicI32::new(-1);
static CHAN_SLOT_DOWN: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];
// True iff at least one channel-cycle key is bound. Lets `handle_raw` skip the
// channel block entirely (no lock, no loop) on every input event when unused.
static CHAN_ANY_BOUND: AtomicBool = AtomicBool::new(false);

fn ptt_bindings() -> &'static Mutex<[Option<String>; 2]> {
    PTT_BINDINGS.get_or_init(|| Mutex::new([Some("F8".into()), None]))
}
fn chan_bindings() -> &'static Mutex<[Option<String>; 2]> {
    CHAN_BINDINGS.get_or_init(|| Mutex::new([None, None]))
}
/// Recompute the "any channel key bound" fast-path flag after a binding change.
fn refresh_chan_bound() {
    let any = chan_bindings().lock().unwrap().iter().any(|x| x.is_some());
    CHAN_ANY_BOUND.store(any, Ordering::Relaxed);
}

// ── Modifier chords (Ctrl / Alt / Shift / Meta + a base key) ──────────────────
// Live modifier state, updated from every keyboard edge. A binding may be a plain
// key ("F8") or a chord ("Shift+KeyT", "Ctrl+Shift+Mouse:Left"); a chord fires
// only while its required modifiers are held.
//
// The live state and `handle_raw` are fed by the Windows Raw Input thread only,
// so they are `cfg(windows)`. The pure chord helpers (`Mods`, `parse_binding`,
// `build_combo`, `mods_ok`, `mod_of_label`) are platform-neutral and
// unit-tested everywhere; on other targets nothing calls them at runtime.
#[cfg(windows)]
static MOD_SHIFT: AtomicBool = AtomicBool::new(false);
#[cfg(windows)]
static MOD_CTRL: AtomicBool = AtomicBool::new(false);
#[cfg(windows)]
static MOD_ALT: AtomicBool = AtomicBool::new(false);
#[cfg(windows)]
static MOD_META: AtomicBool = AtomicBool::new(false);

#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Mods {
    shift: bool,
    ctrl: bool,
    alt: bool,
    meta: bool,
}

/// Which modifier group a raw key label belongs to (None = not a modifier).
#[cfg_attr(not(windows), allow(dead_code))]
fn mod_of_label(code: &str) -> Option<&'static str> {
    match code {
        "ShiftLeft" | "ShiftRight" => Some("Shift"),
        "ControlLeft" | "ControlRight" => Some("Ctrl"),
        "Alt" | "AltGr" => Some("Alt"),
        "MetaLeft" | "MetaRight" => Some("Meta"),
        _ => None,
    }
}

#[cfg(windows)]
fn held_mods() -> Mods {
    Mods {
        shift: MOD_SHIFT.load(Ordering::SeqCst),
        ctrl: MOD_CTRL.load(Ordering::SeqCst),
        alt: MOD_ALT.load(Ordering::SeqCst),
        meta: MOD_META.load(Ordering::SeqCst),
    }
}

/// All required modifiers held? (Extra held modifiers are tolerated.)
#[cfg_attr(not(windows), allow(dead_code))]
fn mods_ok(req: Mods, held: Mods) -> bool {
    (!req.shift || held.shift)
        && (!req.ctrl || held.ctrl)
        && (!req.alt || held.alt)
        && (!req.meta || held.meta)
}

/// Split a binding string into its required modifiers + base key.
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_binding(s: &str) -> (Mods, String) {
    let mut m = Mods::default();
    let mut rest = s;
    while let Some((tok, r)) = rest.split_once('+') {
        match tok {
            "Shift" => m.shift = true,
            "Ctrl" => m.ctrl = true,
            "Alt" => m.alt = true,
            "Meta" => m.meta = true,
            _ => break, // not a modifier → part of the base key
        }
        rest = r;
    }
    (m, rest.to_string())
}

/// Build a canonical chord label from the held modifiers + a base key.
#[cfg_attr(not(windows), allow(dead_code))]
fn build_combo(base: &str, m: Mods) -> String {
    let mut s = String::new();
    if m.ctrl {
        s.push_str("Ctrl+");
    }
    if m.alt {
        s.push_str("Alt+");
    }
    if m.shift {
        s.push_str("Shift+");
    }
    if m.meta {
        s.push_str("Meta+");
    }
    s.push_str(base);
    s
}

/// Process one raw key/mouse edge. In capture mode the next press becomes the
/// new binding (emitted as `ptt-bound`); otherwise edges on the bound code
/// toggle transmit via the `ptt` event. De-duped so key auto-repeat (and
/// duplicate button flags in one packet) don't spam the IPC channel.
#[cfg(windows)]
fn handle_raw(code: String, down: bool) {
    // Track modifier state first (chords gate on it). Modifier keys can still be
    // a plain binding on their own.
    match mod_of_label(&code) {
        Some("Shift") => MOD_SHIFT.store(down, Ordering::SeqCst),
        Some("Ctrl") => MOD_CTRL.store(down, Ordering::SeqCst),
        Some("Alt") => MOD_ALT.store(down, Ordering::SeqCst),
        Some("Meta") => MOD_META.store(down, Ordering::SeqCst),
        _ => {}
    }
    let is_mod = mod_of_label(&code).is_some();

    // ── Capture: the first non-modifier press binds the slot as a chord ──────
    let cap = PTT_CAPTURE_SLOT.load(Ordering::SeqCst);
    if cap >= 0 {
        if down && !is_mod {
            PTT_CAPTURE_SLOT.store(-1, Ordering::SeqCst);
            let slot = (cap as usize).min(1);
            let combo = build_combo(&code, held_mods());
            PTT_SLOT_DOWN[slot].store(false, Ordering::SeqCst);
            ptt_bindings().lock().unwrap()[slot] = Some(combo.clone());
            if let Some(app) = APP_HANDLE.get() {
                let _ = app.emit("ptt-bound", serde_json::json!({ "slot": slot, "code": combo }));
            }
        }
        return;
    }
    let ccap = CHAN_CAPTURE_SLOT.load(Ordering::SeqCst);
    if ccap >= 0 {
        if down && !is_mod {
            CHAN_CAPTURE_SLOT.store(-1, Ordering::SeqCst);
            let slot = (ccap as usize).min(1);
            let combo = build_combo(&code, held_mods());
            CHAN_SLOT_DOWN[slot].store(false, Ordering::SeqCst);
            chan_bindings().lock().unwrap()[slot] = Some(combo.clone());
            refresh_chan_bound();
            if let Some(app) = APP_HANDLE.get() {
                let _ = app.emit("chan-bound", serde_json::json!({ "slot": slot, "code": combo }));
            }
        }
        return;
    }

    let held = held_mods();

    // ── PTT: transmit while a bound chord's base key is held AND its modifiers
    // are held. Recomputed on EVERY edge (incl. modifier edges), so releasing a
    // required modifier also stops transmit. ─────────────────────────────────
    let combined = {
        let b = ptt_bindings().lock().unwrap();
        for i in 0..2 {
            if let Some(bind) = b[i].as_deref() {
                let (_m, base) = parse_binding(bind);
                if base == code {
                    PTT_SLOT_DOWN[i].store(down, Ordering::SeqCst);
                }
            }
        }
        (0..2).any(|i| {
            b[i]
                .as_deref()
                .map(|bind| {
                    let (m, _base) = parse_binding(bind);
                    PTT_SLOT_DOWN[i].load(Ordering::SeqCst) && mods_ok(m, held)
                })
                .unwrap_or(false)
        })
    };
    publish_ptt(Some(combined), None);

    // ── Channel cycle: fire once on the base's press edge with modifiers held.
    // Fast-path: skip the lock + loop entirely when no cycle key is bound. ────
    if CHAN_ANY_BOUND.load(Ordering::Relaxed) {
        let b = chan_bindings().lock().unwrap();
        for i in 0..2 {
            if let Some(bind) = b[i].as_deref() {
                let (m, base) = parse_binding(bind);
                if base == code {
                    let was = CHAN_SLOT_DOWN[i].swap(down, Ordering::SeqCst);
                    if down && !was && mods_ok(m, held) {
                        if let Some(app) = APP_HANDLE.get() {
                            let _ = app.emit("chan-cycle", if i == 0 { -1 } else { 1 });
                        }
                    }
                }
            }
        }
    }
}

#[cfg(windows)]
fn start_raw_input() {
    std::thread::spawn(|| unsafe { raw_input::run() });
}

/// Global PTT capture is Windows-only; elsewhere the in-app PTT button still works.
#[cfg(not(windows))]
fn start_raw_input() {}

#[cfg(windows)]
mod raw_input {
    use super::handle_raw;
    use std::collections::HashMap;
    use std::ptr::null_mut;
    use std::sync::{Mutex, OnceLock};
    use windows_sys::Win32::Devices::HumanInterfaceDevice::{
        HidP_GetButtonCaps, HidP_GetCaps, HidP_GetUsages, HidP_MaxUsageListLength, HidP_Input,
        HIDP_BUTTON_CAPS, HIDP_CAPS,
    };
    use windows_sys::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Input::{
        GetRawInputData, GetRawInputDeviceInfoW, RegisterRawInputDevices, HRAWINPUT, RAWINPUT,
        RAWINPUTDEVICE, RAWINPUTHEADER, RID_INPUT, RIDEV_INPUTSINK, RIM_TYPEKEYBOARD, RIM_TYPEHID,
        RIM_TYPEMOUSE,
    };

    // NTSTATUS success + the GetRawInputDeviceInfoW command for preparsed HID data
    // (defined locally to avoid depending on their exact windows-sys export path).
    const HIDP_STATUS_SUCCESS: i32 = 0x0011_0000;
    const RIDI_PREPARSEDDATA: u32 = 0x2000_0005;

    // Per-device caches: HID preparsed descriptor + last-pressed button usages,
    // keyed by the raw-input device HANDLE (as usize). Used to diff button edges.
    static PREPARSED: OnceLock<Mutex<HashMap<usize, Vec<u8>>>> = OnceLock::new();
    static HID_PRESSED: OnceLock<Mutex<HashMap<usize, Vec<u16>>>> = OnceLock::new();
    fn preparsed_cache() -> &'static Mutex<HashMap<usize, Vec<u8>>> {
        PREPARSED.get_or_init(|| Mutex::new(HashMap::new()))
    }
    fn pressed_cache() -> &'static Mutex<HashMap<usize, Vec<u16>>> {
        HID_PRESSED.get_or_init(|| Mutex::new(HashMap::new()))
    }
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW,
        TranslateMessage, HWND_MESSAGE, MSG, WM_INPUT, WNDCLASSW,
    };

    const RI_KEY_BREAK: u16 = 0x01; // RAWKEYBOARD.Flags bit 0 = key up
    const MOUSE_BTN: [(u16, &str, bool); 10] = [
        (0x0001, "Mouse:Left", true),
        (0x0002, "Mouse:Left", false),
        (0x0004, "Mouse:Right", true),
        (0x0008, "Mouse:Right", false),
        (0x0010, "Mouse:Middle", true),
        (0x0020, "Mouse:Middle", false),
        (0x0040, "Mouse:Unknown(1)", true), // XBUTTON1 = Mouse4
        (0x0080, "Mouse:Unknown(1)", false),
        (0x0100, "Mouse:Unknown(2)", true), // XBUTTON2 = Mouse5
        (0x0200, "Mouse:Unknown(2)", false),
    ];

    /// rdev-compatible labels so previously saved bindings (default "F8") match.
    fn vk_label(vk: u16) -> Option<String> {
        let s: &str = match vk {
            0x08 => "Backspace",
            0x09 => "Tab",
            0x0D => "Return",
            0x13 => "Pause",
            0x14 => "CapsLock",
            0x1B => "Escape",
            0x20 => "Space",
            0x21 => "PageUp",
            0x22 => "PageDown",
            0x23 => "End",
            0x24 => "Home",
            0x25 => "LeftArrow",
            0x26 => "UpArrow",
            0x27 => "RightArrow",
            0x28 => "DownArrow",
            0x2D => "Insert",
            0x2E => "Delete",
            0x10 => "ShiftLeft",
            0x11 => "ControlLeft",
            0x12 => "Alt",
            0xA0 => "ShiftLeft",
            0xA1 => "ShiftRight",
            0xA2 => "ControlLeft",
            0xA3 => "ControlRight",
            0xA4 => "Alt",
            0xA5 => "AltGr",
            0x5B => "MetaLeft",
            0x5C => "MetaRight",
            0x90 => "NumLock",
            0x91 => "ScrollLock",
            0x6A => "KpMultiply",
            0x6B => "KpPlus",
            0x6D => "KpMinus",
            0x6E => "KpDelete",
            0x6F => "KpDivide",
            0xBA => "SemiColon",
            0xBB => "Equal",
            0xBC => "Comma",
            0xBD => "Minus",
            0xBE => "Dot",
            0xBF => "Slash",
            0xC0 => "BackQuote",
            0xDB => "LeftBracket",
            0xDC => "BackSlash",
            0xDD => "RightBracket",
            0xDE => "Quote",
            0x30..=0x39 => return Some(format!("Num{}", vk - 0x30)), // top-row digits
            0x41..=0x5A => return Some(format!("Key{}", (vk as u8 - 0x41 + b'A') as char)),
            0x60..=0x69 => return Some(format!("Kp{}", vk - 0x60)), // numpad digits
            0x70..=0x87 => return Some(format!("F{}", vk - 0x70 + 1)), // F1..F24
            _ => return Some(format!("Vk{vk}")),
        };
        Some(s.to_string())
    }

    pub unsafe fn run() {
        let hinstance = GetModuleHandleW(null_mut());
        let class_name: Vec<u16> = "subraumRawInput\0".encode_utf16().collect();
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinstance;
        wc.lpszClassName = class_name.as_ptr();
        RegisterClassW(&wc);

        // Message-only window: receives WM_INPUT but never shows / takes focus.
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            null_mut(),
            hinstance,
            null_mut(),
        );
        if hwnd.is_null() {
            return;
        }

        let devices = [
            // Generic-desktop / keyboard (usage page 0x01, usage 0x06).
            RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x06, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd },
            // Generic-desktop / mouse (usage page 0x01, usage 0x02).
            RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x02, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd },
            // Generic-desktop / joystick (0x01/0x04) + gamepad (0x01/0x05) for PTT.
            RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x04, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd },
            RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x05, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd },
        ];
        RegisterRawInputDevices(
            devices.as_ptr(),
            devices.len() as u32,
            std::mem::size_of::<RAWINPUTDEVICE>() as u32,
        );

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if msg == WM_INPUT {
            process_input(lparam as HRAWINPUT);
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    unsafe fn process_input(hri: HRAWINPUT) {
        let header_size = std::mem::size_of::<RAWINPUTHEADER>() as u32;
        let mut size: u32 = 0;
        if GetRawInputData(hri, RID_INPUT, null_mut(), &mut size, header_size) != 0 || size == 0 {
            return;
        }
        let mut buf = vec![0u8; size as usize];
        let got = GetRawInputData(hri, RID_INPUT, buf.as_mut_ptr() as *mut _, &mut size, header_size);
        if got == u32::MAX || (got as usize) < std::mem::size_of::<RAWINPUTHEADER>() {
            return;
        }
        let ri = &*(buf.as_ptr() as *const RAWINPUT);
        match ri.header.dwType {
            t if t == RIM_TYPEKEYBOARD => {
                let kb = &ri.data.keyboard;
                if kb.VKey == 0 || kb.VKey == 0xFF {
                    return; // fake / overrun key
                }
                let down = kb.Flags & RI_KEY_BREAK == 0;
                if let Some(code) = vk_label(kb.VKey) {
                    handle_raw(code, down);
                }
            }
            t if t == RIM_TYPEMOUSE => {
                let flags = ri.data.mouse.Anonymous.Anonymous.usButtonFlags;
                for (mask, code, down) in MOUSE_BTN {
                    if flags & mask != 0 {
                        handle_raw(code.to_string(), down);
                    }
                }
                // Scroll wheels (also VKB/HOTAS thumb encoders that present as a
                // mouse wheel). A wheel tick is momentary → emit a press pulse so
                // it can trigger a channel-cycle. usButtonData is a signed delta.
                const RI_MOUSE_WHEEL: u16 = 0x0400;
                const RI_MOUSE_HWHEEL: u16 = 0x0800;
                if flags & (RI_MOUSE_WHEEL | RI_MOUSE_HWHEEL) != 0 {
                    let delta = ri.data.mouse.Anonymous.Anonymous.usButtonData as i16;
                    let code = if flags & RI_MOUSE_WHEEL != 0 {
                        if delta > 0 { "Mouse:WheelUp" } else { "Mouse:WheelDown" }
                    } else if delta > 0 {
                        "Mouse:WheelRight"
                    } else {
                        "Mouse:WheelLeft"
                    };
                    handle_raw(code.to_string(), true);
                    handle_raw(code.to_string(), false);
                }
            }
            t if t == RIM_TYPEHID => process_hid(ri),
            _ => {}
        }
    }

    /// Fetch (and cache) the HID preparsed descriptor for a device.
    unsafe fn get_preparsed(hdev: HANDLE) -> Vec<u8> {
        let key = hdev as usize;
        if let Some(v) = preparsed_cache().lock().unwrap().get(&key) {
            return v.clone();
        }
        let mut size: u32 = 0;
        if GetRawInputDeviceInfoW(hdev, RIDI_PREPARSEDDATA, null_mut(), &mut size) != 0 || size == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u8; size as usize];
        let got = GetRawInputDeviceInfoW(hdev, RIDI_PREPARSEDDATA, buf.as_mut_ptr() as *mut _, &mut size);
        if got == u32::MAX {
            return Vec::new();
        }
        preparsed_cache().lock().unwrap().insert(key, buf.clone());
        buf
    }

    /// Parse a gamepad/joystick HID packet: extract the currently-pressed button
    /// usages via HidP and emit press/release edges as "Pad:<usage>" codes.
    unsafe fn process_hid(ri: &RAWINPUT) {
        let hdev = ri.header.hDevice;
        let hid = &ri.data.hid;
        let size = hid.dwSizeHid as usize;
        let count = hid.dwCount as usize;
        if size == 0 || count == 0 {
            return;
        }
        let pp = get_preparsed(hdev);
        if pp.is_empty() {
            return;
        }
        let pp_ptr = pp.as_ptr();
        let mut caps: HIDP_CAPS = std::mem::zeroed();
        if HidP_GetCaps(pp_ptr as _, &mut caps) != HIDP_STATUS_SUCCESS || caps.NumberInputButtonCaps == 0 {
            return;
        }
        let mut bcaps = vec![std::mem::zeroed::<HIDP_BUTTON_CAPS>(); caps.NumberInputButtonCaps as usize];
        let mut blen = caps.NumberInputButtonCaps;
        if HidP_GetButtonCaps(HidP_Input, bcaps.as_mut_ptr(), &mut blen, pp_ptr as _) != HIDP_STATUS_SUCCESS
            || blen == 0
        {
            return;
        }
        let usage_page = bcaps[0].UsagePage;
        let maxn = HidP_MaxUsageListLength(HidP_Input, usage_page, pp_ptr as _);
        if maxn == 0 {
            return;
        }
        let base = hid.bRawData.as_ptr();
        for r in 0..count {
            let report = base.add(r * size) as *mut u8;
            let mut usages = vec![0u16; maxn as usize];
            let mut ulen = maxn;
            if HidP_GetUsages(HidP_Input, usage_page, 0, usages.as_mut_ptr(), &mut ulen, pp_ptr as _, report as _, size as u32)
                != HIDP_STATUS_SUCCESS
            {
                continue;
            }
            usages.truncate(ulen as usize);
            diff_buttons(hdev as usize, &usages);
        }
    }

    /// Emit press/release edges by diffing this report's pressed usages against
    /// the device's previous set.
    fn diff_buttons(key: usize, now: &[u16]) {
        let prev = pressed_cache().lock().unwrap().get(&key).cloned().unwrap_or_default();
        for u in now {
            if !prev.contains(u) {
                handle_raw(format!("Pad:{u}"), true);
            }
        }
        for u in &prev {
            if !now.contains(u) {
                handle_raw(format!("Pad:{u}"), false);
            }
        }
        pressed_cache().lock().unwrap().insert(key, now.to_vec());
    }
}

// ── Audio ducking: lower other apps' volume while subraum voice is active ───
// Windows-only (WASAPI per-session volume). The frontend drives `set_ducking`
// from the combined "I'm transmitting OR a peer is speaking" state.
#[cfg(windows)]
mod ducking {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::mpsc::Sender;
    use std::sync::{Mutex, OnceLock};
    use windows::core::Interface;
    use windows::Win32::Media::Audio::{
        eMultimedia, eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
        ISimpleAudioVolume, MMDeviceEnumerator,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    // How far to lower other apps, in percent (0 = unchanged, 100 = silent).
    // 75 keeps the previous hardcoded behaviour (drop to 25% of original).
    static DUCK_PERCENT: AtomicU32 = AtomicU32::new(75);
    // Whether ducking is applied right now, so an amount change can re-apply live.
    static ACTIVE: AtomicBool = AtomicBool::new(false);

    fn factor() -> f32 {
        1.0 - DUCK_PERCENT.load(Ordering::Relaxed).min(100) as f32 / 100.0
    }

    // pid -> original master volume, captured the moment ducking turns on.
    fn saved() -> &'static Mutex<HashMap<u32, f32>> {
        static S: OnceLock<Mutex<HashMap<u32, f32>>> = OnceLock::new();
        S.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Set how much other apps get lowered. Re-applies immediately if voice is
    /// already active, so dragging the slider is audible while someone talks.
    /// Safe because `run` scales from the *saved* original, not the current level.
    pub fn set_amount(percent: u32) {
        DUCK_PERCENT.store(percent.min(100), Ordering::Relaxed);
        if ACTIVE.load(Ordering::Relaxed) {
            set(true);
        }
    }

    /// Lower (active) or restore (inactive) every OTHER app's audio session.
    /// Edges go to ONE long-lived worker thread (MTA, so it never disturbs
    /// Tauri's apartment) and are applied strictly in order — a release can
    /// never be overtaken by an earlier press, as separate threads could.
    pub fn set(active: bool) {
        ACTIVE.store(active, Ordering::Relaxed);
        let _ = sender().lock().unwrap().send(active);
    }

    /// Queue into the worker; spawns it (once) on first use.
    fn sender() -> &'static Mutex<Sender<bool>> {
        static TX: OnceLock<Mutex<Sender<bool>>> = OnceLock::new();
        TX.get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel::<bool>();
            std::thread::spawn(move || unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                for active in rx {
                    let _ = run(active);
                }
            });
            Mutex::new(tx)
        })
    }

    /// Apply one ducking edge. Must run on the worker thread (COM is
    /// initialised there once, not per call).
    unsafe fn run(active: bool) -> windows::core::Result<()> {
        let enumr: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let dev = enumr.GetDefaultAudioEndpoint(eRender, eMultimedia)?;
        let mgr: IAudioSessionManager2 = dev.Activate(CLSCTX_ALL, None)?;
        let sessions = mgr.GetSessionEnumerator()?;
        let count = sessions.GetCount()?;
        let me = std::process::id();
        for i in 0..count {
            let ctrl = sessions.GetSession(i)?;
            let ctrl2: IAudioSessionControl2 = ctrl.cast()?;
            let pid = ctrl2.GetProcessId().unwrap_or(0);
            if pid == 0 || pid == me {
                continue; // skip the system session and our own
            }
            let vol: ISimpleAudioVolume = ctrl.cast()?;
            if active {
                let cur = vol.GetMasterVolume()?;
                let orig = *saved().lock().unwrap().entry(pid).or_insert(cur);
                let _ = vol.SetMasterVolume(orig * factor(), std::ptr::null());
            } else if let Some(orig) = saved().lock().unwrap().remove(&pid) {
                let _ = vol.SetMasterVolume(orig, std::ptr::null());
            }
        }
        if !active {
            saved().lock().unwrap().clear();
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod ducking {
    pub fn set(_active: bool) {}
    pub fn set_amount(_percent: u32) {}
}

/// Duck/restore other apps' audio. No-op off Windows.
#[tauri::command]
fn set_ducking(active: bool) {
    ducking::set(active);
}

/// How much to lower other apps while voice is active, in percent
/// (0 = unchanged, 100 = silent). Clamped Rust-side.
#[tauri::command]
fn set_duck_amount(percent: u32) {
    ducking::set_amount(percent);
}

/// A binding code as the webview sends it ("F8", "Mouse:Left", "Pad:3",
/// "Ctrl+Shift+KeyT"): kept only when non-empty and bounded, else unbound.
fn binding_code(code: Option<String>) -> Option<String> {
    code.filter(|c| !c.is_empty() && c.len() <= 64)
}

#[tauri::command]
fn set_ptt_binding(slot: usize, code: Option<String>) {
    if slot >= 2 {
        return;
    }
    let code = binding_code(code);
    PTT_SLOT_DOWN[slot].store(false, Ordering::SeqCst); // clear any stuck state on rebind
    ptt_bindings().lock().unwrap()[slot] = code;
}

#[tauri::command]
fn start_ptt_capture(slot: usize) {
    if slot < 2 {
        PTT_CAPTURE_SLOT.store(slot as i32, Ordering::SeqCst);
    }
}

/// Bind a channel-cycle hotkey: slot 0 = previous, slot 1 = next.
#[tauri::command]
fn set_chan_binding(slot: usize, code: Option<String>) {
    if slot >= 2 {
        return;
    }
    let code = binding_code(code);
    CHAN_SLOT_DOWN[slot].store(false, Ordering::SeqCst);
    chan_bindings().lock().unwrap()[slot] = code;
    refresh_chan_bound();
}

/// Arm capture: the next global key/button press binds channel-cycle `slot`.
#[tauri::command]
fn start_chan_capture(slot: usize) {
    if slot < 2 {
        CHAN_CAPTURE_SLOT.store(slot as i32, Ordering::SeqCst);
    }
}

fn make_sink(app: &AppHandle) -> Sink {
    let app = app.clone();
    Arc::new(move |ev: UiEvent| {
        let _ = app.emit("ui", ev);
    })
}

const MAX_CHAT_LEN: usize = 2000;

/// Identifier guard: non-empty, bounded, safe charset.
fn valid_id(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}
/// Like `valid_id` but also allows '.' — a deep-link `user_id` may be a Discord
/// name, which can contain a dot.
fn valid_user_id(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}
/// Display name: 1–64 chars, no control chars (the rule `connect` applies).
fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.chars().count() <= 64 && !s.chars().any(|c| c.is_control())
}
/// Upper bound for a serverless SDP code (base64 of an SDP is a few KiB).
pub(crate) const MAX_CODE_LEN: usize = 32 * 1024;
/// Serverless code guard: non-empty, bounded, base64 alphabet plus whitespace
/// (chat apps wrap long pastes; the engine strips it).
fn valid_code(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_CODE_LEN
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=') || c.is_whitespace())
}
fn valid_hex(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.chars().all(|c| c.is_ascii_hexdigit())
}
/// Channel (frequency) name rule shared by `set_channel` / `remove_channel`:
/// 1–MAX_CHANNEL_LEN chars after trimming, no control chars.
fn valid_channel(name: &str) -> bool {
    !name.is_empty() && name.chars().count() <= companion_core::MAX_CHANNEL_LEN && !name.chars().any(|c| c.is_control())
}
/// Room token from the webview: trimmed; empty → none; otherwise ≤128 hex chars.
fn clean_token(token: Option<String>) -> Result<Option<String>, String> {
    match token.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
        Some(t) if valid_hex(&t, 128) => Ok(Some(t)),
        Some(_) => Err("invalid token".into()),
        None => Ok(None),
    }
}
/// Pinned server cert: trimmed; empty → none; otherwise exactly 64 hex chars (SHA-256).
fn clean_cert(cert: Option<String>) -> Result<Option<String>, String> {
    match cert.map(|c| c.trim().to_string()).filter(|c| !c.is_empty()) {
        Some(c) if c.len() == 64 && valid_hex(&c, 64) => Ok(Some(c)),
        Some(_) => Err("invalid cert_sha256 (64 hex chars)".into()),
        None => Ok(None),
    }
}
/// Audio device name at connect time: trimmed; empty → default device;
/// anything over 256 bytes is refused (cpal names are far shorter).
fn clean_device(d: Option<String>) -> Result<Option<String>, String> {
    match d.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        Some(s) if s.len() <= 256 => Ok(Some(s)),
        Some(_) => Err("invalid device name".into()),
        None => Ok(None),
    }
}
/// Live device switch: same trim/empty rule, but an overlong name simply falls
/// back to the default device instead of erroring (nothing to report back to).
fn live_device(name: Option<String>) -> Option<String> {
    name.map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && s.len() <= 256)
}
/// Chat text as sent on the wire: capped at MAX_CHAT_LEN chars; whitespace-only → nothing.
fn bounded_chat(text: &str) -> Option<String> {
    let text: String = text.chars().take(MAX_CHAT_LEN).collect();
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}
/// A finite value clamped to [lo, hi]; NaN/±inf fall back to `dflt`.
fn norm_f32(v: f32, lo: f32, hi: f32, dflt: f32) -> f32 {
    if v.is_finite() {
        v.clamp(lo, hi)
    } else {
        dflt
    }
}
/// Normalize a DSP config at the IPC boundary: reject NaN/inf, clamp to sane ranges.
fn normalize_dsp(mut cfg: companion_core::audio::DspConfig) -> companion_core::audio::DspConfig {
    cfg.gate_threshold = norm_f32(cfg.gate_threshold, 0.0, 0.5, 0.015);
    cfg.comp_threshold = norm_f32(cfg.comp_threshold, 0.01, 1.0, 0.15);
    cfg.comp_ratio = norm_f32(cfg.comp_ratio, 1.0, 20.0, 3.0);
    cfg.comp_makeup = norm_f32(cfg.comp_makeup, 0.1, 4.0, 1.4);
    cfg.limiter_ceiling = norm_f32(cfg.limiter_ceiling, 0.05, 1.0, 0.97);
    cfg
}
/// Normalize the radio-effect config like `normalize_dsp`. The clamp ranges
/// alone keep a real passband (low cut ≤ 1 kHz < 1.2 kHz ≤ high cut), so the
/// cascade can never collapse into a near-mute band.
fn normalize_radio(mut cfg: companion_core::audio::RadioCfg) -> companion_core::audio::RadioCfg {
    cfg.low_cut = norm_f32(cfg.low_cut, 50.0, 1000.0, 300.0);
    cfg.high_cut = norm_f32(cfg.high_cut, 1200.0, 12000.0, 3400.0);
    cfg.saturation = norm_f32(cfg.saturation, 0.0, 1.0, 0.35);
    cfg.destruction = norm_f32(cfg.destruction, 0.0, 1.0, 0.15);
    cfg
}
/// Upper bound for the webview's control-state snapshot (a status blob, not a
/// channel for bulk data).
const MAX_CONTROL_STATE: usize = 16 * 1024;
/// True when a snapshot is small enough to cache and fan out to controllers.
fn control_state_ok(snapshot: &serde_json::Value) -> bool {
    serde_json::to_string(snapshot).map(|s| s.len() <= MAX_CONTROL_STATE).unwrap_or(false)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)] // one IPC call = one connect form; splitting it would only move the fields into a struct
async fn connect(
    app: AppHandle,
    state: State<'_, AppState>,
    server: String,
    room: String,
    user_id: String,
    name: String,
    token: Option<String>,
    cert_sha256: Option<String>,
    input_device: Option<String>,
    output_device: Option<String>,
    relay_enabled: Option<bool>,
) -> Result<(), String> {
    // ── Rust-side validation (never trust the webview) ──────────────────────
    let relay_enabled = relay_enabled.unwrap_or(false);
    let server = server.trim().to_string();
    if !companion_core::signaling::server_url_ok(&server) {
        return Err("invalid server URL — use wss:// or loopback ws://".into());
    }
    if !valid_id(&room, 64) {
        return Err("invalid room".into());
    }
    if !valid_user_id(&user_id, 64) {
        return Err("invalid user_id".into());
    }
    let name = name.trim().to_string();
    if !valid_name(&name) {
        return Err("invalid name (1–64 chars, no control chars)".into());
    }
    let token = clean_token(token)?;
    let cert_sha256 = clean_cert(cert_sha256)?;
    let input_device = clean_device(input_device)?;
    let output_device = clean_device(output_device)?;

    // One engine at a time. A deep link arriving mid-session would otherwise
    // stack a second engine, and the first one's teardown Status knocks the UI
    // off the session that is still running.
    if state.engine.lock().unwrap().is_some() {
        return Err("already connected — disconnect first".into());
    }
    // Transmit/chat consult the serverless slot first; a stale one would eat them.
    *state.serverless.lock().unwrap() = None;

    let engine = start(
        EngineConfig { server, room, user_id, name, token, cert_sha256, input_device, output_device, relay_enabled },
        make_sink(&app),
    )
    .await
    .map_err(|e| e.to_string())?;
    *state.engine.lock().unwrap() = Some(engine);
    Ok(())
}

// ── Serverless 1:1 (copy-paste SDP) ───────────────────────────────────────
#[tauri::command]
async fn serverless_offer(app: AppHandle, state: State<'_, AppState>, name: String) -> Result<String, String> {
    let name = name.trim().to_string();
    if !valid_name(&name) {
        return Err("invalid name (1–64 chars, no control chars)".into());
    }
    let (s, code) = Serverless::create_offer(make_sink(&app), name).await.map_err(|e| e.to_string())?;
    *state.serverless.lock().unwrap() = Some(Arc::new(s));
    Ok(code)
}

#[tauri::command]
async fn serverless_accept_offer(app: AppHandle, state: State<'_, AppState>, name: String, code: String) -> Result<String, String> {
    let name = name.trim().to_string();
    if !valid_name(&name) {
        return Err("invalid name (1–64 chars, no control chars)".into());
    }
    if !valid_code(&code) {
        return Err("invalid code".into());
    }
    let (s, answer) = Serverless::accept_offer(code, make_sink(&app), name).await.map_err(|e| e.to_string())?;
    *state.serverless.lock().unwrap() = Some(Arc::new(s));
    Ok(answer)
}

#[tauri::command]
async fn serverless_accept_answer(state: State<'_, AppState>, code: String) -> Result<(), String> {
    if !valid_code(&code) {
        return Err("invalid code".into());
    }
    let s = state.serverless.lock().unwrap().clone();
    match s {
        Some(s) => s.accept_answer(code).await.map_err(|e| e.to_string()),
        None => Err("keine offene Serverless-Sitzung".into()),
    }
}

#[tauri::command]
fn toggle_transmit(state: State<AppState>) {
    if let Some(s) = state.serverless.lock().unwrap().as_ref() {
        s.toggle_transmit();
        return;
    }
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.toggle_transmit();
    }
}

#[tauri::command]
fn set_transmit(state: State<AppState>, on: bool) {
    if let Some(s) = state.serverless.lock().unwrap().as_ref() {
        s.set_transmit(on);
        return;
    }
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.set_transmit(on);
    }
}

#[tauri::command]
fn send_chat(state: State<AppState>, text: String) {
    // Bound the message length (defense against oversized webview input).
    let Some(text) = bounded_chat(&text) else { return };
    if let Some(s) = state.serverless.lock().unwrap().as_ref() {
        s.send_chat(text);
        return;
    }
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.send_chat(text);
    }
}

/// Clamp incoming volume to a sane gain range (also clamped in core).
fn clamp_vol(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 2.0)
    } else {
        1.0
    }
}

#[tauri::command]
fn set_master_volume(state: State<AppState>, volume: f32) {
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.set_master_volume(clamp_vol(volume));
    }
}

#[tauri::command]
fn set_peer_volume(state: State<AppState>, user_id: String, volume: f32) -> Result<(), String> {
    if !valid_user_id(&user_id, 64) {
        return Err("invalid user_id".into());
    }
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.set_peer_volume(&user_id, clamp_vol(volume));
    }
    Ok(())
}

#[tauri::command]
fn list_audio_devices() -> (Vec<String>, Vec<String>) {
    companion_core::audio::list_devices()
}

#[tauri::command]
fn rotate_key(state: State<AppState>) {
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.rotate_key();
    }
}

#[tauri::command]
fn reconnect_session(state: State<AppState>) {
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.reconnect();
    }
}

/// Switch to a named channel (frequency). Validated at the IPC boundary; the
/// engine announces it to all peers and re-gates incoming audio.
#[tauri::command]
fn set_channel(state: State<AppState>, name: String) -> Result<(), String> {
    let name = name.trim().to_string();
    if !valid_channel(&name) {
        return Err("invalid channel (1–32 chars, no control chars)".into());
    }
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.set_channel(name);
    }
    Ok(())
}

/// Delete an (empty) channel from the shared directory. The engine drops it,
/// tombstones it, and tells every peer to do the same.
#[tauri::command]
fn remove_channel(state: State<AppState>, name: String) -> Result<(), String> {
    let name = name.trim().to_string();
    if !valid_channel(&name) {
        return Err("invalid channel (1–32 chars, no control chars)".into());
    }
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.remove_channel(name);
    }
    Ok(())
}

#[tauri::command]
fn set_dsp(state: State<AppState>, cfg: companion_core::audio::DspConfig) {
    let cfg = normalize_dsp(cfg);
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.set_dsp(cfg);
    }
}

/// Receive-bus radio effect ("Funk-Effekt"): band-limit + saturation +
/// degradation on incoming voice. Normalized at the IPC boundary like set_dsp.
#[tauri::command]
fn set_radio_fx(state: State<AppState>, cfg: companion_core::audio::RadioCfg) {
    let cfg = normalize_radio(cfg);
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.set_radio(cfg);
    }
}

/// The webview reports its control-relevant state (connected, transmitting,
/// mute, deafen, channel, channel list, volume); fanned out to every
/// authenticated controller. Size-capped: this is a status blob, not a channel
/// for bulk data.
#[tauri::command]
fn control_state(state: State<AppState>, snapshot: serde_json::Value) {
    if !control_state_ok(&snapshot) {
        return;
    }
    if let Some(srv) = state.control.lock().unwrap().as_ref() {
        srv.publish_state(snapshot);
    }
}

#[tauri::command]
fn set_monitor(state: State<AppState>, on: bool) {
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.set_monitor(on);
    }
}

/// Toggle the local "Funk-Klick" earcon played at the start of incoming transmissions.
#[tauri::command]
fn set_earcon(state: State<AppState>, on: bool) {
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.set_earcon(on);
    }
}

/// Volume of the local "Funk-Klick" earcon (0.0 mute … 1.0 normal … 2.0 +6 dB).
#[tauri::command]
fn set_earcon_volume(state: State<AppState>, volume: f32) {
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.set_earcon_volume(clamp_vol(volume));
    }
}

/// Local confirmation tone for latched ("hands-free") push-to-talk.
#[tauri::command]
fn ptt_latch_cue(state: State<AppState>, on: bool) {
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.ptt_latch_cue(on);
    }
}

#[tauri::command]
fn set_low_bandwidth(state: State<AppState>, on: bool) {
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.set_low_bandwidth(on);
    }
}

/// Switch capture/playback device LIVE while connected (empty string = default).
/// When not connected it's a no-op — the saved choice is applied on next connect.
#[tauri::command]
fn set_input_device(state: State<AppState>, name: Option<String>) {
    let name = live_device(name);
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.set_input_device(name);
    }
}

#[tauri::command]
fn set_output_device(state: State<AppState>, name: Option<String>) {
    let name = live_device(name);
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.set_output_device(name);
    }
}

/// Leave the session: drop the engine (stops mesh + audio threads) → the engine
/// task emits Status{connected:false}, returning the UI to the start screen.
#[tauri::command]
fn disconnect(state: State<AppState>) {
    *state.engine.lock().unwrap() = None;
    *state.serverless.lock().unwrap() = None;
}

/// Network self-check (pre-session): can we send/receive WebRTC data, is STUN
/// reachable, is the signaling server reachable.
#[tauri::command]
async fn net_selfcheck(server: String) -> Result<companion_core::selfcheck::SelfCheck, String> {
    let server = server.trim().to_string();
    if !companion_core::signaling::server_url_ok(&server) {
        return Err("invalid server URL".into());
    }
    Ok(companion_core::selfcheck::run(&server, None).await)
}

fn pending_deeplink() -> &'static Mutex<Option<String>> {
    PENDING_DEEPLINK.get_or_init(|| Mutex::new(None))
}

/// Deep-link schemes we answer to. `subraum://` is current; `squadlink://` is the
/// pre-rename scheme, kept so links already handed out (Fleetplanner entries,
/// pinned messages) keep working. Both carry the identical query format.
const DEEPLINK_SCHEMES: [&str; 2] = ["subraum://", "squadlink://"];

/// True for an argv entry that is one of our deep links.
fn is_deeplink(arg: &str) -> bool {
    DEEPLINK_SCHEMES.iter().any(|s| arg.starts_with(s))
}

/// True when running inside an MSIX package (Microsoft Store build). Used to skip
/// registry-based deep-link registration (the package manifest declares the
/// `subraum` + `squadlink` protocols) and to hide the self-update UI (the Store
/// updates the app).
#[cfg(windows)]
fn is_packaged() -> bool {
    use windows_sys::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
    const APPMODEL_ERROR_NO_PACKAGE: u32 = 15700;
    let mut len: u32 = 0;
    let rc = unsafe { GetCurrentPackageFullName(&mut len, std::ptr::null_mut()) };
    rc != APPMODEL_ERROR_NO_PACKAGE
}
#[cfg(not(windows))]
fn is_packaged() -> bool {
    false
}

/// Webview reads this on mount to suppress the self-update prompt in Store builds.
#[tauri::command]
fn is_store_build() -> bool {
    is_packaged()
}

/// Drained once by the webview on mount: the subraum:// URL (if any) the app
/// was cold-started with. Returns None on a normal launch.
#[tauri::command]
fn take_pending_deeplink() -> Option<String> {
    pending_deeplink().lock().unwrap().take()
}

/// Open the public download page in the system browser (for the update prompt).
/// Uses ShellExecuteW rather than shelling out to cmd.exe — the latter trips the
/// Store's "blocked executable" check (a cmd.exe reference in the binary).
#[tauri::command]
fn open_download() {
    open_url("https://subraum.cc/download/");
}

#[cfg(windows)]
fn open_url(url: &str) {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let op: Vec<u16> = "open\0".encode_utf16().collect();
    let file: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );
    }
}

#[cfg(target_os = "macos")]
fn open_url(url: &str) {
    let _ = std::process::Command::new("open").arg(url).spawn();
}

// Linux (and, for lack of a launcher, Android: the spawn fails silently there).
#[cfg(all(not(windows), not(target_os = "macos")))]
fn open_url(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// Turn off WebView2's browser accelerator keys for a window.
///
/// WebView2 brings Edge's key bindings along, and they surface as bugs in an
/// app that is not a browser: F3 opens the find bar over the session UI, F7
/// pops the caret-browsing prompt, F5/Ctrl+R reload the shell out from under a
/// live call. Both F3 and F7 were reported by users. They are handled by the
/// host before the DOM sees them, so `preventDefault()` in the webview cannot
/// suppress them — the one switch that does is on `ICoreWebView2Settings3`.
/// It covers only browser-specific accelerators; text editing (Ctrl+C/V/X/A)
/// and our own hotkeys are unaffected.
///
/// webkitgtk and WKWebView have no such bindings, hence Windows-only with a
/// no-op fallback.
#[cfg(windows)]
fn disable_browser_accelerators(win: &tauri::WebviewWindow) {
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings3;
    use windows_core::Interface;

    let _ = win.with_webview(|webview| unsafe {
        let controller = webview.controller();
        let Ok(core) = controller.CoreWebView2() else { return };
        let Ok(settings) = core.Settings() else { return };
        // Settings3 is WebView2 runtime 1.0.774.44+ (2020). Older runtimes just
        // keep the accelerators rather than losing the window.
        if let Ok(s3) = settings.cast::<ICoreWebView2Settings3>() {
            let _ = s3.SetAreBrowserAcceleratorKeysEnabled(false);
        }
    });
}

#[cfg(not(windows))]
fn disable_browser_accelerators(_win: &tauri::WebviewWindow) {}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // WebKitGTK 2.42+ (shipped by rolling distros like Arch) defaults to a DMABUF
    // renderer that fails under many GPU/driver/Xwayland combos, leaving the Tauri
    // window blank/white or crashing on launch. Disabling it forces the stable GL
    // path. Set before any webview is created; only if the user hasn't overridden it.
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
        if std::env::var_os("WEBKIT_DISABLE_COMPOSITING_MODE").is_none() {
            std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
        }
    }

    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default();
    // Single-instance is desktop-only (the plugin has no mobile target) and MUST
    // be the first plugin: a second launch (e.g. clicking a subraum:// link while
    // running) forwards its argv here; we surface the deep link to the
    // already-running window instead of opening a new one.
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            if let Some(url) = argv.iter().find(|a| is_deeplink(a)) {
                let _ = app.emit("deeplink", url.clone());
            }
        }));
    }
    builder
        .plugin(tauri_plugin_deep_link::init())
        // Closing the MAIN window must end the app, full stop. The in-game
        // overlay is a second window; without this, Tauri's "exit when the last
        // window closes" default keeps the process (overlay, engine, audio,
        // control server) alive after the user hit X. Overlay-close stays
        // harmless: it only removes that one window.
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { .. } = event {
                    window.app_handle().exit(0);
                }
            }
        })
        .manage(AppState { engine: Mutex::new(None), serverless: Mutex::new(None), control: Mutex::new(None) })
        .setup(|app| {
            let _ = APP_HANDLE.set(app.handle().clone());
            start_raw_input();
            // Kill WebView2's own key bindings on every window. The overlay is
            // created later from the webview, so also catch window-created —
            // otherwise F3 keeps working in the second window.
            for (_, win) in app.webview_windows() {
                disable_browser_accelerators(&win);
            }
            {
                use tauri::Listener;
                let handle = app.handle().clone();
                app.listen("tauri://window-created", move |_| {
                    for (_, win) in handle.webview_windows() {
                        disable_browser_accelerators(&win);
                    }
                });
            }
            // Local control API (Stream Deck / Companion): loopback WS + token.
            // Commands land here and are re-emitted as the SAME events the
            // Raw-Input hook and hotkeys use, so all gating (self-mute blocks
            // PTT, channel validation) stays in one place — the webview.
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel();
                    match control::ControlServer::start(out_tx).await {
                        Ok(srv) => {
                            if let Ok(dir) = handle.path().app_config_dir() {
                                if let Err(e) = control::write_discovery(&dir, srv.port, &srv.token) {
                                    eprintln!("control: discovery file failed: {e}");
                                }
                            }
                            if let Some(state) = handle.try_state::<AppState>() {
                                *state.control.lock().unwrap() = Some(srv);
                            }
                            while let Some(cmd) = out_rx.recv().await {
                                match cmd {
                                    control::CtlOut::Ptt(on) => publish_ptt(None, Some(on)),
                                    control::CtlOut::ChanCycle(d) => { let _ = handle.emit("chan-cycle", d); }
                                    control::CtlOut::Ctl(v) => { let _ = handle.emit("ctl", v); }
                                    control::CtlOut::Refresh => { let _ = handle.emit("ctl-refresh", ()); }
                                }
                            }
                        }
                        Err(e) => eprintln!("control: server failed to start: {e}"),
                    }
                });
            }
            // Cold start: stash the launch URL for the webview to drain on mount
            // (Windows delivers deep links via argv).
            if let Some(url) = std::env::args().find(|a| is_deeplink(a)) {
                *pending_deeplink().lock().unwrap() = Some(url);
            }
            // Register both schemes at runtime (dev + portable runs; the NSIS/MSI
            // installer also registers them). In an MSIX/Store build the package
            // manifest declares the protocols, so skip the registry write.
            // on_open_url covers warm relaunch (macOS); a running instance is reached
            // via single-instance above.
            #[cfg(any(windows, target_os = "linux"))]
            if !is_packaged() {
                use tauri_plugin_deep_link::DeepLinkExt;
                let _ = app.deep_link().register_all();
            }
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let handle = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    if let Some(url) = event.urls().first() {
                        let _ = handle.emit("deeplink", url.to_string());
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            connect,
            serverless_offer,
            serverless_accept_offer,
            serverless_accept_answer,
            toggle_transmit,
            set_transmit,
            send_chat,
            set_master_volume,
            set_peer_volume,
            list_audio_devices,
            rotate_key,
            reconnect_session,
            set_channel,
            remove_channel,
            set_dsp,
            set_radio_fx,
            control_state,
            set_monitor,
            set_low_bandwidth,
            set_input_device,
            set_output_device,
            disconnect,
            net_selfcheck,
            open_download,
            take_pending_deeplink,
            is_store_build,
            set_ptt_binding,
            start_ptt_capture,
            set_chan_binding,
            start_chan_capture,
            set_ducking,
            set_duck_amount,
            set_earcon,
            set_earcon_volume,
            ptt_latch_cue
        ])
        .run(tauri::generate_context!())
        .expect("error running tauri app");
}

/// Input guards at the IPC boundary. The webview is the only caller today, but a
/// Tauri command is reachable by anything running in it, so these are the
/// last check before untrusted strings reach the engine and the signaling wire.
#[cfg(test)]
mod ipc_validation_tests {
    use super::*;

    #[test]
    fn valid_name_bounds_and_control_chars() {
        assert!(valid_name("Commander"));
        assert!(!valid_name(""));
        assert!(!valid_name(&"x".repeat(65)));
        assert!(!valid_name("a\u{7}b"));
    }

    #[test]
    fn valid_code_accepts_base64_with_whitespace_and_rejects_junk() {
        assert!(valid_code("QUJD\nREVG=="));
        assert!(!valid_code(""));
        assert!(!valid_code("<script>"));
        assert!(!valid_code(&"A".repeat(MAX_CODE_LEN + 1)));
    }

    #[test]
    fn control_ptt_release_does_not_cut_a_held_key() {
        let mut s = PttSources::default();
        assert_eq!(s.update(Some(true), None), Some(true));
        assert_eq!(s.update(None, Some(true)), None); // already transmitting
        assert_eq!(s.update(None, Some(false)), None); // key still held
        assert_eq!(s.update(Some(false), None), Some(false));
    }

    #[test]
    fn control_ptt_alone_still_works() {
        let mut s = PttSources::default();
        assert_eq!(s.update(None, Some(true)), Some(true));
        assert_eq!(s.update(None, Some(false)), Some(false));
    }

    #[test]
    fn valid_id_accepts_the_ids_we_generate() {
        assert!(valid_id("squad-1a2b3c4d", 64));
        assert!(valid_id("a1b2c3d4", 64)); // crypto.randomUUID().slice(0,8)
        assert!(valid_id("Room_9", 64));
    }

    #[test]
    fn valid_id_rejects_empty_overlong_and_unsafe_charsets() {
        assert!(!valid_id("", 64), "empty");
        assert!(!valid_id(&"a".repeat(65), 64), "over the cap");
        assert!(valid_id(&"a".repeat(64), 64), "exactly at the cap is fine");

        // Anything that could change the meaning of a URL, a JSON document or a
        // rendered page downstream.
        for bad in ["room/../etc", "room?a=b", "room#frag", "a b", "a\nb", "<script>", "ro'om", "a\"b", "café", "room."] {
            assert!(!valid_id(bad, 64), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn valid_user_id_additionally_allows_a_dot() {
        // Deep links carry a Discord name as `uid`, which may contain a dot.
        assert!(valid_user_id("some.player", 64));
        assert!(valid_user_id("plain", 64));
        assert!(!valid_id("some.player", 64), "the stricter guard still says no");
        // but nothing beyond that
        assert!(!valid_user_id("", 64));
        assert!(!valid_user_id("a/b", 64));
        assert!(!valid_user_id(&"a".repeat(65), 64));
    }

    #[test]
    fn valid_hex_accepts_only_hex() {
        assert!(valid_hex("deadBEEF01", 64));
        assert!(!valid_hex("", 64));
        assert!(!valid_hex("xyz", 64));
        assert!(!valid_hex("dead beef", 64));
        assert!(!valid_hex(&"a".repeat(65), 64));
    }

    /// Volumes arrive as f32 straight from JS, where NaN and Infinity are one
    /// arithmetic slip away. An unclamped value reaches the mixer gain.
    #[test]
    fn clamp_vol_bounds_every_float_js_can_send() {
        assert_eq!(clamp_vol(1.0), 1.0);
        assert_eq!(clamp_vol(0.0), 0.0);
        assert_eq!(clamp_vol(2.0), 2.0);
        assert_eq!(clamp_vol(-5.0), 0.0);
        assert_eq!(clamp_vol(99.0), 2.0);
        assert_eq!(clamp_vol(f32::NAN), 1.0, "NaN falls back to unity gain");
        assert_eq!(clamp_vol(f32::INFINITY), 1.0);
        assert_eq!(clamp_vol(f32::NEG_INFINITY), 1.0);
    }
}

/// Modifier-chord helpers: platform-neutral pure functions that the Windows
/// Raw Input thread feeds. A wrong parse here means a binding like
/// "Ctrl+Shift+KeyT" either never fires or fires on the bare key.
#[cfg(test)]
mod chord_tests {
    use super::*;

    const ALL: Mods = Mods { shift: true, ctrl: true, alt: true, meta: true };

    #[test]
    fn mod_of_label_knows_both_sides_of_every_modifier() {
        assert_eq!(mod_of_label("ShiftLeft"), Some("Shift"));
        assert_eq!(mod_of_label("ShiftRight"), Some("Shift"));
        assert_eq!(mod_of_label("ControlLeft"), Some("Ctrl"));
        assert_eq!(mod_of_label("ControlRight"), Some("Ctrl"));
        assert_eq!(mod_of_label("Alt"), Some("Alt"));
        assert_eq!(mod_of_label("AltGr"), Some("Alt"));
        assert_eq!(mod_of_label("MetaLeft"), Some("Meta"));
        assert_eq!(mod_of_label("MetaRight"), Some("Meta"));
        for plain in ["F8", "KeyT", "Mouse:Left", "Pad:3", "Shift", "Ctrl", ""] {
            assert_eq!(mod_of_label(plain), None, "{plain:?} is not a modifier label");
        }
    }

    #[test]
    fn mods_ok_requires_every_required_modifier_and_tolerates_extras() {
        let none = Mods::default();
        let shift = Mods { shift: true, ..none };
        let ctrl_shift = Mods { ctrl: true, shift: true, ..none };
        // No requirement: always satisfied, whatever is held.
        assert!(mods_ok(none, none));
        assert!(mods_ok(none, ALL));
        // Exact match and extras.
        assert!(mods_ok(shift, shift));
        assert!(mods_ok(shift, ctrl_shift), "an extra held Ctrl must not block a Shift chord");
        assert!(mods_ok(ctrl_shift, ALL));
        // A missing required modifier fails, even with others held.
        assert!(!mods_ok(shift, none));
        assert!(!mods_ok(ctrl_shift, shift));
        assert!(!mods_ok(ALL, ctrl_shift));
    }

    #[test]
    fn parse_binding_splits_modifiers_from_the_base_key() {
        assert_eq!(parse_binding("F8"), (Mods::default(), "F8".to_string()));
        assert_eq!(parse_binding("Mouse:Left"), (Mods::default(), "Mouse:Left".to_string()));
        assert_eq!(parse_binding("Shift+KeyT"), (Mods { shift: true, ..Mods::default() }, "KeyT".to_string()));
        assert_eq!(
            parse_binding("Ctrl+Shift+Mouse:Left"),
            (Mods { ctrl: true, shift: true, ..Mods::default() }, "Mouse:Left".to_string())
        );
        // Order of the modifier tokens does not matter.
        assert_eq!(parse_binding("Meta+Alt+F1"), (Mods { alt: true, meta: true, ..Mods::default() }, "F1".to_string()));
        assert_eq!(parse_binding("Alt+Meta+F1"), parse_binding("Meta+Alt+F1"));
        // Every modifier at once.
        assert_eq!(parse_binding("Ctrl+Alt+Shift+Meta+KeyZ"), (ALL, "KeyZ".to_string()));
    }

    #[test]
    fn parse_binding_stops_at_the_first_non_modifier_token() {
        // "KeyA+Shift" is not a chord: the base key comes first, so the rest is the base.
        assert_eq!(parse_binding("KeyA+Shift"), (Mods::default(), "KeyA+Shift".to_string()));
        // A gamepad/mouse label containing '+' in a strange place is kept verbatim.
        assert_eq!(parse_binding("Pad:1+2"), (Mods::default(), "Pad:1+2".to_string()));
        // Unknown modifier spelling is not a modifier.
        assert_eq!(parse_binding("Control+KeyA"), (Mods::default(), "Control+KeyA".to_string()));
    }

    #[test]
    fn parse_binding_edge_cases_never_panic() {
        assert_eq!(parse_binding(""), (Mods::default(), String::new()));
        // Trailing '+' with no base key: modifiers parsed, empty base (never
        // matches a real key edge, so such a binding is inert).
        assert_eq!(parse_binding("Shift+"), (Mods { shift: true, ..Mods::default() }, String::new()));
        assert_eq!(parse_binding("+"), (Mods::default(), "+".to_string()));
        // A duplicated modifier is idempotent.
        assert_eq!(parse_binding("Shift+Shift+F2"), (Mods { shift: true, ..Mods::default() }, "F2".to_string()));
    }

    #[test]
    fn build_combo_uses_the_canonical_ctrl_alt_shift_meta_order() {
        assert_eq!(build_combo("F8", Mods::default()), "F8");
        assert_eq!(build_combo("KeyT", Mods { shift: true, ..Mods::default() }), "Shift+KeyT");
        assert_eq!(build_combo("KeyT", ALL), "Ctrl+Alt+Shift+Meta+KeyT");
        assert_eq!(
            build_combo("Mouse:Left", Mods { meta: true, ctrl: true, ..Mods::default() }),
            "Ctrl+Meta+Mouse:Left"
        );
    }

    #[test]
    fn build_combo_and_parse_binding_round_trip_for_every_modifier_set() {
        for bits in 0u8..16 {
            let m = Mods { ctrl: bits & 1 != 0, alt: bits & 2 != 0, shift: bits & 4 != 0, meta: bits & 8 != 0 };
            for base in ["F8", "KeyT", "Mouse:Left", "Mouse:WheelUp", "Pad:12"] {
                let combo = build_combo(base, m);
                assert_eq!(parse_binding(&combo), (m, base.to_string()), "round trip of {combo:?}");
            }
        }
    }

    #[test]
    fn a_saved_chord_label_reparses_to_the_same_chord() {
        // What `handle_raw` stores on capture is `build_combo(...)`; what it
        // later compares against is `parse_binding(...)` of that string. The
        // two must agree or a captured chord can never fire.
        let held = Mods { ctrl: true, shift: true, ..Mods::default() };
        let stored = build_combo("KeyR", held);
        let (req, base) = parse_binding(&stored);
        assert_eq!(base, "KeyR");
        assert!(mods_ok(req, held));
        assert!(!mods_ok(req, Mods { shift: true, ..Mods::default() }), "releasing Ctrl must stop the chord");
    }
}

/// Deep-link recognition on argv / single-instance forwarding.
#[cfg(test)]
mod deeplink_tests {
    use super::*;

    #[test]
    fn both_schemes_are_recognized_current_one_first() {
        assert_eq!(DEEPLINK_SCHEMES, ["subraum://", "squadlink://"]);
        assert!(is_deeplink("subraum://join?code=abc&pin=123456"));
        assert!(is_deeplink("squadlink://join?code=abc&pin=123456"));
        assert!(is_deeplink("subraum://"), "a bare scheme still counts as ours (webview rejects the payload)");
    }

    #[test]
    fn anything_else_on_argv_is_not_a_deep_link() {
        for arg in [
            "",
            "subraum",
            "subraum:",
            "subraum:/join",
            "https://subraum.cc/j/abc",
            "wss://subraum.cc/ws",
            "--flag",
            "C:\\Program Files\\subraum\\subraum.exe",
            " subraum://join", // leading space: not what a launcher hands us
            "xsubraum://join",
        ] {
            assert!(!is_deeplink(arg), "{arg:?} must not be treated as a deep link");
        }
    }

    #[test]
    fn scheme_match_is_case_sensitive() {
        // Launchers hand the scheme over as registered (lowercase). An
        // uppercase spelling is refused rather than normalized, consistent
        // with the WSS:// pinning fix on the engine side.
        assert!(!is_deeplink("SUBRAUM://join?code=abc"));
        assert!(!is_deeplink("Subraum://join?code=abc"));
        assert!(!is_deeplink("SQUADLINK://join?code=abc"));
    }

    #[test]
    fn pending_deeplink_is_drained_exactly_once() {
        // Only this test touches PENDING_DEEPLINK.
        assert_eq!(take_pending_deeplink(), None, "normal launch: nothing pending");
        *pending_deeplink().lock().unwrap() = Some("subraum://join?code=x".into());
        assert_eq!(take_pending_deeplink().as_deref(), Some("subraum://join?code=x"));
        assert_eq!(take_pending_deeplink(), None, "a second drain finds nothing");
    }
}

/// Global binding state behind the `set_*_binding` / `start_*_capture`
/// commands. Each test owns one set of statics (PTT vs channel) so the
/// parallel test runner cannot interleave them.
#[cfg(test)]
mod binding_state_tests {
    use super::*;

    #[test]
    fn binding_code_keeps_only_non_empty_bounded_codes() {
        assert_eq!(binding_code(None), None);
        assert_eq!(binding_code(Some(String::new())), None);
        assert_eq!(binding_code(Some("F8".into())).as_deref(), Some("F8"));
        assert_eq!(binding_code(Some("Ctrl+Shift+Mouse:Left".into())).as_deref(), Some("Ctrl+Shift+Mouse:Left"));
        assert_eq!(binding_code(Some("x".repeat(64))).map(|s| s.len()), Some(64), "64 bytes is still fine");
        assert_eq!(binding_code(Some("x".repeat(65))), None, "65 bytes is unbound, not truncated");
    }

    #[test]
    fn ptt_bindings_default_rebind_clear_and_capture_arming() {
        // Only this test touches PTT_BINDINGS / PTT_SLOT_DOWN / PTT_CAPTURE_SLOT.
        assert_eq!(*ptt_bindings().lock().unwrap(), [Some("F8".to_string()), None], "slot 0 defaults to F8");

        set_ptt_binding(1, Some("Mouse:Left".into()));
        assert_eq!(ptt_bindings().lock().unwrap()[1].as_deref(), Some("Mouse:Left"));

        // A stuck "held" flag is cleared when the slot is rebound.
        PTT_SLOT_DOWN[0].store(true, Ordering::SeqCst);
        set_ptt_binding(0, Some("KeyR".into()));
        assert!(!PTT_SLOT_DOWN[0].load(Ordering::SeqCst));
        assert_eq!(ptt_bindings().lock().unwrap()[0].as_deref(), Some("KeyR"));

        // Empty / overlong / None all unbind.
        set_ptt_binding(0, Some(String::new()));
        assert_eq!(ptt_bindings().lock().unwrap()[0], None);
        set_ptt_binding(1, Some("y".repeat(65)));
        assert_eq!(ptt_bindings().lock().unwrap()[1], None);
        set_ptt_binding(1, Some("F9".into()));
        set_ptt_binding(1, None);
        assert_eq!(ptt_bindings().lock().unwrap()[1], None);

        // Out-of-range slot: ignored, no panic, nothing changes.
        set_ptt_binding(2, Some("F10".into()));
        set_ptt_binding(usize::MAX, Some("F10".into()));
        assert_eq!(*ptt_bindings().lock().unwrap(), [None, None]);

        // Capture arming: valid slots arm, invalid ones leave the state alone.
        assert_eq!(PTT_CAPTURE_SLOT.load(Ordering::SeqCst), -1, "idle by default");
        start_ptt_capture(1);
        assert_eq!(PTT_CAPTURE_SLOT.load(Ordering::SeqCst), 1);
        start_ptt_capture(2);
        start_ptt_capture(usize::MAX);
        assert_eq!(PTT_CAPTURE_SLOT.load(Ordering::SeqCst), 1, "bad slot does not disturb an armed capture");
        start_ptt_capture(0);
        assert_eq!(PTT_CAPTURE_SLOT.load(Ordering::SeqCst), 0);
        PTT_CAPTURE_SLOT.store(-1, Ordering::SeqCst);
    }

    #[test]
    fn channel_bindings_track_the_any_bound_fast_path_flag() {
        // Only this test touches CHAN_BINDINGS / CHAN_SLOT_DOWN / CHAN_ANY_BOUND / CHAN_CAPTURE_SLOT.
        assert_eq!(*chan_bindings().lock().unwrap(), [None, None], "channel keys are unbound by default");
        refresh_chan_bound();
        assert!(!CHAN_ANY_BOUND.load(Ordering::Relaxed));

        set_chan_binding(1, Some("F7".into()));
        assert!(CHAN_ANY_BOUND.load(Ordering::Relaxed), "binding next-channel sets the flag");
        set_chan_binding(0, Some("F6".into()));
        assert!(CHAN_ANY_BOUND.load(Ordering::Relaxed));
        set_chan_binding(1, None);
        assert!(CHAN_ANY_BOUND.load(Ordering::Relaxed), "one key still bound");
        set_chan_binding(0, Some(String::new()));
        assert!(!CHAN_ANY_BOUND.load(Ordering::Relaxed), "last key unbound clears the flag");
        assert_eq!(*chan_bindings().lock().unwrap(), [None, None]);

        CHAN_SLOT_DOWN[1].store(true, Ordering::SeqCst);
        set_chan_binding(1, Some("F7".into()));
        assert!(!CHAN_SLOT_DOWN[1].load(Ordering::SeqCst), "rebind clears a stuck held flag");
        set_chan_binding(1, None);

        set_chan_binding(2, Some("F5".into()));
        assert_eq!(*chan_bindings().lock().unwrap(), [None, None], "slot 2 is ignored");
        assert!(!CHAN_ANY_BOUND.load(Ordering::Relaxed));

        assert_eq!(CHAN_CAPTURE_SLOT.load(Ordering::SeqCst), -1);
        start_chan_capture(0);
        assert_eq!(CHAN_CAPTURE_SLOT.load(Ordering::SeqCst), 0);
        start_chan_capture(7);
        assert_eq!(CHAN_CAPTURE_SLOT.load(Ordering::SeqCst), 0);
        CHAN_CAPTURE_SLOT.store(-1, Ordering::SeqCst);
    }

    #[test]
    fn publish_ptt_merges_keyboard_and_control_sources_into_one_global_state() {
        // Only this test touches PTT_SOURCES. APP_HANDLE is unset in tests, so
        // nothing is emitted; the merged state is what we check.
        let emitted = || PTT_SOURCES.lock().unwrap().emitted;
        assert!(!emitted());
        publish_ptt(Some(true), None);
        assert!(emitted(), "keyboard press transmits");
        publish_ptt(None, Some(true));
        assert!(emitted());
        publish_ptt(Some(false), None);
        assert!(emitted(), "keyboard release while the Stream Deck still holds keeps transmitting");
        publish_ptt(None, Some(false));
        assert!(!emitted(), "last source released stops transmitting");
        publish_ptt(None, None);
        assert!(!emitted(), "a no-op update changes nothing");
    }

    #[test]
    fn ptt_sources_only_report_edges() {
        let mut s = PttSources::default();
        assert_eq!(s.update(None, None), None, "nothing changed, nothing to emit");
        assert_eq!(s.update(Some(true), None), Some(true));
        assert_eq!(s.update(Some(true), None), None, "key auto-repeat is not a new edge");
        assert_eq!(s.update(Some(true), Some(true)), None);
        assert_eq!(s.update(Some(false), Some(false)), Some(false), "both released in one update is one edge");
        assert_eq!(s.update(Some(false), None), None);
    }
}

/// Pure normalizers behind the remaining IPC commands.
#[cfg(test)]
mod ipc_normalizer_tests {
    use super::*;
    use companion_core::audio::{DspConfig, RadioCfg};

    #[test]
    fn valid_channel_counts_chars_not_bytes_and_bans_control_chars() {
        assert!(valid_channel("Funk 1"));
        assert!(valid_channel(&"a".repeat(companion_core::MAX_CHANNEL_LEN)));
        assert!(!valid_channel(&"a".repeat(companion_core::MAX_CHANNEL_LEN + 1)));
        assert!(valid_channel(&"\u{e4}".repeat(companion_core::MAX_CHANNEL_LEN)), "32 umlauts are 32 chars");
        assert!(!valid_channel(""));
        assert!(!valid_channel("Funk\u{0}1"));
        assert!(!valid_channel("Funk\n1"));
    }

    #[test]
    fn clean_token_trims_and_requires_hex_up_to_128() {
        assert_eq!(clean_token(None), Ok(None));
        assert_eq!(clean_token(Some(String::new())), Ok(None));
        assert_eq!(clean_token(Some("   ".into())), Ok(None), "whitespace-only means no token");
        assert_eq!(clean_token(Some("  ab12  ".into())), Ok(Some("ab12".into())));
        assert_eq!(clean_token(Some("f".repeat(128))), Ok(Some("f".repeat(128))));
        assert!(clean_token(Some("f".repeat(129))).is_err());
        assert!(clean_token(Some("not-hex".into())).is_err());
        assert!(clean_token(Some("ab 12".into())).is_err(), "inner whitespace is not trimmed away");
    }

    #[test]
    fn clean_cert_requires_exactly_a_sha256_hex_digest() {
        assert_eq!(clean_cert(None), Ok(None));
        assert_eq!(clean_cert(Some("  ".into())), Ok(None));
        let d = "0123456789abcdef".repeat(4);
        assert_eq!(clean_cert(Some(format!(" {d} "))), Ok(Some(d.clone())));
        assert_eq!(clean_cert(Some(d.to_uppercase())), Ok(Some(d.to_uppercase())), "case is left to the pinning code");
        assert!(clean_cert(Some(d[..63].to_string())).is_err(), "63 chars");
        assert!(clean_cert(Some(format!("{d}0"))).is_err(), "65 chars");
        assert!(clean_cert(Some("g".repeat(64))).is_err(), "non-hex");
        assert!(clean_cert(Some(format!("{}:{}", &d[..31], &d[32..]))).is_err(), "colon-separated fingerprints are not accepted");
    }

    #[test]
    fn device_names_are_trimmed_and_bounded() {
        assert_eq!(clean_device(None), Ok(None));
        assert_eq!(clean_device(Some("".into())), Ok(None));
        assert_eq!(clean_device(Some("  Mic (USB)  ".into())), Ok(Some("Mic (USB)".into())));
        assert_eq!(clean_device(Some("m".repeat(256))), Ok(Some("m".repeat(256))));
        assert!(clean_device(Some("m".repeat(257))).is_err(), "connect refuses an absurd name");

        // The live switch has nobody to report an error to: it falls back to default.
        assert_eq!(live_device(None), None);
        assert_eq!(live_device(Some("   ".into())), None);
        assert_eq!(live_device(Some(" Speakers ".into())).as_deref(), Some("Speakers"));
        assert_eq!(live_device(Some("m".repeat(256))).map(|s| s.len()), Some(256));
        assert_eq!(live_device(Some("m".repeat(257))), None);
    }

    #[test]
    fn bounded_chat_drops_blank_lines_and_caps_at_max_chars() {
        assert_eq!(bounded_chat(""), None);
        assert_eq!(bounded_chat("   \n\t"), None);
        assert_eq!(bounded_chat("hi").as_deref(), Some("hi"));
        assert_eq!(bounded_chat("  hi  ").as_deref(), Some("  hi  "), "surrounding whitespace is kept, only blank-only is dropped");
        let long = "\u{e4}".repeat(MAX_CHAT_LEN + 1);
        let capped = bounded_chat(&long).unwrap();
        assert_eq!(capped.chars().count(), MAX_CHAT_LEN, "cap counts chars, so a multibyte text is not cut mid-codepoint");
        // Whitespace padding past the cap that would leave an empty message
        // after truncation still yields nothing.
        let padded = format!("{}x", " ".repeat(MAX_CHAT_LEN));
        assert_eq!(bounded_chat(&padded), None);
    }

    #[test]
    fn norm_f32_clamps_finite_values_and_defaults_the_rest() {
        assert_eq!(norm_f32(0.5, 0.0, 1.0, 0.3), 0.5);
        assert_eq!(norm_f32(-1.0, 0.0, 1.0, 0.3), 0.0);
        assert_eq!(norm_f32(7.0, 0.0, 1.0, 0.3), 1.0);
        assert_eq!(norm_f32(f32::NAN, 0.0, 1.0, 0.3), 0.3);
        assert_eq!(norm_f32(f32::INFINITY, 0.0, 1.0, 0.3), 0.3);
        assert_eq!(norm_f32(f32::NEG_INFINITY, 0.0, 1.0, 0.3), 0.3);
    }

    #[test]
    fn normalize_dsp_leaves_defaults_alone_and_repairs_bad_floats() {
        let d = DspConfig::default();
        let n = normalize_dsp(d);
        assert_eq!(
            (n.gate_threshold, n.comp_threshold, n.comp_ratio, n.comp_makeup, n.limiter_ceiling),
            (d.gate_threshold, d.comp_threshold, d.comp_ratio, d.comp_makeup, d.limiter_ceiling)
        );
        assert_eq!((n.gate, n.compressor, n.limiter), (d.gate, d.compressor, d.limiter));

        let bad = DspConfig {
            gate: false,
            gate_threshold: f32::NAN,
            compressor: false,
            comp_threshold: f32::INFINITY,
            comp_ratio: -3.0,
            comp_makeup: 100.0,
            limiter: true,
            limiter_ceiling: 0.0,
        };
        let n = normalize_dsp(bad);
        assert_eq!(n.gate_threshold, 0.015, "NaN -> default");
        assert_eq!(n.comp_threshold, 0.15, "inf -> default");
        assert_eq!(n.comp_ratio, 1.0, "clamped to the floor");
        assert_eq!(n.comp_makeup, 4.0, "clamped to the ceiling");
        assert_eq!(n.limiter_ceiling, 0.05, "clamped to the floor");
        assert!(!n.gate && !n.compressor && n.limiter, "switches pass through untouched");
    }

    #[test]
    fn normalize_radio_keeps_a_real_passband_for_any_input() {
        let d = RadioCfg::default();
        let n = normalize_radio(d);
        assert_eq!((n.low_cut, n.high_cut, n.saturation, n.destruction), (300.0, 3400.0, 0.35, 0.15));
        assert_eq!(n.enabled, d.enabled);

        let n = normalize_radio(RadioCfg { enabled: true, low_cut: f32::NAN, high_cut: f32::NAN, saturation: 5.0, destruction: -1.0 });
        assert_eq!((n.low_cut, n.high_cut, n.saturation, n.destruction), (300.0, 3400.0, 1.0, 0.0));
        assert!(n.enabled);

        // A user cannot invert the band: low is capped at 1 kHz, high floors at 1.2 kHz.
        for (lo, hi) in [(5000.0, 100.0), (1000.0, 1200.0), (f32::INFINITY, f32::NEG_INFINITY), (0.0, 1e9)] {
            let n = normalize_radio(RadioCfg { low_cut: lo, high_cut: hi, ..RadioCfg::default() });
            assert!(n.low_cut < n.high_cut, "({lo}, {hi}) -> ({}, {})", n.low_cut, n.high_cut);
            assert!((50.0..=1000.0).contains(&n.low_cut));
            assert!((1200.0..=12000.0).contains(&n.high_cut));
        }
    }

    #[test]
    fn control_state_is_capped_at_16_kib_serialized() {
        assert!(control_state_ok(&serde_json::json!({ "connected": true, "channel": "Funk 1" })));
        assert!(control_state_ok(&serde_json::Value::Null));
        // A JSON string serializes with two quotes: 16382 chars -> exactly 16384 bytes.
        let at_cap = serde_json::Value::String("x".repeat(MAX_CONTROL_STATE - 2));
        assert!(control_state_ok(&at_cap), "exactly at the cap is accepted");
        let over = serde_json::Value::String("x".repeat(MAX_CONTROL_STATE - 1));
        assert!(!control_state_ok(&over), "one byte over is dropped");
    }
}
