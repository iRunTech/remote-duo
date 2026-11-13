//! Windows: WASAPI loopback of the default output device (an input stream
//! opened on an output device), via cpal.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SizedSample};
use tokio::sync::mpsc;

pub fn start(tx: mpsc::Sender<Vec<u8>>) -> anyhow::Result<Box<dyn Send>> {
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let worker = {
        let stop = stop.clone();
        // cpal streams aren't Send; keep this one on its own thread.
        thread::Builder::new().name("audio-capture".into()).spawn(move || {
            match open(tx) {
                Ok(stream) => {
                    let _ = ready_tx.send(Ok(()));
                    while !stop.load(Ordering::Relaxed) {
                        thread::sleep(Duration::from_millis(100));
                    }
                    drop(stream);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        })?
    };
    ready_rx.recv()??;
    println!("sound capture started");
    Ok(Box::new(Running { stop, worker: Some(worker) }))
}

fn open(tx: mpsc::Sender<Vec<u8>>) -> anyhow::Result<cpal::Stream> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("no sound output device"))?;
    let config = device.default_output_config()?;
    let (format, stream_config) = (config.sample_format(), config.config());
    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, &stream_config, tx, |s| s)?,
        SampleFormat::I16 => build::<i16>(&device, &stream_config, tx, |s| s as f32 / i16::MAX as f32)?,
        SampleFormat::I32 => build::<i32>(&device, &stream_config, tx, |s| s as f32 / i32::MAX as f32)?,
        other => anyhow::bail!("unsupported sound format {other:?}"),
    };
    stream.play()?;
    Ok(stream)
}

fn build<T: SizedSample + Copy + Send + 'static>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    tx: mpsc::Sender<Vec<u8>>,
    to_f32: fn(T) -> f32,
) -> anyhow::Result<cpal::Stream> {
    let (channels, rate) = (config.channels as usize, config.sample_rate.0);
    Ok(device.build_input_stream(
        config,
        move |data: &[T], _| {
            // Down- or up-mix to stereo.
            let frames = data.chunks(channels.max(1));
            let stereo = frames.flat_map(|f| {
                let l = to_f32(f[0]);
                let r = if f.len() > 1 { to_f32(f[1]) } else { l };
                [l, r]
            });
            let samples: Vec<f32> = stereo.collect();
            let _ = tx.try_send(super::packet(2, rate, samples.into_iter()));
        },
        |e| eprintln!("sound capture error: {e}"),
        None,
    )?)
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
        println!("sound capture stopped");
    }
}
