#!/bin/sh
# Installs Remote Duo Host on macOS or Linux:
#   curl -fsSL https://iruntech.github.io/remote-duo/install.sh | sh
#
# macOS: puts "Remote Duo Host.app" in /Applications (or ~/Applications) and opens it.
# Linux: puts remote-duo-host in ~/.local/bin, adds a menu entry, and starts it.
# Every download is checked against the release's SHA256SUMS.
set -eu

REPO="iRunTech/remote-duo"
BASE="https://github.com/$REPO/releases/latest/download"

say() { printf '\033[1m%s\033[0m\n' "$*"; }
die() { printf 'Error: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "this installer needs '$1'"; }

need curl
need tar
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

fetch() {
  curl -fsSL --retry 3 -o "$TMP/$1" "$BASE/$1" || die "couldn't download $1"
}

verify() {
  fetch SHA256SUMS
  expected=$(grep " $1\$" "$TMP/SHA256SUMS" | cut -d' ' -f1)
  [ -n "$expected" ] || die "no checksum listed for $1"
  if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$TMP/$1" | cut -d' ' -f1)
  else
    actual=$(shasum -a 256 "$TMP/$1" | cut -d' ' -f1)
  fi
  [ "$actual" = "$expected" ] || die "checksum mismatch for $1; try again"
}

install_macos() {
  file=RemoteDuoHost-macOS.zip
  say "Downloading Remote Duo Host for macOS…"
  fetch "$file"
  verify "$file"
  dest=/Applications
  [ -w "$dest" ] || dest="$HOME/Applications"
  mkdir -p "$dest"
  osascript -e 'quit app "Remote Duo Host"' >/dev/null 2>&1 || true
  pkill -f "Remote Duo Host.app/Contents/MacOS/remote-duo-host" 2>/dev/null || true
  rm -rf "$dest/Remote Duo Host.app"
  ditto -x -k "$TMP/$file" "$dest"
  say "Installed to $dest/Remote Duo Host.app"
  open "$dest/Remote Duo Host.app"
  echo
  echo "Look for the book icon in the menu bar. The first time your iPhone connects,"
  echo "macOS asks you to allow Accessibility and Screen Recording for Remote Duo Host."
}

install_linux() {
  case $(uname -m) in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) die "no build for $(uname -m) yet" ;;
  esac
  file="remote-duo-host-linux-$arch.tar.gz"
  say "Downloading Remote Duo Host for Linux ($arch)…"
  fetch "$file"
  verify "$file"
  tar -xzf "$TMP/$file" -C "$TMP"
  bin="$HOME/.local/bin"
  apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
  mkdir -p "$bin" "$apps"
  pkill -x remote-duo-host 2>/dev/null || true
  install -m 755 "$TMP/remote-duo-host/remote-duo-host" "$bin/remote-duo-host"
  sed "s|^Exec=.*|Exec=$bin/remote-duo-host|" "$TMP/remote-duo-host/remote-duo-host.desktop" \
    > "$apps/remote-duo-host.desktop"
  say "Installed to $bin/remote-duo-host"

  if ! ldconfig -p 2>/dev/null | grep -q libayatana-appindicator3; then
    echo
    echo "For the tray icon, install libayatana-appindicator3:"
    echo "  Debian/Ubuntu: sudo apt install libayatana-appindicator3-1"
    echo "  Fedora:        sudo dnf install libayatana-appindicator-gtk3"
    echo "  Arch:          sudo pacman -S libayatana-appindicator"
  fi
  case ":$PATH:" in
    *":$bin:"*) ;;
    *) echo; echo "Add $bin to your PATH to run remote-duo-host from a terminal." ;;
  esac

  if [ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]; then
    nohup "$bin/remote-duo-host" >/dev/null 2>&1 &
    echo
    echo "Remote Duo Host is running in the system tray. Open it from your app menu next time."
  else
    echo
    echo "No desktop session found. Run '$bin/remote-duo-host --headless' to get a pairing PIN."
  fi
}

case $(uname -s) in
  Darwin) install_macos ;;
  Linux) install_linux ;;
  *) die "this installer is for macOS and Linux; on Windows use install.ps1" ;;
esac
