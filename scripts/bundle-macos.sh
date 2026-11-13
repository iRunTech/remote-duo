#!/bin/sh
# Build "Remote Duo Host.app": a menu bar app (no Dock icon) with its own
# Accessibility / Screen Recording permissions. Signed with your Apple
# Development identity so permissions survive rebuilds.
set -eu
cd "$(dirname "$0")/../host"

cargo build --release
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP="../dist/Remote Duo Host.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/remote-duo-host "$APP/Contents/MacOS/"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Remote Duo Host</string>
  <key>CFBundleDisplayName</key><string>Remote Duo Host</string>
  <key>CFBundleIdentifier</key><string>com.iruntech.remoteduo.host</string>
  <key>CFBundleExecutable</key><string>remote-duo-host</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSUIElement</key><true/>
  <key>NSLocalNetworkUsageDescription</key>
  <string>Remote Duo lets your iPhone find and control this computer over your network.</string>
  <key>NSBonjourServices</key><array><string>_remoteduo._tcp</string></array>
</dict>
</plist>
PLIST

IDENTITY=$(security find-identity -v -p codesigning | sed -n 's/.*"\(Apple Development[^"]*\)".*/\1/p' | head -1)
if [ -n "$IDENTITY" ]; then
  codesign --force --sign "$IDENTITY" "$APP"
  echo "Signed with $IDENTITY"
else
  codesign --force --sign - "$APP"
  echo "No Apple Development identity found; ad-hoc signed (permissions reset on each rebuild)"
fi
echo "Built $APP"
