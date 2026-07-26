//! Tauri shell for RDOC SquadLink Lite. Thin layer over companion-core: commands
//! drive the engine, engine state is forwarded to the webview as "ui" events.
//!
//! Entry point is `run()` — desktop `main.rs` calls it directly; on mobile
//! (Android/iOS) Tauri generates the platform entry that invokes it via
//! `#[tauri::mobile_entry_point]`.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use companion_core::serverless::Serverless;
use companion_core::{start, Engine, EngineConfig, Sink, UiEvent};
use tauri::{AppHandle, Emitter, State};

struct AppState {
    engine: Mutex<Option<Engine>>,
    serverless: Mutex<Option<Arc<Serverless>>>,
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
static PTT_COMBINED: AtomicBool = AtomicBool::new(false); // last emitted transmit state

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
static MOD_SHIFT: AtomicBool = AtomicBool::new(false);
static MOD_CTRL: AtomicBool = AtomicBool::new(false);
static MOD_ALT: AtomicBool = AtomicBool::new(false);
static MOD_META: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Default, PartialEq)]
struct Mods {
    shift: bool,
    ctrl: bool,
    alt: bool,
    meta: bool,
}

/// Which modifier group a raw key label belongs to (None = not a modifier).
fn mod_of_label(code: &str) -> Option<&'static str> {
    match code {
        "ShiftLeft" | "ShiftRight" => Some("Shift"),
        "ControlLeft" | "ControlRight" => Some("Ctrl"),
        "Alt" | "AltGr" => Some("Alt"),
        "MetaLeft" | "MetaRight" => Some("Meta"),
        _ => None,
    }
}

fn held_mods() -> Mods {
    Mods {
        shift: MOD_SHIFT.load(Ordering::SeqCst),
        ctrl: MOD_CTRL.load(Ordering::SeqCst),
        alt: MOD_ALT.load(Ordering::SeqCst),
        meta: MOD_META.load(Ordering::SeqCst),
    }
}

/// All required modifiers held? (Extra held modifiers are tolerated.)
fn mods_ok(req: Mods, held: Mods) -> bool {
    (!req.shift || held.shift)
        && (!req.ctrl || held.ctrl)
        && (!req.alt || held.alt)
        && (!req.meta || held.meta)
}

/// Split a binding string into its required modifiers + base key.
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
    if PTT_COMBINED.swap(combined, Ordering::SeqCst) != combined {
        if let Some(app) = APP_HANDLE.get() {
            let _ = app.emit("ptt", combined);
        }
    }

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
        let class_name: Vec<u16> = "SquadLinkRawInput\0".encode_utf16().collect();
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

// ── Audio ducking: lower other apps' volume while SquadLink voice is active ───
// Windows-only (WASAPI per-session volume). The frontend drives `set_ducking`
// from the combined "I'm transmitting OR a peer is speaking" state.
#[cfg(windows)]
mod ducking {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
    /// Runs on a short-lived MTA thread so it never disturbs Tauri's apartment.
    pub fn set(active: bool) {
        ACTIVE.store(active, Ordering::Relaxed);
        std::thread::spawn(move || unsafe {
            let _ = run(active);
        });
    }

    unsafe fn run(active: bool) -> windows::core::Result<()> {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
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

#[tauri::command]
fn set_ptt_binding(slot: usize, code: Option<String>) {
    if slot >= 2 {
        return;
    }
    // Codes like "F8" / "Mouse:Left" / "Pad:3"; bound length.
    let code = code.filter(|c| !c.is_empty() && c.len() <= 64);
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
    let code = code.filter(|c| !c.is_empty() && c.len() <= 64);
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
fn valid_hex(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.chars().all(|c| c.is_ascii_hexdigit())
}

#[tauri::command]
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
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(|c| c.is_control()) {
        return Err("invalid name (1–64 chars, no control chars)".into());
    }
    let token = match token.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
        Some(t) if valid_hex(&t, 128) => Some(t),
        Some(_) => return Err("invalid token".into()),
        None => None,
    };
    let cert_sha256 = match cert_sha256.map(|c| c.trim().to_string()).filter(|c| !c.is_empty()) {
        Some(c) if c.len() == 64 && valid_hex(&c, 64) => Some(c),
        Some(_) => return Err("invalid cert_sha256 (64 hex chars)".into()),
        None => None,
    };
    let clean_dev = |d: Option<String>| -> Result<Option<String>, String> {
        match d.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
            Some(s) if s.len() <= 256 => Ok(Some(s)),
            Some(_) => Err("invalid device name".into()),
            None => Ok(None),
        }
    };
    let input_device = clean_dev(input_device)?;
    let output_device = clean_dev(output_device)?;

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
    let (s, code) = Serverless::create_offer(make_sink(&app), name).await.map_err(|e| e.to_string())?;
    *state.serverless.lock().unwrap() = Some(Arc::new(s));
    Ok(code)
}

#[tauri::command]
async fn serverless_accept_offer(app: AppHandle, state: State<'_, AppState>, name: String, code: String) -> Result<String, String> {
    let (s, answer) = Serverless::accept_offer(code, make_sink(&app), name).await.map_err(|e| e.to_string())?;
    *state.serverless.lock().unwrap() = Some(Arc::new(s));
    Ok(answer)
}

#[tauri::command]
async fn serverless_accept_answer(state: State<'_, AppState>, code: String) -> Result<(), String> {
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
    let text: String = text.chars().take(MAX_CHAT_LEN).collect();
    if text.trim().is_empty() {
        return;
    }
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
    if name.is_empty()
        || name.chars().count() > companion_core::MAX_CHANNEL_LEN
        || name.chars().any(|c| c.is_control())
    {
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
    if name.is_empty()
        || name.chars().count() > companion_core::MAX_CHANNEL_LEN
        || name.chars().any(|c| c.is_control())
    {
        return Err("invalid channel (1–32 chars, no control chars)".into());
    }
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.remove_channel(name);
    }
    Ok(())
}

#[tauri::command]
fn set_dsp(state: State<AppState>, mut cfg: companion_core::audio::DspConfig) {
    // Normalize at the IPC boundary: reject NaN/inf, clamp to sane ranges.
    let norm = |v: f32, lo: f32, hi: f32, dflt: f32| if v.is_finite() { v.clamp(lo, hi) } else { dflt };
    cfg.gate_threshold = norm(cfg.gate_threshold, 0.0, 0.5, 0.015);
    cfg.comp_threshold = norm(cfg.comp_threshold, 0.01, 1.0, 0.15);
    cfg.comp_ratio = norm(cfg.comp_ratio, 1.0, 20.0, 3.0);
    cfg.comp_makeup = norm(cfg.comp_makeup, 0.1, 4.0, 1.4);
    cfg.limiter_ceiling = norm(cfg.limiter_ceiling, 0.05, 1.0, 0.97);
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.set_dsp(cfg);
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
    let name = name.map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && s.len() <= 256);
    if let Some(e) = state.engine.lock().unwrap().as_ref() {
        e.set_input_device(name);
    }
}

#[tauri::command]
fn set_output_device(state: State<AppState>, name: Option<String>) {
    let name = name.map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && s.len() <= 256);
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

/// True when running inside an MSIX package (Microsoft Store build). Used to skip
/// registry-based deep-link registration (the package manifest declares the
/// `squadlink` protocol) and to hide the self-update UI (the Store updates the app).
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

/// Drained once by the webview on mount: the squadlink:// URL (if any) the app
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
    open_url("https://squadlink.raumdock.org/download/");
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

#[cfg(not(windows))]
fn open_url(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

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
    // be the first plugin: a second launch (e.g. clicking a squadlink:// link while
    // running) forwards its argv here; we surface the deep link to the
    // already-running window instead of opening a new one.
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            if let Some(url) = argv.iter().find(|a| a.starts_with("squadlink://")) {
                let _ = app.emit("deeplink", url.clone());
            }
        }));
    }
    builder
        .plugin(tauri_plugin_deep_link::init())
        .manage(AppState { engine: Mutex::new(None), serverless: Mutex::new(None) })
        .setup(|app| {
            let _ = APP_HANDLE.set(app.handle().clone());
            start_raw_input();
            // Cold start: stash the launch URL for the webview to drain on mount
            // (Windows delivers deep links via argv).
            if let Some(url) = std::env::args().find(|a| a.starts_with("squadlink://")) {
                *pending_deeplink().lock().unwrap() = Some(url);
            }
            // Register the squadlink:// scheme at runtime (dev + portable runs; the
            // NSIS/MSI installer also registers it). In an MSIX/Store build the
            // package manifest declares the protocol, so skip the registry write.
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
            set_earcon_volume
        ])
        .run(tauri::generate_context!())
        .expect("error running tauri app");
}
