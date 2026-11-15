//! Updates: ask GitHub for the newest release, and install it in one click.
//! Downloads are checked against the release's SHA256SUMS; on macOS the new
//! app must also be signed by the same developer as the running one.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};
use sha2::{Digest, Sha256};

const REPO: &str = "iRunTech/remote-duo";
pub const RELEASES_PAGE: &str = "https://github.com/iRunTech/remote-duo/releases/latest";
/// Generous: the Mac zip is a few MB.
const MAX_DOWNLOAD: u64 = 200 << 20;

#[derive(Clone, Debug)]
pub struct Release {
    pub tag: String,
    /// Without the leading "v".
    pub version: String,
}

/// The newest release, if it's newer than this build.
pub fn check() -> anyhow::Result<Option<Release>> {
    let text = ureq::get(&format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .header("User-Agent", concat!("remote-duo-host/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .call()?
        .body_mut()
        .read_to_string()?;
    let body: serde_json::Value = serde_json::from_str(&text)?;
    let tag = body["tag_name"].as_str().context("no tag_name in release")?.to_string();
    let version = tag.trim_start_matches('v').to_string();
    Ok(newer(&version, env!("CARGO_PKG_VERSION")).then_some(Release { tag, version }))
}

/// Compares dotted version numbers ("0.10.0" > "0.9.3").
fn newer(candidate: &str, current: &str) -> bool {
    let parts = |v: &str| v.split(['.', '-']).map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parts(candidate) > parts(current)
}

fn get(url: &str) -> anyhow::Result<Vec<u8>> {
    Ok(ureq::get(url)
        .header("User-Agent", concat!("remote-duo-host/", env!("CARGO_PKG_VERSION")))
        .call()?
        .body_mut()
        .with_config()
        .limit(MAX_DOWNLOAD)
        .read_to_vec()?)
}

/// Downloads a release file and checks it against SHA256SUMS.
fn download_verified(release: &Release, name: &str, dir: &Path) -> anyhow::Result<PathBuf> {
    let base = format!("https://github.com/{REPO}/releases/download/{}", release.tag);
    let sums = String::from_utf8(get(&format!("{base}/SHA256SUMS"))?)?;
    let expected = sums
        .lines()
        .find_map(|l| {
            let (hash, file) = l.split_once(char::is_whitespace)?;
            (file.trim().trim_start_matches('*') == name).then(|| hash.to_lowercase())
        })
        .with_context(|| format!("no checksum listed for {name}"))?;
    let bytes = get(&format!("{base}/{name}"))?;
    let actual: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if actual != expected {
        bail!("{name} didn't match its checksum");
    }
    let path = dir.join(name);
    std::fs::write(&path, bytes)?;
    Ok(path)
}

fn run(cmd: &mut Command) -> anyhow::Result<String> {
    let out = cmd.output().with_context(|| format!("couldn't run {cmd:?}"))?;
    if !out.status.success() {
        bail!("{cmd:?} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr))
}

/// The running .app bundle, if there is one.
#[cfg(target_os = "macos")]
pub fn app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let app = exe.ancestors().nth(3)?;
    (app.extension()? == "app").then(|| app.to_path_buf())
}

#[cfg(target_os = "macos")]
fn team_id(app: &Path) -> Option<String> {
    let out = run(Command::new("codesign").args(["-dv", "--verbose=2"]).arg(app)).ok()?;
    out.lines()
        .find_map(|l| l.strip_prefix("TeamIdentifier="))
        .filter(|t| *t != "not set")
        .map(str::to_string)
}

/// Installs `release` in place of this copy. Afterwards call `relaunch`
/// (Windows: the installer restarts the app itself, so just quit).
pub fn install(release: &Release) -> anyhow::Result<()> {
    let dir = std::env::temp_dir().join(format!("remote-duo-update-{}", release.version));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let result = install_from(release, &dir);
    if !cfg!(windows) {
        let _ = std::fs::remove_dir_all(&dir);
    }
    result
}

#[cfg(target_os = "macos")]
fn install_from(release: &Release, dir: &Path) -> anyhow::Result<()> {
    let app = app_bundle().context("this copy isn't an installed app; download the update instead")?;
    let team = team_id(&app).context("this copy was built locally; download the update instead")?;
    let zip = download_verified(release, "RemoteDuoHost-macOS.zip", dir)?;
    run(Command::new("ditto").args(["-x", "-k"]).arg(&zip).arg(dir))?;
    let fresh = dir.join("Remote Duo Host.app");
    run(Command::new("codesign").args(["--verify", "--deep", "--strict"]).arg(&fresh))?;
    if team_id(&fresh).as_deref() != Some(team.as_str()) {
        bail!("the update isn't signed by the same developer");
    }

    // Copy next to the old app first, so a failure leaves it untouched.
    let staged = app.with_extension("app-new");
    let old = app.with_extension("app-old");
    let _ = std::fs::remove_dir_all(&staged);
    let _ = std::fs::remove_dir_all(&old);
    run(Command::new("ditto").arg(&fresh).arg(&staged)).context("couldn't write to the Applications folder")?;
    std::fs::rename(&app, &old)?;
    if let Err(e) = std::fs::rename(&staged, &app) {
        let _ = std::fs::rename(&old, &app);
        return Err(e.into());
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

#[cfg(target_os = "linux")]
fn install_from(release: &Release, dir: &Path) -> anyhow::Result<()> {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => bail!("no build for {other}"),
    };
    let tarball = download_verified(release, &format!("remote-duo-host-linux-{arch}.tar.gz"), dir)?;
    run(Command::new("tar").arg("-xzf").arg(&tarball).arg("-C").arg(dir))?;
    let exe = std::env::current_exe()?;
    let staged = exe.with_extension("new");
    std::fs::copy(dir.join("remote-duo-host/remote-duo-host"), &staged)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    // Replacing a running binary by rename is fine on Linux.
    std::fs::rename(&staged, &exe).context("couldn't replace the installed copy")?;
    Ok(())
}

#[cfg(windows)]
fn install_from(release: &Release, dir: &Path) -> anyhow::Result<()> {
    let setup = download_verified(release, "RemoteDuoHost-Windows-Setup.exe", dir)?;
    // The installer stops this copy, replaces it, and starts the new one.
    Command::new(setup).args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART"]).spawn()?;
    Ok(())
}

/// Starts the freshly installed copy once this one has exited. Waiting on our
/// pid rather than a fixed delay matters on macOS: `open` on an app that is
/// still shutting down just activates the old copy, which then quits.
pub fn relaunch() {
    #[cfg(unix)]
    let pid = std::process::id().to_string();
    #[cfg(target_os = "macos")]
    if let Some(app) = app_bundle() {
        let _ = Command::new("/bin/sh")
            .arg("-c")
            .arg(r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; open "$0""#)
            .arg(app)
            .arg(&pid)
            .spawn();
    }
    #[cfg(target_os = "linux")]
    if let Ok(exe) = std::env::current_exe() {
        let _ = Command::new("/bin/sh")
            .arg("-c")
            .arg(r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; exec "$0""#)
            .arg(exe)
            .arg(&pid)
            .spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::newer;

    #[test]
    fn compares_versions() {
        assert!(newer("0.2.0", "0.1.0"));
        assert!(newer("0.10.0", "0.9.3"));
        assert!(newer("1.0.0", "0.99.99"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
    }
}
