//! Dev tool: pair, open the video channel, and dump ~3s of stream to an
//! Annex-B .h264 file you can inspect with ffprobe/ffplay.
//! Usage: cargo run --example video_probe -- 127.0.0.1:47800 <pin> out.h264 [fit WxH]

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
    let out_path = args.get(3).cloned().unwrap_or_else(|| "out.h264".into());

    let mut ws = common::connect(url).await?;
    let hello = serde_json::json!({ "t": "hello", "name": "video-probe", "pin": pin });
    ws.send(Message::Text(hello.to_string().into())).await?;
    let reply: serde_json::Value = serde_json::from_str(ws.next().await.unwrap()?.to_text()?)?;
    println!("displays: {}", reply["displays"]);
    let token = reply["token"].as_str().ok_or_else(|| anyhow::anyhow!("pairing failed: {reply}"))?.to_string();

    // Optionally switch to a phone-shaped virtual display and stream that.
    let mut display = serde_json::Value::Null;
    if args.get(4).is_some_and(|a| a == "fit") {
        let (w, h) = args.get(5).and_then(|s| s.split_once('x')).unwrap_or(("900", "630"));
        ws.send(Message::Text(format!(r#"{{"t":"fit","w":{w},"h":{h}}}"#).into())).await?;
        let reply: serde_json::Value = serde_json::from_str(ws.next().await.unwrap()?.to_text()?)?;
        println!("fitted: {reply}");
        display = reply["display"].clone();
    }

    let mut video = common::connect(url).await?;
    let hello = serde_json::json!({ "t": "hello", "name": "video-probe", "token": token, "channel": "video", "max_width": 2000, "display": display });
    video.send(Message::Text(hello.to_string().into())).await?;
    println!("welcome: {}", video.next().await.unwrap()?);

    let mut file = std::fs::File::create(&out_path)?;
    let (mut frames, mut keys, mut bytes, mut cursors) = (0, 0, 0usize, 0);
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        let Ok(Some(msg)) = tokio::time::timeout(Duration::from_secs(3), video.next()).await else { break };
        let Message::Binary(data) = msg? else { continue };
        bytes += data.len();
        match data[0] {
            1 => {
                let w = u16::from_be_bytes([data[1], data[2]]);
                let h = u16::from_be_bytes([data[3], data[4]]);
                let sps_len = u16::from_be_bytes([data[5], data[6]]) as usize;
                let sps = &data[7..7 + sps_len];
                let pps = &data[9 + sps_len..];
                if keys == 0 { println!("config {w}x{h}, sps {} bytes, pps {} bytes", sps.len(), pps.len()); }
                for nal in [sps, pps] { file.write_all(&[0, 0, 0, 1])?; file.write_all(nal)?; }
            }
            2 => {
                frames += 1;
                if data[1] == 1 { keys += 1; }
                let mut avcc = &data[10..];
                while avcc.len() >= 4 {
                    let len = u32::from_be_bytes([avcc[0], avcc[1], avcc[2], avcc[3]]) as usize;
                    file.write_all(&[0, 0, 0, 1])?;
                    file.write_all(&avcc[4..4 + len])?;
                    avcc = &avcc[4 + len..];
                }
            }
            3 => {
                cursors += 1;
                let x = f32::from_be_bytes([data[1], data[2], data[3], data[4]]);
                let y = f32::from_be_bytes([data[5], data[6], data[7], data[8]]);
                if cursors == 1 { println!("cursor at {x:.3}, {y:.3} (client draws: {})", data[9] == 1); }
            }
            _ => {}
        }
        if frames == 30 { video.send(Message::Text(r#"{"t":"keyframe"}"#.into())).await?; }
    }
    let secs = start.elapsed().as_secs_f64();
    println!("{cursors} cursor updates");
    println!("{frames} frames ({keys} keyframes) in {secs:.1}s, {:.1} Mbps", bytes as f64 * 8.0 / secs / 1e6);
    Ok(())
}
