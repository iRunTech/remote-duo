//! Files dropped on a desktop app's window, saved to this computer's
//! Downloads folder. See protocol/PROTOCOL.md, "Files channel".

use std::path::{Path, PathBuf};

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio_tungstenite::tungstenite::Message;

/// Bigger than anyone should drag onto a remote screen.
const MAX_SIZE: u64 = 20 << 30;

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum FileMsg {
    /// A file follows, as binary frames adding up to `size` bytes.
    File { name: String, size: u64 },
}

/// Receives files until the client hangs up, answering each with `saved`
/// (and where) or `error`.
pub async fn receive<Tx, Rx>(mut tx: Tx, mut rx: Rx, device: &str) -> anyhow::Result<()>
where
    Tx: SinkExt<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
    Rx: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let reply = |v: serde_json::Value| Message::Text(v.to_string().into());
    while let Some(frame) = rx.next().await {
        let Message::Text(text) = frame? else { continue };
        let Ok(FileMsg::File { name, size }) = serde_json::from_str(text.as_str()) else { continue };
        if size > MAX_SIZE {
            tx.send(reply(serde_json::json!({"t": "error", "msg": "That file is too big to send."}))).await?;
            return Ok(());
        }
        let Some(dir) = downloads() else {
            tx.send(reply(serde_json::json!({"t": "error", "msg": "This computer has no Downloads folder."}))).await?;
            return Ok(());
        };
        let path = free_path(&dir, &clean_name(&name));
        // Written under a temporary name, so an interrupted transfer never
        // looks like a finished file.
        let partial = path.with_file_name(format!(
            ".{}.part",
            path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        ));
        let mut file = tokio::fs::File::create(&partial).await?;
        let mut received = 0u64;
        let result = async {
            while received < size {
                match rx.next().await {
                    Some(Ok(Message::Binary(chunk))) => {
                        received += chunk.len() as u64;
                        anyhow::ensure!(received <= size, "more data than announced");
                        file.write_all(&chunk).await?;
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    Some(Ok(_)) | None => anyhow::bail!("the transfer stopped partway"),
                    Some(Err(e)) => return Err(e.into()),
                }
            }
            file.flush().await?;
            anyhow::Ok(())
        }
        .await;
        drop(file);
        if let Err(e) = result {
            let _ = tokio::fs::remove_file(&partial).await;
            return Err(e);
        }
        tokio::fs::rename(&partial, &path).await?;
        eprintln!("received {} from {device}", path.display());
        let saved = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        tx.send(reply(serde_json::json!({"t": "saved", "name": saved}))).await?;
    }
    Ok(())
}

fn downloads() -> Option<PathBuf> {
    let dir = dirs::download_dir().or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Just a file name: no folders (so nothing lands outside Downloads), no
/// characters Windows forbids, not hidden, and not a reserved device name.
fn clean_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let mut clean: String = base
        .chars()
        .map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
        .collect();
    clean = clean.trim().trim_start_matches('.').trim_end_matches(['.', ' ']).to_string();
    let stem = clean.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit());
    if clean.is_empty() || reserved {
        clean = format!("File {clean}").trim().to_string();
    }
    // Most file systems cap names at 255 bytes.
    while clean.len() > 200 {
        clean.pop();
    }
    clean
}

/// `name` in `dir`, or "name (2).ext" and so on if that's taken.
fn free_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .expect("some number is free")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_stay_in_the_folder_and_valid() {
        assert_eq!(clean_name("report.pdf"), "report.pdf");
        assert_eq!(clean_name("../../etc/passwd"), "passwd");
        assert_eq!(clean_name(r"C:\Windows\evil.exe"), "evil.exe");
        assert_eq!(clean_name(".bashrc"), "bashrc");
        assert_eq!(clean_name("a<b>c?.txt"), "a_b_c_.txt");
        assert_eq!(clean_name("CON.txt"), "File CON.txt");
        assert_eq!(clean_name(""), "File");
    }

    #[test]
    fn taken_names_get_a_number() {
        let dir = std::env::temp_dir().join(format!("rd-files-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"").unwrap();
        assert_eq!(free_path(&dir, "a.txt"), dir.join("a (2).txt"));
        assert_eq!(free_path(&dir, "b.txt"), dir.join("b.txt"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
