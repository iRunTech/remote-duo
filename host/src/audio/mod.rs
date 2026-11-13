//! Computer sound for the phone: raw PCM over the audio channel.
//! Packet: `0x04 | u8 channels | u32 sample_rate | s16le interleaved samples`.

#[cfg(target_os = "macos")]
mod mac;
#[cfg(windows)]
mod windows;

use tokio::sync::mpsc;

/// About half a second of 10 ms chunks; beyond that, drop rather than lag.
const QUEUE: usize = 50;

/// A running capture. Dropping it stops capture.
pub struct AudioSource {
    pub rx: mpsc::Receiver<Vec<u8>>,
    _running: Box<dyn Send>,
}

pub fn start() -> anyhow::Result<AudioSource> {
    let (tx, rx) = mpsc::channel(QUEUE);
    #[cfg(target_os = "macos")]
    let running = mac::start(tx)?;
    #[cfg(windows)]
    let running = windows::start(tx)?;
    #[cfg(not(any(target_os = "macos", windows)))]
    let running: Box<dyn Send> = {
        drop(tx);
        anyhow::bail!("Computer sound isn't available on {} yet", std::env::consts::OS)
    };
    Ok(AudioSource { rx, _running: running })
}

/// Build a packet from interleaved float samples in -1...1.
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
fn packet(channels: u8, rate: u32, samples: impl ExactSizeIterator<Item = f32>) -> Vec<u8> {
    let mut out = Vec::with_capacity(6 + samples.len() * 2);
    out.push(0x04);
    out.push(channels);
    out.extend_from_slice(&rate.to_be_bytes());
    for s in samples {
        out.extend_from_slice(&((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes());
    }
    out
}
