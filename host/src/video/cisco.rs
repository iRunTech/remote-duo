//! Cisco's prebuilt OpenH264, the software H.264 encoder for Linux (and the
//! fallback on Windows). Cisco pays the H.264 patent royalties for its own
//! binary, but only when each user's computer downloads it straight from
//! Cisco, so it's never bundled: the first screen view fetches it into the
//! config folder. The openh264 crate checks it against Cisco's published
//! hashes before loading it.
//!
//! Cisco's terms also ask that users can turn it off, next to the text in
//! `NOTICE` (the tray menu does that), and that its license is reproduced
//! (THIRD-PARTY-NOTICES.md).

use std::io::Read;
use std::path::PathBuf;
use std::sync::Mutex;

use openh264::OpenH264API;

pub const NOTICE: &str = "OpenH264 Video Codec provided by Cisco Systems, Inc.";

const BASE_URL: &str = "https://ciscobinary.openh264.org";
/// Cisco's files are about 1 MB compressed; refuse anything wildly bigger.
const MAX_DOWNLOAD: u64 = 16 << 20;

/// Serializes downloads when several screen views start at once.
static DOWNLOAD: Mutex<()> = Mutex::new(());

/// Cisco's file name for this platform (version 2.6.0, which the crate knows).
fn library_name() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "libopenh264-2.6.0-linux64.8.so",
        ("linux", "aarch64") => "libopenh264-2.6.0-linux-arm64.8.so",
        ("windows", "x86_64") => "openh264-2.6.0-win64.dll",
        ("windows", "aarch64") => "openh264-2.6.0-win-arm64.dll",
        ("macos", "aarch64") => "libopenh264-2.6.0-mac-arm64.dylib",
        ("macos", "x86_64") => "libopenh264-2.6.0-mac-x64.dylib",
        _ => return None,
    })
}

fn disabled_marker() -> PathBuf {
    crate::identity::config_dir().join("openh264-disabled")
}

/// Whether the user allows using (and downloading) Cisco's codec.
pub fn enabled() -> bool {
    std::env::var_os("REMOTE_DUO_NO_OPENH264").is_none() && !disabled_marker().exists()
}

pub fn set_enabled(on: bool) -> std::io::Result<()> {
    let marker = disabled_marker();
    if on {
        match std::fs::remove_file(marker) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        std::fs::create_dir_all(crate::identity::config_dir())?;
        std::fs::write(marker, b"")
    }
}

/// Loads Cisco's library, downloading it first if needed.
pub fn api() -> anyhow::Result<OpenH264API> {
    if !enabled() {
        anyhow::bail!(
            "Screen view needs the OpenH264 video codec, which is turned off. Turn on \"{NOTICE}\" \
             in the Remote Duo Host menu."
        );
    }
    let name = library_name()
        .ok_or_else(|| anyhow::anyhow!("no OpenH264 build for this kind of computer"))?;
    let path = crate::identity::config_dir().join(name);
    {
        let _guard = DOWNLOAD.lock().unwrap();
        if !path.exists() {
            download(name, &path).map_err(|e| {
                anyhow::anyhow!(
                    "couldn't download the OpenH264 video codec from Cisco ({e:#}). Screen view needs \
                     it once; check this computer's internet connection and try again"
                )
            })?;
        }
    }
    match OpenH264API::from_blob_path(&path) {
        Ok(api) => Ok(api),
        Err(e) => {
            // Damaged or not Cisco's: fetch a fresh copy next time.
            let _ = std::fs::remove_file(&path);
            Err(anyhow::anyhow!("couldn't load the OpenH264 video codec: {e}"))
        }
    }
}

fn download(name: &str, path: &PathBuf) -> anyhow::Result<()> {
    println!("downloading {name} from Cisco ({NOTICE})");
    let url = format!("{BASE_URL}/{name}.bz2");
    let compressed = ureq::get(&url)
        .call()?
        .body_mut()
        .with_config()
        .limit(MAX_DOWNLOAD)
        .read_to_vec()?;
    let mut library = Vec::new();
    bzip2::read::BzDecoder::new(compressed.as_slice()).read_to_end(&mut library)?;

    std::fs::create_dir_all(path.parent().unwrap())?;
    let partial = path.with_extension("part");
    std::fs::write(&partial, &library)?;
    std::fs::rename(&partial, path)?;
    Ok(())
}
