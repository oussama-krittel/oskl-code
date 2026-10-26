#!/bin/zsh
# Builds "OSKL Code.app" and installs it to /Applications (or $1).
# Usage: packaging/build-app.sh [install-dir]
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"

APP_NAME="OSKL Code"
BIN_NAME="oskl-code"
DEST="${1:-/Applications}"
BUILD="target/app"
APP="$BUILD/$APP_NAME.app"
VERSION=$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)"/\1/')

echo "→ building release binary"
cargo build --release

echo "→ rendering icon"
rm -rf "$BUILD/icon" "$BUILD/AppIcon.iconset"
cargo run -q --release --example render_icon -- "$BUILD/icon"
cp "$BUILD/icon/wordmark.png" packaging/wordmark.png
ICONSET="$BUILD/AppIcon.iconset"
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
  cp "$BUILD/icon/icon_$s.png" "$ICONSET/icon_${s}x${s}.png"
done
cp "$BUILD/icon/icon_32.png"   "$ICONSET/icon_16x16@2x.png"
cp "$BUILD/icon/icon_64.png"   "$ICONSET/icon_32x32@2x.png"
cp "$BUILD/icon/icon_256.png"  "$ICONSET/icon_128x128@2x.png"
cp "$BUILD/icon/icon_512.png"  "$ICONSET/icon_256x256@2x.png"
cp "$BUILD/icon/icon_1024.png" "$ICONSET/icon_512x512@2x.png"

echo "→ assembling $APP_NAME.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/kite "$APP/Contents/MacOS/$BIN_NAME"
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$APP_NAME</string>
  <key>CFBundleDisplayName</key><string>$APP_NAME</string>
  <key>CFBundleIdentifier</key><string>dev.oskl.code</string>
  <key>CFBundleExecutable</key><string>$BIN_NAME</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

echo "→ signing (ad-hoc, for this Mac)"
codesign --force --deep --sign - "$APP"

echo "→ installing to $DEST"
rm -rf "$DEST/$APP_NAME.app"
cp -R "$APP" "$DEST/"
touch "$DEST/$APP_NAME.app"
echo "✓ installed: $DEST/$APP_NAME.app"
