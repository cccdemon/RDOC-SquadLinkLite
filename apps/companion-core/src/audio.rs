//! Native audio: cpal capture/playback + linear resampling around 48 kHz Opus.
//! Device thread owns the cpal streams (kept off the async runtime). Encode/
//! decode/mix run on plain std threads (audiopus stays out of tokio).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use audiopus::coder::{Decoder, Encoder};
use audiopus::{Application, Channels, SampleRate};
use bytes::Bytes;
use nnnoiseless::DenoiseState;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

pub const OPUS_SR: u32 = 48000;
pub const FRAME: usize = 960; // 20 ms mono @ 48 kHz

pub type Buf = Arc<Mutex<VecDeque<i16>>>;
pub type MixMap = Arc<Mutex<HashMap<String, VecDeque<i16>>>>;

/// Output gains: one master + per-peer (user_id → factor). 1.0 = unchanged.
/// Applied live in the mixer; both clamp to 0.0..2.0 (0 = mute, 2 = +6 dB).
#[derive(Default)]
pub struct Gains {
    master: Mutex<Option<f32>>, // None ⇒ 1.0
    peers: Mutex<HashMap<String, f32>>,
}
impl Gains {
    pub fn new() -> Self {
        Gains { master: Mutex::new(Some(1.0)), peers: Mutex::new(HashMap::new()) }
    }
    pub fn set_master(&self, v: f32) {
        *self.master.lock().unwrap() = Some(v.clamp(0.0, 2.0));
    }
    pub fn set_peer(&self, peer: &str, v: f32) {
        self.peers.lock().unwrap().insert(peer.to_string(), v.clamp(0.0, 2.0));
    }
    fn master_v(&self) -> f32 {
        self.master.lock().unwrap().unwrap_or(1.0)
    }
    fn peer_v(&self, peer: &str) -> f32 {
        *self.peers.lock().unwrap().get(peer).unwrap_or(&1.0)
    }
}

/// Mix key for the local "Funk-Klick" earcon. Not a real peer → `peer_v` returns
/// the 1.0 default, so the click bypasses per-peer gain and just rides the master
/// bus and tanh limiter like any other source.
pub const EARCON_KEY: &str = "__earcon__";

/// A short "radio click" played LOCALLY through the subraum output device at the
/// start of an incoming transmission, so the listener can tell subraum voice
/// apart from game/other audio. Toggleable; off = `click()` is a no-op.
pub struct Earcon {
    mix: MixMap,
    enabled: Arc<AtomicBool>,
    /// Playback gain for the click, as f32 bits (0.0 mute … 1.0 default … 2.0 +6 dB).
    volume: Arc<AtomicU32>,
    samples: Vec<i16>,
}
impl Earcon {
    pub fn new(mix: MixMap, enabled: Arc<AtomicBool>) -> Self {
        Earcon { mix, enabled, volume: Arc::new(AtomicU32::new(1.0f32.to_bits())), samples: render_click() }
    }
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::SeqCst);
    }
    /// Set the click playback volume (0.0 mute … 1.0 normal … 2.0 +6 dB). Live.
    pub fn set_volume(&self, v: f32) {
        let v = if v.is_finite() { v.clamp(0.0, 2.0) } else { 1.0 };
        self.volume.store(v.to_bits(), Ordering::SeqCst);
    }
    /// Queue one click into the mixer (no-op when disabled or muted). Cheap +
    /// non-blocking; safe to call from the async engine loop on a peer's PTT-onset.
    pub fn click(&self) {
        if !self.enabled.load(Ordering::SeqCst) {
            return;
        }
        let vol = f32::from_bits(self.volume.load(Ordering::SeqCst));
        if vol <= 0.0 {
            return; // muted click
        }
        let mut m = self.mix.lock().unwrap();
        let b = m.entry(EARCON_KEY.to_string()).or_default();
        b.clear(); // drop any un-played click so rapid PTT toggling can't pile up
        if (vol - 1.0).abs() < f32::EPSILON {
            b.extend(self.samples.iter().copied());
        } else {
            b.extend(self.samples.iter().map(|&s| (s as f32 * vol).clamp(-32768.0, 32767.0) as i16));
        }
    }
}

/// Render the click once: two short decaying ticks ("k-chk", ~28 ms total) at
/// 48 kHz mono i16 — the familiar push-to-talk key sound.
fn render_click() -> Vec<i16> {
    let sr = OPUS_SR as f32;
    let mut out: Vec<i16> = Vec::new();
    // (gap-before-tick ms, frequency Hz, amplitude 0..1, decay time-constant ms)
    for (start_ms, freq, amp, tau_ms) in [(0.0f32, 1800.0f32, 0.28f32, 1.5f32), (18.0, 1300.0, 0.20, 2.0)] {
        let gap = (start_ms / 1000.0 * sr) as usize;
        while out.len() < gap {
            out.push(0);
        }
        let n = (tau_ms * 5.0 / 1000.0 * sr) as usize; // ~5 time-constants of decay
        for i in 0..n {
            let t = i as f32 / sr;
            let env = (-t / (tau_ms / 1000.0)).exp();
            let s = (2.0 * std::f32::consts::PI * freq * t).sin() * env * amp;
            out.push((s.clamp(-1.0, 1.0) * 32767.0) as i16);
        }
    }
    out
}

/// List input + output device names for the settings UI.
pub fn list_devices() -> (Vec<String>, Vec<String>) {
    let host = cpal::default_host();
    let ins = host
        .input_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default();
    let outs = host
        .output_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default();
    (ins, outs)
}

/// Streaming linear resampler (mono f32), phase preserved across calls.
pub struct Resampler {
    step: f64,
    t: f64,
    prev: f32,
    have_prev: bool,
}
impl Resampler {
    pub fn new(src: u32, dst: u32) -> Self {
        Self { step: src as f64 / dst as f64, t: 0.0, prev: 0.0, have_prev: false }
    }
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        for &cur in input {
            if !self.have_prev {
                self.prev = cur;
                self.have_prev = true;
                continue;
            }
            while self.t < 1.0 {
                out.push(self.prev + (cur - self.prev) * self.t as f32);
                self.t += self.step;
            }
            self.t -= 1.0;
            self.prev = cur;
        }
    }
}

struct Picked {
    device: cpal::Device,
    config: StreamConfig,
    fmt: SampleFormat,
    channels: u16,
    rate: u32,
}

fn choose(host: &cpal::Host, input: bool, want: Option<&str>) -> Result<Picked> {
    let kind = if input { "Input" } else { "Output" };
    let devs: Vec<cpal::Device> =
        if input { host.input_devices()?.collect() } else { host.output_devices()?.collect() };
    let device = if let Some(w) = want.filter(|s| !s.is_empty()) {
        devs.iter()
            .find(|d| d.name().map(|n| n.contains(w)).unwrap_or(false))
            .cloned()
            .ok_or_else(|| anyhow!("{kind}-Device '{w}' nicht gefunden"))?
    } else if input {
        host.default_input_device().ok_or_else(|| anyhow!("kein Default-Input"))?
    } else {
        host.default_output_device().ok_or_else(|| anyhow!("kein Default-Output"))?
    };
    let s = if input { device.default_input_config()? } else { device.default_output_config()? };
    Ok(Picked {
        config: s.config(),
        fmt: s.sample_format(),
        channels: s.channels(),
        rate: s.sample_rate().0,
        device,
    })
}

fn build_input(p: &Picked, cap: Buf) -> Result<cpal::Stream> {
    let ch = p.channels as usize;
    let err = |e| eprintln!("input stream error: {e}");
    Ok(match p.fmt {
        SampleFormat::F32 => p.device.build_input_stream(
            &p.config,
            move |data: &[f32], _: &_| {
                let mut b = cap.lock().unwrap();
                for fr in data.chunks(ch) {
                    let s: f32 = fr.iter().copied().sum::<f32>() / ch as f32;
                    b.push_back((s.clamp(-1.0, 1.0) * 32767.0) as i16);
                }
            },
            err,
            None,
        )?,
        SampleFormat::I16 => p.device.build_input_stream(
            &p.config,
            move |data: &[i16], _: &_| {
                let mut b = cap.lock().unwrap();
                for fr in data.chunks(ch) {
                    let s: i32 = fr.iter().map(|&x| x as i32).sum::<i32>() / ch as i32;
                    b.push_back(s as i16);
                }
            },
            err,
            None,
        )?,
        other => return Err(anyhow!("Input-Format {other:?} nicht unterstützt")),
    })
}

fn build_output(p: &Picked, play: Buf) -> Result<cpal::Stream> {
    let ch = p.channels as usize;
    let err = |e| eprintln!("output stream error: {e}");
    Ok(match p.fmt {
        SampleFormat::F32 => p.device.build_output_stream(
            &p.config,
            move |data: &mut [f32], _: &_| {
                let mut b = play.lock().unwrap();
                for fr in data.chunks_mut(ch) {
                    let v = b.pop_front().unwrap_or(0) as f32 / 32768.0;
                    for o in fr.iter_mut() {
                        *o = v;
                    }
                }
            },
            err,
            None,
        )?,
        SampleFormat::I16 => p.device.build_output_stream(
            &p.config,
            move |data: &mut [i16], _: &_| {
                let mut b = play.lock().unwrap();
                for fr in data.chunks_mut(ch) {
                    let v = b.pop_front().unwrap_or(0);
                    for o in fr.iter_mut() {
                        *o = v;
                    }
                }
            },
            err,
            None,
        )?,
        other => return Err(anyhow!("Output-Format {other:?} nicht unterstützt")),
    })
}

/// Live device-switch request to the device thread. `None` = system default.
pub enum DevCmd {
    SetInput(Option<String>),
    SetOutput(Option<String>),
}

/// Device thread: pick devices, build + play streams, report rates, then park
/// (cpal streams must outlive the program and stay off the async runtime).
///
/// Stays alive and watches `dev_rx` so the input/output device can be switched
/// LIVE (without a reconnect): on a request it drops the affected stream, re-
/// opens the new device, and publishes its sample rate via `in_rate`/`out_rate`
/// so the encode/mixer resamplers retune. A new device's rate can differ (e.g.
/// 48k vs 192k), which is why the rates are shared atomics, not constructor args.
#[allow(clippy::too_many_arguments)]
pub fn run_devices(
    cap: Buf,
    play: Buf,
    rate_tx: std::sync::mpsc::Sender<(u32, u32)>,
    in_name: Option<String>,
    out_name: Option<String>,
    stop: Arc<AtomicBool>,
    in_rate: Arc<AtomicU32>,
    out_rate: Arc<AtomicU32>,
    dev_rx: std::sync::mpsc::Receiver<DevCmd>,
) {
    let host = cpal::default_host();
    let in_want = in_name.or_else(|| std::env::var("IN_DEVICE").ok());
    let out_want = out_name.or_else(|| std::env::var("OUT_DEVICE").ok());
    let inp = choose(&host, true, in_want.as_deref()).expect("Input-Device");
    let outp = choose(&host, false, out_want.as_deref()).expect("Output-Device");
    eprintln!(
        "Input : {} @ {}Hz | Output: {} @ {}Hz",
        inp.device.name().unwrap_or_default(),
        inp.rate,
        outp.device.name().unwrap_or_default(),
        outp.rate
    );
    in_rate.store(inp.rate, Ordering::SeqCst);
    out_rate.store(outp.rate, Ordering::SeqCst);
    let _ = rate_tx.send((inp.rate, outp.rate)); // unblocks setup_audio once devices are open
    let mut in_s = Some(build_input(&inp, cap.clone()).expect("input stream"));
    let mut out_s = Some(build_output(&outp, play.clone()).expect("output stream"));
    in_s.as_ref().unwrap().play().expect("play input");
    out_s.as_ref().unwrap().play().expect("play output");

    // Hold the streams alive until shutdown; dropping them stops capture/playback.
    while !stop.load(Ordering::SeqCst) {
        while let Ok(cmd) = dev_rx.try_recv() {
            match cmd {
                // Build + start the NEW stream before dropping the old one: if the
                // new device fails (unsupported format, unplugged, …) the current
                // device keeps running instead of going permanently silent.
                DevCmd::SetInput(name) => {
                    let want = name.or_else(|| std::env::var("IN_DEVICE").ok());
                    match choose(&host, true, want.as_deref()) {
                        Ok(p) => match build_input(&p, cap.clone()) {
                            Ok(s) if s.play().is_ok() => {
                                in_rate.store(p.rate, Ordering::SeqCst);
                                in_s = Some(s); // replaces (drops) the old stream now that the new one runs
                            }
                            Ok(_) => eprintln!("input switch: new device won't start; keeping current"),
                            Err(e) => eprintln!("input rebuild failed: {e}; keeping current"),
                        },
                        Err(e) => eprintln!("input switch failed: {e}; keeping current"),
                    }
                }
                DevCmd::SetOutput(name) => {
                    let want = name.or_else(|| std::env::var("OUT_DEVICE").ok());
                    match choose(&host, false, want.as_deref()) {
                        Ok(p) => match build_output(&p, play.clone()) {
                            Ok(s) if s.play().is_ok() => {
                                out_rate.store(p.rate, Ordering::SeqCst);
                                out_s = Some(s); // replaces (drops) the old stream now that the new one runs
                            }
                            Ok(_) => eprintln!("output switch: new device won't start; keeping current"),
                            Err(e) => eprintln!("output rebuild failed: {e}; keeping current"),
                        },
                        Err(e) => eprintln!("output switch failed: {e}; keeping current"),
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Configurable capture-path DSP chain. All three stages are toggleable and live
/// (read each frame). Thresholds/ceiling are linear (0..1, i.e. fraction of full
/// scale). Defaults are on; the limiter prevents the makeup-gain clipping that
/// caused occasional crackle.
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct DspConfig {
    pub gate: bool,
    pub gate_threshold: f32, // below this the noise gate closes (~0.015 ≈ -36 dB)
    pub compressor: bool,
    pub comp_threshold: f32, // knee start (~0.15 ≈ -16 dB)
    pub comp_ratio: f32,     // >= 1
    pub comp_makeup: f32,    // post-gain
    pub limiter: bool,
    pub limiter_ceiling: f32, // hard peak ceiling (~0.97)
}
impl Default for DspConfig {
    fn default() -> Self {
        DspConfig {
            gate: true,
            gate_threshold: 0.015,
            compressor: true,
            comp_threshold: 0.15,
            comp_ratio: 3.0,
            comp_makeup: 1.4,
            limiter: true,
            limiter_ceiling: 0.97,
        }
    }
}

/// Per-stream DSP state (noise gate → compressor → limiter), mono −1..1.
struct Dsp {
    gate_env: f32,
    gate_gain: f32,
    comp_env: f32,
    lim_gain: f32,
    atk: f32,
    rel: f32,
    gate_open: f32,
    gate_close: f32,
    lim_rel: f32,
}
impl Dsp {
    fn new() -> Self {
        let sr = OPUS_SR as f32;
        Dsp {
            gate_env: 0.0,
            gate_gain: 1.0,
            comp_env: 0.0,
            lim_gain: 1.0,
            atk: (-1.0f32 / (0.005 * sr)).exp(),        // comp ~5 ms attack
            rel: (-1.0f32 / (0.080 * sr)).exp(),        // comp ~80 ms release
            gate_open: (-1.0f32 / (0.002 * sr)).exp(),  // gate opens fast (2 ms)
            gate_close: (-1.0f32 / (0.150 * sr)).exp(), // gate closes slow (150 ms)
            lim_rel: (-1.0f32 / (0.050 * sr)).exp(),    // limiter release 50 ms
        }
    }
    fn process(&mut self, x: f32, c: &DspConfig) -> f32 {
        let mut s = x;
        if c.gate {
            let a = s.abs();
            self.gate_env = if a > self.gate_env {
                a
            } else {
                self.rel * self.gate_env + (1.0 - self.rel) * a
            };
            let target = if self.gate_env >= c.gate_threshold { 1.0 } else { 0.0 };
            let coef = if target > self.gate_gain { self.gate_open } else { self.gate_close };
            self.gate_gain = coef * self.gate_gain + (1.0 - coef) * target;
            s *= self.gate_gain;
        }
        if c.compressor {
            let a = s.abs();
            let coef = if a > self.comp_env { self.atk } else { self.rel };
            self.comp_env = coef * self.comp_env + (1.0 - coef) * a;
            let gain = if self.comp_env > c.comp_threshold {
                (c.comp_threshold + (self.comp_env - c.comp_threshold) / c.comp_ratio.max(1.0))
                    / self.comp_env.max(1e-6)
            } else {
                1.0
            };
            s = s * gain * c.comp_makeup;
        }
        if c.limiter {
            let ceil = c.limiter_ceiling.clamp(0.05, 1.0);
            let peak = s.abs();
            let target = if peak > ceil { ceil / peak } else { 1.0 };
            if target < self.lim_gain {
                self.lim_gain = target; // instant attack: never let a peak through
            } else {
                self.lim_gain = self.lim_rel * self.lim_gain + (1.0 - self.lim_rel); // release →1
            }
            s *= self.lim_gain;
        }
        s.clamp(-1.0, 1.0) // final safety net
    }
}

/// Radio-effect config ("Funk-Effekt"): band-limit + light saturation +
/// optional degradation, applied to the RECEIVE mix bus in `mixer_loop` — every
/// incoming voice, after per-peer gains, before master volume.
///
/// Receive-side on purpose: it is each listener's own immersion choice, it
/// never touches what the mesh carries (no protocol change, encode-once
/// untouched), and a destruction stage in the send path would fight RNNoise and
/// the Opus encoder while being baked into what everyone else hears. What you
/// SEND stays clean. Everything on the local mix bus is colored, though — that
/// includes the earcon click and the mic self-test loopback ("self"), which
/// doubles as an effect preview without needing a second person.
///
/// Cutoffs are Hz at the 48 kHz mixer rate; `saturation`/`destruction` are
/// 0..1. Defaults approximate a narrow-band FM voice channel (300–3400 Hz).
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct RadioCfg {
    pub enabled: bool,
    /// High-pass cutoff — "FX Tiefen abschneiden".
    pub low_cut: f32,
    /// Low-pass cutoff — "FX Höhen abschneiden".
    pub high_cut: f32,
    /// Drive into a tanh waveshaper, 0 = clean.
    pub saturation: f32,
    /// Bitcrush: sample-hold decimation + amplitude quantization, 0 = off.
    pub destruction: f32,
}
impl Default for RadioCfg {
    fn default() -> Self {
        RadioCfg { enabled: false, low_cut: 300.0, high_cut: 3400.0, saturation: 0.35, destruction: 0.15 }
    }
}

/// RBJ-cookbook biquad (Q = 0.707), coefficients recomputed only when the
/// cutoff moves. Direct form 1; state is tiny, one instance per filter.
struct Biquad {
    b0: f32, b1: f32, b2: f32, a1: f32, a2: f32,
    x1: f32, x2: f32, y1: f32, y2: f32,
    /// Cutoff the coefficients were computed for (0 = never).
    tuned_for: f32,
    highpass: bool,
}
impl Biquad {
    fn new(highpass: bool) -> Self {
        Biquad { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0, x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0, tuned_for: 0.0, highpass }
    }
    fn tune(&mut self, cutoff: f32) {
        if cutoff == self.tuned_for {
            return;
        }
        self.tuned_for = cutoff;
        let w = 2.0 * std::f32::consts::PI * cutoff / OPUS_SR as f32;
        let (sw, cw) = (w.sin(), w.cos());
        let q = std::f32::consts::FRAC_1_SQRT_2; // Butterworth Q = 0.707
        let alpha = sw / (2.0 * q);
        let a0 = 1.0 + alpha;
        if self.highpass {
            self.b0 = (1.0 + cw) / 2.0 / a0;
            self.b1 = -(1.0 + cw) / a0;
            self.b2 = self.b0;
        } else {
            self.b0 = (1.0 - cw) / 2.0 / a0;
            self.b1 = (1.0 - cw) / a0;
            self.b2 = self.b0;
        }
        self.a1 = -2.0 * cw / a0;
        self.a2 = (1.0 - alpha) / a0;
    }
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2 - self.a1 * self.y1 - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Per-bus radio-effect state. Mono −1..1 at the 48 kHz mixer rate.
pub struct RadioFx {
    hp: Biquad,
    lp: Biquad,
    /// Bitcrush sample-hold: the value being repeated and the repeat phase.
    held: f32,
    hold_phase: f32,
}
impl Default for RadioFx {
    fn default() -> Self {
        Self::new()
    }
}
impl RadioFx {
    pub fn new() -> Self {
        RadioFx { hp: Biquad::new(true), lp: Biquad::new(false), held: 0.0, hold_phase: 0.0 }
    }

    /// Process one sample. Chain: HPF → LPF → saturation → destruction.
    /// Saturation is drive→tanh with makeup, so turning it up changes texture
    /// more than loudness. Destruction decimates (repeat every Nth sample) and
    /// quantizes amplitude — the "cheap handset" grit.
    pub fn process(&mut self, x: f32, c: &RadioCfg) -> f32 {
        if !c.enabled {
            return x;
        }
        self.hp.tune(c.low_cut);
        self.lp.tune(c.high_cut);
        let mut s = self.lp.process(self.hp.process(x));
        if c.saturation > 0.0 {
            let drive = 1.0 + c.saturation * 9.0; // 0..1 → 1..10
            // Makeup normalized at a −6 dB reference: a typical voice level maps
            // back onto itself, so the slider changes texture, not loudness.
            // (Unit-gain-at-full-scale makeup turned −9 dB voice into a nearly
            // full-scale square — a 2.7× loudness jump.)
            const REF: f32 = 0.5;
            s = (s * drive).tanh() * (REF / (REF * drive).tanh());
        }
        if c.destruction > 0.0 {
            // Decimation: hold each sample for 1..=12 ticks (48 kHz → down to 4 kHz).
            let factor = 1.0 + c.destruction * 11.0;
            self.hold_phase += 1.0;
            if self.hold_phase >= factor {
                self.hold_phase -= factor;
                self.held = s;
            }
            s = self.held;
            // Quantization: 16 → ~5 bit equivalent.
            let bits = 16.0 - c.destruction * 11.0;
            let steps = 2f32.powf(bits - 1.0);
            s = (s * steps).round() / steps;
        }
        s.clamp(-1.0, 1.0)
    }
}

/// Capture → resample(in→48k) → RNNoise noise-suppression (10ms blocks) →
/// compressor → 20ms frame → (if transmitting) Opus encode → WebRTC writer task.
///
/// RNNoise removes background noise (fan, keyboard, hum). It is NOT echo
/// cancellation — without a headset, speaker echo still leaks; full APM-AEC
/// (libwebrtc) doesn't build on Windows-MSVC, so headset stays recommended.
#[allow(clippy::too_many_arguments)]
pub fn encode_loop(
    cap: Buf,
    in_rate: Arc<AtomicU32>, // live capture rate (changes on a device switch)
    transmit: Arc<AtomicBool>,
    opus_tx: UnboundedSender<Bytes>,
    dsp_cfg: Arc<Mutex<DspConfig>>,
    mix: MixMap,
    monitor: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    bitrate: Arc<AtomicU32>, // 0 = Opus auto; else target bits/s (low-bw mode)
    dtx: Arc<AtomicBool>,    // app-level DTX: don't send silent frames
) {
    const NS: usize = DenoiseState::FRAME_SIZE; // 480 = 10ms @ 48k
    let mut enc = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Voip)
        .expect("opus encoder");
    let mut last_br = u32::MAX;
    let mut cur_rate = in_rate.load(Ordering::SeqCst);
    let mut up = Resampler::new(cur_rate, OPUS_SR);
    let mut den = DenoiseState::new();
    let mut dsp = Dsp::new();
    let mut buf48: Vec<f32> = Vec::new(); // post-resample, −1..1
    let mut den_in = [0f32; NS];
    let mut den_out = [0f32; NS];
    let mut clean: Vec<f32> = Vec::new(); // post-denoise, −1..1
    let mut frame = [0i16; FRAME];
    let mut encoded = [0u8; 4000];
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        // Capture device switched → its rate may differ; retune the resampler.
        let r = in_rate.load(Ordering::SeqCst);
        if r != cur_rate {
            cur_rate = r;
            up = Resampler::new(cur_rate, OPUS_SR);
        }
        let chunk: Vec<f32> = {
            let mut b = cap.lock().unwrap();
            if b.is_empty() {
                Vec::new()
            } else {
                b.drain(..).map(|s| s as f32 / 32768.0).collect()
            }
        };
        if chunk.is_empty() {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        up.process(&chunk, &mut buf48);
        // Denoise in 10ms blocks. RNNoise works in i16-scaled f32.
        while buf48.len() >= NS {
            for (i, s) in buf48.drain(..NS).enumerate() {
                den_in[i] = s * 32768.0;
            }
            den.process_frame(&mut den_out, &den_in);
            for s in den_out.iter() {
                clean.push((s / 32768.0).clamp(-1.0, 1.0));
            }
        }
        while clean.len() >= FRAME {
            let cfg = *dsp_cfg.lock().unwrap(); // snapshot once per 20 ms frame
            for (i, s) in clean.drain(..FRAME).enumerate() {
                frame[i] = (dsp.process(s, &cfg) * 32767.0) as i16;
            }
            // Mic self-check: route the processed mic to local playback as "self".
            if monitor.load(Ordering::SeqCst) {
                let mut m = mix.lock().unwrap();
                let b = m.entry("self".to_string()).or_default();
                for s in frame.iter() {
                    b.push_back(*s);
                }
                while b.len() > 9600 {
                    b.pop_front(); // ~200ms cap
                }
            }
            // Mic self-check is a LOCAL loopback only: while monitoring, never
            // encode/send to peers (otherwise the room hears the test).
            if transmit.load(Ordering::SeqCst) && !monitor.load(Ordering::SeqCst) {
                // Live bitrate (low-bandwidth mode).
                let br = bitrate.load(Ordering::SeqCst);
                if br != last_br {
                    let _ = enc.set_bitrate(if br == 0 {
                        audiopus::Bitrate::Auto
                    } else {
                        audiopus::Bitrate::BitsPerSecond(br as i32)
                    });
                    last_br = br;
                }
                // App-level DTX: skip near-silent frames → no packets during silence.
                let silent = frame.iter().all(|&s| (s as i32).abs() < 250);
                if !(dtx.load(Ordering::SeqCst) && silent) {
                    if let Ok(n) = enc.encode(&frame[..], &mut encoded[..]) {
                        let _ = opus_tx.send(Bytes::copy_from_slice(&encoded[..n]));
                    }
                }
            }
        }
    }
}

/// Decode incoming Opus frames (per peer) → push i16 @48k into the mix map.
pub fn decode_loop(mut rx: UnboundedReceiver<(String, Bytes)>, mix: MixMap) {
    let mut decoders: HashMap<String, Decoder> = HashMap::new();
    let mut out = [0i16; FRAME];
    while let Some((peer, payload)) = rx.blocking_recv() {
        let dec = decoders
            .entry(peer.clone())
            .or_insert_with(|| Decoder::new(SampleRate::Hz48000, Channels::Mono).expect("opus decoder"));
        if let Ok(n) = dec.decode(Some(&payload[..]), &mut out[..], false) {
            let mut m = mix.lock().unwrap();
            let b = m.entry(peer).or_default();
            for s in &out[..n] {
                b.push_back(*s);
            }
            while b.len() > 19200 {
                b.pop_front(); // cap ~400ms jitter
            }
        }
    }
}

/// Demand-driven mixer: keeps the playback ring topped up to ~60ms of audio,
/// then sleeps briefly. A fixed 20ms sleep is ~31ms on Windows (timer
/// granularity) → the ring drains → underrun crackle; producing on demand
/// decouples from sleep precision. Sum is soft-limited (tanh) so several
/// simultaneous speakers can't hard-clip.
pub fn mixer_loop(
    mix: MixMap,
    play: Buf,
    out_rate: Arc<AtomicU32>,
    gains: Arc<Gains>,
    radio: Arc<Mutex<RadioCfg>>,
    stop: Arc<AtomicBool>,
) {
    let mut cur_rate = out_rate.load(Ordering::SeqCst);
    let mut down = Resampler::new(OPUS_SR, cur_rate);
    let mut fx = RadioFx::new();
    let mut target = cur_rate as usize * 60 / 1000; // ~60ms buffered (absorbs OS jitter)
    let mut cap = cur_rate as usize / 2; // hard cap ~0.5s
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        // Output device switched → its rate may differ; retune the resampler.
        let r = out_rate.load(Ordering::SeqCst);
        if r != cur_rate {
            cur_rate = r;
            down = Resampler::new(OPUS_SR, cur_rate);
            target = cur_rate as usize * 60 / 1000;
            cap = cur_rate as usize / 2;
        }
        // Refill the playback ring up to `target`.
        loop {
            if play.lock().unwrap().len() >= target {
                break;
            }
            let mut mixed = [0i32; FRAME];
            {
                let mut m = mix.lock().unwrap();
                for (peer, b) in m.iter_mut() {
                    // Partial frame contributes what it has (rest = silence).
                    let n = b.len().min(FRAME);
                    if n > 0 {
                        let g = gains.peer_v(peer);
                        for x in mixed.iter_mut().take(n) {
                            *x += (b.pop_front().unwrap() as f32 * g) as i32;
                        }
                    }
                }
            }
            let master = gains.master_v();
            let rc = *radio.lock().unwrap();
            // Radio effect on the summed voice bus, BEFORE master volume and the
            // tanh guard: saturating the sum gives the intermodulation a shared
            // radio channel would, and the master fader stays a clean fader.
            // tanh ≈ linear for quiet signals, smoothly saturates near ±1 → no
            // hard-clip clicks when multiple peers speak at once.
            let f: Vec<f32> = mixed
                .iter()
                .map(|&v| (fx.process(v as f32 / 32768.0, &rc) * master).tanh())
                .collect();
            let mut o: Vec<f32> = Vec::new();
            down.process(&f, &mut o);
            {
                let mut p = play.lock().unwrap();
                for v in o {
                    p.push_back((v * 32767.0) as i16);
                }
                while p.len() > cap {
                    p.pop_front();
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod radio_fx_tests {
    use super::*;

    /// RMS of a pure sine pushed through the effect, transient skipped.
    fn rms_through(fx: &mut RadioFx, c: &RadioCfg, freq: f32) -> f32 {
        let sr = OPUS_SR as f32;
        let n = OPUS_SR as usize; // 1 s
        let mut acc = 0.0f64;
        let mut counted = 0usize;
        for i in 0..n {
            let x = (2.0 * std::f32::consts::PI * freq * i as f32 / sr).sin() * 0.5;
            let y = fx.process(x, c);
            if i >= n / 4 {
                acc += (y as f64) * (y as f64);
                counted += 1;
            }
        }
        (acc / counted as f64).sqrt() as f32
    }

    fn band_only() -> RadioCfg {
        RadioCfg { enabled: true, saturation: 0.0, destruction: 0.0, ..RadioCfg::default() }
    }

    #[test]
    fn disabled_is_a_bit_exact_passthrough() {
        let mut fx = RadioFx::new();
        let c = RadioCfg { enabled: false, ..RadioCfg::default() };
        for i in 0..1000 {
            let x = ((i * 7919) % 2001) as f32 / 1000.0 - 1.0;
            assert_eq!(fx.process(x, &c), x);
        }
    }

    /// The band edges must actually cut: 300–3400 Hz passband, a rumble below
    /// and a hiss above both land well under the in-band level.
    #[test]
    fn bandpass_shapes_the_spectrum() {
        let c = band_only();
        let inband = rms_through(&mut RadioFx::new(), &c, 1000.0);
        let rumble = rms_through(&mut RadioFx::new(), &c, 60.0);
        let hiss = rms_through(&mut RadioFx::new(), &c, 12000.0);
        assert!(inband > 0.3, "1 kHz must pass, rms={inband}");
        assert!(rumble < inband * 0.25, "60 Hz must be cut: {rumble} vs {inband}");
        assert!(hiss < inband * 0.25, "12 kHz must be cut: {hiss} vs {inband}");
    }

    /// Tighter cutoffs must bite harder — the sliders have to do something.
    #[test]
    fn cutoffs_respond_to_the_sliders() {
        let wide = band_only();
        let narrow = RadioCfg { low_cut: 600.0, high_cut: 1800.0, ..band_only() };
        let lo_wide = rms_through(&mut RadioFx::new(), &wide, 300.0);
        let lo_narrow = rms_through(&mut RadioFx::new(), &narrow, 300.0);
        assert!(lo_narrow < lo_wide * 0.6, "raising low_cut must cut 300 Hz harder");
        let hi_wide = rms_through(&mut RadioFx::new(), &wide, 3000.0);
        let hi_narrow = rms_through(&mut RadioFx::new(), &narrow, 3000.0);
        assert!(hi_narrow < hi_wide * 0.6, "lowering high_cut must cut 3 kHz harder");
    }

    /// Saturation reshapes without a level jump (makeup) and never exceeds ±1.
    #[test]
    fn saturation_is_bounded_and_level_stable() {
        let clean = band_only();
        let hot = RadioCfg { saturation: 1.0, ..band_only() };
        let r_clean = rms_through(&mut RadioFx::new(), &clean, 1000.0);
        let r_hot = rms_through(&mut RadioFx::new(), &hot, 1000.0);
        assert!(r_hot <= 1.0 && r_hot > r_clean * 0.5 && r_hot < r_clean * 2.0,
            "saturation must not double or halve the level: {r_clean} -> {r_hot}");

        let mut fx = RadioFx::new();
        for i in 0..4800 {
            let x = (2.0 * std::f32::consts::PI * 440.0 * i as f32 / OPUS_SR as f32).sin();
            let y = fx.process(x, &hot);
            assert!((-1.0..=1.0).contains(&y));
        }
    }

    /// Full destruction still yields signal (not silence, not NaN) — it is a
    /// texture, not a mute.
    #[test]
    fn destruction_degrades_but_never_kills() {
        let c = RadioCfg { destruction: 1.0, ..band_only() };
        let r = rms_through(&mut RadioFx::new(), &c, 1000.0);
        assert!(r.is_finite() && r > 0.05, "destroyed signal must survive, rms={r}");
    }

    /// Retuning mid-stream (slider drag) must not blow up the filter state.
    #[test]
    fn live_retune_stays_finite() {
        let mut fx = RadioFx::new();
        let mut c = band_only();
        for i in 0..(OPUS_SR as usize) {
            if i % 4800 == 0 {
                c.low_cut = 100.0 + (i / 4800) as f32 * 80.0;
                c.high_cut = 8000.0 - (i / 4800) as f32 * 500.0;
            }
            let x = (2.0 * std::f32::consts::PI * 700.0 * i as f32 / OPUS_SR as f32).sin();
            let y = fx.process(x, &c);
            assert!(y.is_finite());
        }
    }
}
