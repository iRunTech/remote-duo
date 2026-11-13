# Remote Duo

Turn your iPhone into a trackpad, keyboard, and remote for any computer — macOS, Windows, or Linux.
Built for the iPhone Duo: folded it's a trackpad; unfolded in portrait, the top half (above the fold) shows
your computer's screen and the bottom half is the trackpad and keyboard.

This repo is the open-source **computer side** (the host). The iPhone app is coming to the App Store.

```
host/                  Rust host that runs on the computer (input, screen, sound, Bonjour, pairing)
protocol/PROTOCOL.md   wire protocol between the host and the phone
scripts/               packaging
```

## Install

Get it from **[iruntech.github.io/remote-duo](https://iruntech.github.io/remote-duo/)**, or from a terminal:

```sh
# macOS or Linux
curl -fsSL https://iruntech.github.io/remote-duo/install.sh | sh
```

```powershell
# Windows (PowerShell)
irm https://iruntech.github.io/remote-duo/install.ps1 | iex
```

Installers and checksums for every version are on the [releases page](../../releases).

A book icon appears in the menu bar (or system tray) with the pairing PIN, **Show Pairing QR
Code…**, connected phones, **Start at Login**, and **Quit**. On macOS, the first time a phone
connects, macOS asks to allow **Accessibility** (mouse/keyboard) and **Screen Recording** (screen
view) for Remote Duo Host. Linux needs `libayatana-appindicator3` for the tray; on Wayland, the
desktop asks which screen to share the first time you open the screen view. On Windows, set your
network to *Private* so the phone can find the PC.

- Port: `47800` (override with `REMOTE_DUO_PORT`)
- Paired devices are stored in `~/Library/Application Support/remote-duo/devices.json`
  (Windows: `%APPDATA%\remote-duo`, Linux: `~/.config/remote-duo`)

## Build from source

```sh
cd host && cargo build --release      # any OS
./scripts/bundle-macos.sh             # macOS: builds dist/Remote Duo Host.app
```

`remote-duo-host --headless` prints the PIN and a QR code instead of showing a tray icon (used
automatically on Linux without a desktop session).

**Releasing:** bump `version` in `host/Cargo.toml`, commit, then `git tag v<version> && git push
--tags`. The [release workflow](.github/workflows/release.yml) builds the signed Mac app (DMG and
zip), the Windows installer, and Linux tarballs, and publishes them with checksums.

## Connect your iPhone

Open Remote Duo on the iPhone, tap **+**, and pick your computer from the ones found on your
Wi-Fi, then enter the PIN from the host's menu (or scan **Show Pairing QR Code…** with the
Camera app).

**Outside your home:** install [Tailscale](https://tailscale.com) on both devices, then use
**+** → *Connect by address* with the computer's Tailscale name (e.g. `studio.tail1234.ts.net`).

## Fit to Duo (Mac)

Tap the arrows in the top-left corner of the screen view to give the Mac a screen shaped exactly
like the phone's screen half, so the view fills it with no black bars. The Mac's own displays
mirror it while it's on, and go back when you tap again or disconnect. Pick how big text looks in
Settings → Fit to Duo → Text size. Handy for a Mac with no monitor at all.

## Gestures

| Gesture | Action |
|---|---|
| 1 finger move | move cursor |
| tap / tap twice | click / double click |
| 2-finger tap | right click |
| 2-finger move | scroll (with momentum) |
| hold, then move | drag |
| 3-finger move | drag |
| 3-finger tap | middle click |

## Roadmap

1. ✅ Trackpad, keyboard, media remote over LAN · Bonjour discovery · PIN/QR pairing
2. ✅ Live screen view on macOS hosts (ScreenCaptureKit → VideoToolbox H.264), tap to click
3. ✅ Screen view on Windows and Linux (xcap capture + OpenH264) · menu bar / tray app
4. ✅ TLS on both channels with certificate pinning
5. ✅ Computer sound (macOS, Windows) · display picker · zoom follows the cursor

## License

The host and protocol are licensed under the [Apache License 2.0](LICENSE). "Remote Duo" and its
icon are trademarks of iRunTech; forks are welcome but please give them a different name.
