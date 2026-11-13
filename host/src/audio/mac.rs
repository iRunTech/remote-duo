//! macOS: ScreenCaptureKit system audio (excluding our own process).

use screencapturekit::prelude::*;
use tokio::sync::mpsc;

const RATE: u32 = 48_000;
const CHANNELS: u32 = 2;

pub fn start(tx: mpsc::Sender<Vec<u8>>) -> anyhow::Result<Box<dyn Send>> {
    let content = SCShareableContent::get().map_err(|e| {
        anyhow::anyhow!(
            "can't capture sound ({e:?}). Grant Screen Recording permission to this app \
             in System Settings → Privacy & Security, then restart the host"
        )
    })?;
    let display = content.displays().into_iter().next().ok_or_else(|| anyhow::anyhow!("no displays found"))?;
    // Excluding (no) applications, rather than windows, also picks up sound
    // from apps without windows.
    let filter = SCContentFilter::create()
        .with_display(&display)
        .with_excluding_applications(&[], &[])
        .build()?;
    // ScreenCaptureKit always captures video too; keep it tiny and slow.
    let config = SCStreamConfiguration::new()
        .with_width(2)
        .with_height(2)
        .with_minimum_frame_interval(&CMTime::new(1, 1))
        .with_captures_audio(true)
        .with_sample_rate(RATE as i32)
        .with_channel_count(CHANNELS as i32)
        .with_excludes_current_process_audio(true);

    let mut stream = SCStream::new(&filter, &config)?;
    stream.add_output_handler(|_: CMSampleBuffer, _: SCStreamOutputType| {}, SCStreamOutputType::Screen)?;
    stream.add_output_handler(
        move |sample: CMSampleBuffer, kind: SCStreamOutputType| {
            if kind != SCStreamOutputType::Audio {
                return;
            }
            if let Some(p) = convert(&sample) {
                let _ = tx.try_send(p);
            }
        },
        SCStreamOutputType::Audio,
    )?;
    stream.start_capture()?;
    println!("sound capture started");
    Ok(Box::new(Running(Some(stream))))
}

/// ScreenCaptureKit hands us float32, usually one buffer per channel.
fn convert(sample: &CMSampleBuffer) -> Option<Vec<u8>> {
    let list = sample.audio_buffer_list().ok()?;
    let read = |bytes: &[u8]| -> Vec<f32> {
        bytes.as_chunks::<4>().0.iter().map(|b| f32::from_ne_bytes(*b)).collect()
    };
    let buffers: Vec<Vec<f32>> = (0..list.num_buffers()).filter_map(|i| list.buffer(i)).map(|b| read(b.data())).collect();
    let samples: Vec<f32> = match buffers.as_slice() {
        // Planar: interleave left/right.
        [left, right, ..] => left.iter().zip(right).flat_map(|(l, r)| [*l, *r]).collect(),
        // One buffer: interleaved stereo already, or mono to duplicate.
        [only] if list.get(0)?.number_channels >= 2 => only.clone(),
        [only] => only.iter().flat_map(|s| [*s, *s]).collect(),
        [] => return None,
    };
    Some(super::packet(CHANNELS as u8, RATE, samples.into_iter()))
}

struct Running(Option<SCStream>);

// SCStream is safe to stop and release from any thread.
unsafe impl Send for Running {}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(stream) = self.0.take() {
            let _ = stream.stop_capture();
        }
        println!("sound capture stopped");
    }
}
