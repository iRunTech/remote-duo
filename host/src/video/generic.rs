//! Windows and Linux (and macOS with REMOTE_DUO_GENERIC_CAPTURE=1, for
//! testing): xcap captures a monitor as RGBA frames (DXGI on
//! Windows, PipeWire or X11 on Linux) and Cisco's OpenH264 (see cisco.rs)
//! encodes them in software using its screen-content real-time mode.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use openh264::encoder::{
    BitRate, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, UsageType,
};
use openh264::formats::{RgbaSliceU8, YUVBuffer};
use tokio::sync::mpsc;
use xcap::{Frame, Monitor};

use super::{Control, Packet, cisco};

const FPS: u32 = 30;

pub fn start(max_width: u32, display_id: u32) -> anyhow::Result<(mpsc::Receiver<Packet>, Arc<Control>, Box<dyn Send>)> {
    // Downloads Cisco's library the first time; fail here so the phone hears why.
    cisco::api()?;
    let (tx, rx) = mpsc::channel(4);
    let ctl = Arc::new(Control::default());
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let worker = {
        let (ctl, stop) = (ctl.clone(), stop.clone());
        thread::Builder::new().name("video-encode".into()).spawn(move || {
            // The recorder isn't Send on every platform, so it lives here.
            let (mut source, mut frames) = match open_local(display_id) {
                Ok(r) => {
                    let _ = ready_tx.send(Ok(()));
                    r
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let mut state = None;
            let mut desktop = Watch::new();
            loop {
                let switch = encode_loop(&frames, &tx, &ctl, &stop, max_width, &mut state, &mut desktop);
                drop(source);
                let Some(switch) = switch else { return };
                // Windows changed desktops (sign-in screen or back), which
                // leaves the old capture dark: start the one for the new desktop,
                // retrying while Windows settles.
                loop {
                    if stop.load(Ordering::Relaxed) || tx.is_closed() {
                        return;
                    }
                    match open_source(switch, display_id) {
                        Ok(r) => {
                            (source, frames) = r;
                            break;
                        }
                        Err(e) => {
                            eprintln!("reopening screen capture: {e}");
                            thread::sleep(Duration::from_millis(500));
                        }
                    }
                }
                ctl.force_key.store(true, Ordering::Relaxed);
            }
        })?
    };
    ready_rx.recv()??;
    println!("screen capture started (software H.264)");
    Ok((rx, ctl, Box::new(Running { stop, worker: Some(worker) })))
}

/// Where frames come from: our own capture, or the Windows sign-in helper.
/// Dropping it stops the capture.
enum Source {
    Local(xcap::VideoRecorder),
    #[cfg(windows)]
    SignIn(#[allow(dead_code)] crate::secure::StopOnDrop),
}

impl Drop for Source {
    fn drop(&mut self) {
        // The only kind off Windows.
        #[allow(irrefutable_let_patterns)]
        if let Source::Local(recorder) = self {
            let _ = recorder.stop();
        }
    }
}

type Frames = std::sync::mpsc::Receiver<Frame>;

fn open_local(display_id: u32) -> anyhow::Result<(Source, Frames)> {
    let monitors = Monitor::all().map_err(|e| anyhow::anyhow!("can't list displays: {e}"))?;
    let monitor = monitors
        .into_iter()
        .min_by_key(|m| m.id().ok() != Some(display_id))
        .ok_or_else(|| anyhow::anyhow!("no displays found"))?;
    let (recorder, frames) = monitor
        .video_recorder()
        .map_err(|e| anyhow::anyhow!("can't capture the screen: {e}"))?;
    recorder.start().map_err(|e| anyhow::anyhow!("can't start screen capture: {e}"))?;
    Ok((Source::Local(recorder), frames))
}

#[cfg(windows)]
fn open_source(switch: Switch, display_id: u32) -> anyhow::Result<(Source, Frames)> {
    if switch == Switch::ToSignIn {
        match crate::secure::open_frames(display_id) {
            Ok(stream) => {
                println!("screen view: showing the sign-in screen");
                let (stream, frames) = stream.split();
                return Ok((Source::SignIn(stream), frames));
            }
            // Without the helper, our own capture can't see sign-in, but it
            // keeps the session going until the user's desktop is back.
            Err(e) => eprintln!("sign-in helper unavailable: {e}"),
        }
    }
    open_local(display_id)
}

#[cfg(not(windows))]
fn open_source(_switch: Switch, display_id: u32) -> anyhow::Result<(Source, Frames)> {
    open_local(display_id)
}

#[cfg(windows)]
use crate::secure::{Switch, Watch};

/// Only Windows has a separate sign-in desktop to watch for.
#[cfg(not(windows))]
#[derive(Clone, Copy)]
enum Switch {}

#[cfg(not(windows))]
struct Watch;

#[cfg(not(windows))]
impl Watch {
    fn new() -> Self {
        Watch
    }
    fn poll(&mut self) -> Option<Switch> {
        None
    }
}

struct Running {
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        println!("screen capture stopped");
    }
}

/// Encodes frames until told to stop or the phone goes away (returns None),
/// or until Windows switches desktops, after which the capture has to be
/// reopened.
fn encode_loop(
    frames: &Frames,
    tx: &mpsc::Sender<Packet>,
    ctl: &Control,
    stop: &AtomicBool,
    max_width: u32,
    state: &mut Option<EncoderState>,
    desktop: &mut Watch,
) -> Option<Switch> {
    let mut need_key = false;
    let mut last = Instant::now() - Duration::from_secs(1);
    let interval = Duration::from_secs_f64(1.0 / FPS as f64);
    let epoch = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        if let Some(switch) = desktop.poll() {
            return Some(switch);
        }
        let Ok(mut frame) = frames.recv_timeout(Duration::from_millis(200)) else { continue };
        // Some backends capture as fast as they can; keep only the newest.
        while let Ok(newer) = frames.try_recv() {
            frame = newer;
        }
        let since = last.elapsed();
        if since < interval {
            thread::sleep(interval - since);
        }
        last = Instant::now();

        let (w, h) = output_size(frame.width, frame.height, max_width);
        if state.as_ref().is_none_or(|s| (s.width, s.height) != (w, h)) {
            match EncoderState::new(w, h) {
                Ok(s) => *state = Some(s),
                Err(e) => {
                    eprintln!("encoder: {e}");
                    return None;
                }
            }
        }
        let s = state.as_mut().unwrap();

        let rgba = scale_rgba(&frame.raw, frame.width, frame.height, w, h);
        s.yuv.read_rgb(RgbaSliceU8::new(&rgba, (w as usize, h as usize)));
        if ctl.force_key.swap(false, Ordering::Relaxed) || need_key {
            s.encoder.force_intra_frame();
        }
        let bitstream = match s.encoder.encode(&s.yuv) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("encode failed: {e}");
                continue;
            }
        };
        if matches!(bitstream.frame_type(), FrameType::Skip | FrameType::Invalid) {
            continue;
        }

        // OpenH264 emits Annex-B; the wire wants SPS/PPS split out and AVCC frames.
        let key = matches!(bitstream.frame_type(), FrameType::IDR | FrameType::I);
        let mut avcc = Vec::new();
        for nal in openh264::nal_units(&bitstream.to_vec()) {
            let nal = strip_start_code(nal);
            match nal.first().map(|b| b & 0x1f) {
                Some(7) => s.sps = nal.to_vec(),
                Some(8) => s.pps = nal.to_vec(),
                Some(_) => {
                    avcc.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                    avcc.extend_from_slice(nal);
                }
                None => {}
            }
        }
        if avcc.is_empty() {
            continue;
        }
        if key {
            let config = Packet::Config { width: w as u16, height: h as u16, sps: s.sps.clone(), pps: s.pps.clone() };
            if tx.try_send(config).is_err() {
                need_key = true;
                continue;
            }
            need_key = false;
        } else if need_key {
            continue;
        }
        let pts_us = epoch.elapsed().as_micros() as u64;
        if tx.try_send(Packet::Frame { key, pts_us, data: avcc }).is_err() {
            if tx.is_closed() {
                return None;
            }
            // Network can't keep up: skip ahead to a fresh keyframe.
            need_key = true;
        }
    }
    None
}

struct EncoderState {
    width: u32,
    height: u32,
    encoder: Encoder,
    yuv: YUVBuffer,
    sps: Vec<u8>,
    pps: Vec<u8>,
}

impl EncoderState {
    fn new(width: u32, height: u32) -> anyhow::Result<Self> {
        let config = EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            // Not supported for screen content; OpenH264 warns if left on.
            .adaptive_quantization(false)
            .background_detection(false)
            .bitrate(BitRate::from_bps(bitrate_for(width, height)))
            .max_frame_rate(FrameRate::from_hz(FPS as f32))
            .intra_frame_period(IntraFramePeriod::from_num_frames(FPS * 2))
            .num_threads(4);
        let encoder = Encoder::with_api_config(cisco::api()?, config)?;
        Ok(Self {
            width,
            height,
            encoder,
            yuv: YUVBuffer::new(width as usize, height as usize),
            sps: Vec::new(),
            pps: Vec::new(),
        })
    }
}

/// Software encoding is cheaper than VideoToolbox's, so aim a bit lower.
fn bitrate_for(width: u32, height: u32) -> u32 {
    let pixels = (width * height) as f64;
    (6_000_000.0 * pixels / (1920.0 * 1080.0)).clamp(2_000_000.0, 12_000_000.0) as u32
}

/// Fit within `max_width`, keeping aspect, with even dimensions for H.264.
fn output_size(width: u32, height: u32, max_width: u32) -> (u32, u32) {
    let scale = (max_width as f64 / width as f64).min(1.0);
    let even = |v: u32| ((v as f64 * scale) as u32).max(2) & !1;
    (even(width), even(height))
}

fn strip_start_code(nal: &[u8]) -> &[u8] {
    let zeros = nal.iter().take_while(|&&b| b == 0).count();
    if zeros >= 2 && nal.get(zeros) == Some(&1) { &nal[zeros + 1..] } else { nal }
}

/// Box-filter downscale (each output pixel averages the source pixels it
/// covers), which keeps small text legible. Identity when sizes match.
fn scale_rgba(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    if (sw, sh) == (dw, dh) {
        return src.to_vec();
    }
    let span = |i: usize, s: usize, d: usize| (i * s / d, ((i + 1) * s / d).max(i * s / d + 1).min(s));
    let cols: Vec<(usize, usize)> = (0..dw).map(|x| span(x, sw, dw)).collect();
    let mut out = vec![0u8; dw * dh * 4];
    for y in 0..dh {
        let (y0, y1) = span(y, sh, dh);
        for (x, &(x0, x1)) in cols.iter().enumerate() {
            let mut acc = [0u32; 4];
            for row in y0..y1 {
                let base = row * sw * 4;
                for px in &src[base + x0 * 4..base + x1 * 4].as_chunks::<4>().0[..] {
                    for c in 0..4 {
                        acc[c] += px[c] as u32;
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            let o = (y * dw + x) * 4;
            for c in 0..4 {
                out[o + c] = (acc[c] / n) as u8;
            }
        }
    }
    out
}
