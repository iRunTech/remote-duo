//! Dev client for poking a running host without the iOS app.
//! Usage: cargo run --example client -- 127.0.0.1:47800 <pin> [move|center|fit WxH]

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

mod common;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let url = args.get(1).map(String::as_str).unwrap_or("127.0.0.1:47800");
    let pin = args.get(2).cloned().unwrap_or_default();
    let wiggle = args.get(3).is_some_and(|a| a == "move");
    let center = args.get(3).is_some_and(|a| a == "center");

    let hello = |pin: Option<&str>, token: Option<&str>| {
        serde_json::json!({ "t": "hello", "name": "dev-client", "pin": pin, "token": token }).to_string()
    };

    let token = {
        let mut ws = common::connect(url).await?;
        ws.send(Message::Text(hello(Some(&pin), None).into())).await?;
        let reply = ws.next().await.ok_or_else(|| anyhow::anyhow!("closed"))??;
        println!("pair reply: {reply}");
        let v: serde_json::Value = serde_json::from_str(reply.to_text()?)?;
        v["token"].as_str().map(String::from)
    };
    let Some(token) = token else { return Ok(()) };

    let mut ws = common::connect(url).await?;
    ws.send(Message::Text(hello(None, Some(&token)).into())).await?;
    println!("token reply: {}", ws.next().await.ok_or_else(|| anyhow::anyhow!("closed"))??);
    ws.send(Message::Text(r#"{"t":"ping"}"#.into())).await?;
    println!("ping reply: {}", ws.next().await.ok_or_else(|| anyhow::anyhow!("closed"))??);

    if wiggle {
        for i in 0..40 {
            let dx = if i < 20 { -5.0 } else { 5.0 };
            if i == 20 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            ws.send(Message::Text(format!(r#"{{"t":"move","dx":{dx},"dy":0}}"#).into())).await?;
            tokio::time::sleep(std::time::Duration::from_millis(16)).await;
        }
        println!("wiggled the cursor");
    }
    if center {
        ws.send(Message::Text(r#"{"t":"move_abs","x":0.5,"y":0.5}"#.into())).await?;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        println!("moved to screen center");
    }
    if args.get(3).is_some_and(|a| a == "fit") {
        let size = args.get(4).map(String::as_str).unwrap_or("700x480");
        let (w, h) = size.split_once('x').ok_or_else(|| anyhow::anyhow!("size is WxH"))?;
        ws.send(Message::Text(format!(r#"{{"t":"fit","w":{w},"h":{h}}}"#).into())).await?;
        println!("fit reply: {}", ws.next().await.ok_or_else(|| anyhow::anyhow!("closed"))??);
        std::process::Command::new("screencapture").args(["-x", "/tmp/remote-duo-fit.png"]).status()?;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        ws.send(Message::Text(r#"{"t":"unfit"}"#.into())).await?;
        println!("unfit reply: {}", ws.next().await.ok_or_else(|| anyhow::anyhow!("closed"))??);
    }
    ws.close(None).await?;
    Ok(())
}
