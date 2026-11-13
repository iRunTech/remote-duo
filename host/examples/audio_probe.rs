//! Dev tool: pair, open the audio channel, and record ~3s to a WAV file.
//! Usage: cargo run --example audio_probe -- 127.0.0.1:47800 <pin> out.wav

use std::io::Write;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

mod common;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let url = args.get(1).map(String::as_str).unwrap_or("127.0.0.1:47800");
    let pin = args.get(2).cloned().unwrap_or_default();
    let out_path = args.get(3).cloned().unwrap_or_else(|| "out.wav".into());

    let mut ws = common::connect(url).await?;
    let hello = serde_json::json!({ "t": "hello", "name": "audio-probe", "pin": pin });
    ws.send(Message::Text(hello.to_string().into())).await?;
    let reply: serde_json::Value = serde_json::from_str(ws.next().await.unwrap()?.to_text()?)?;
    let token = reply["token"].as_str().ok_or_else(|| anyhow::anyhow!("pairing failed: {reply}"))?.to_string();

    let mut audio = common::connect(url).await?;
    let hello = serde_json::json!({ "t": "hello", "name": "audio-probe", "token": token, "channel": "audio" });
    audio.send(Message::Text(hello.to_string().into())).await?;
    println!("welcome: {}", audio.next().await.unwrap()?);

    let (mut pcm, mut packets, mut rate, mut channels) = (Vec::new(), 0, 48_000u32, 2u16);
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        let Ok(Some(msg)) = tokio::time::timeout(Duration::from_secs(3), audio.next()).await else { break };
        match msg? {
            Message::Binary(data) if data.first() == Some(&4) => {
                packets += 1;
                channels = data[1] as u16;
                rate = u32::from_be_bytes([data[2], data[3], data[4], data[5]]);
                pcm.extend_from_slice(&data[6..]);
            }
            Message::Text(t) => println!("text: {t}"),
            _ => {}
        }
    }
    let peak = pcm.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b).unsigned_abs()).max().unwrap_or(0);
    println!("{packets} packets, {:.2}s of {channels}ch {rate} Hz audio, peak level {peak}",
             pcm.len() as f64 / (rate as f64 * channels as f64 * 2.0));

    let mut f = std::fs::File::create(&out_path)?;
    let byte_rate = rate * channels as u32 * 2;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + pcm.len() as u32).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&channels.to_le_bytes())?;
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&byte_rate.to_le_bytes())?;
    f.write_all(&(channels * 2).to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&(pcm.len() as u32).to_le_bytes())?;
    f.write_all(&pcm)?;
    Ok(())
}
