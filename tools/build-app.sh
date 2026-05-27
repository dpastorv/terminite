#!/usr/bin/env bash
# Build a macOS .app bundle out of terminite.
#
# Produces dist/Terminite.app — a self-contained bundle the user can
# drag into /Applications, launch from Spotlight / Launchpad / Dock,
# pin, and otherwise treat as a real Mac application.
#
# Two outputs the user cares about:
#   1. dist/Terminite.app                 — the GUI bundle
#   2. dist/Terminite.app/Contents/MacOS/
#      terminite                          — the same binary, usable
#                                           as the CLI (terminite tabs,
#                                           terminite shell-init, …)
#
# To get the CLI on PATH after install:
#   ln -sfn /Applications/Terminite.app/Contents/MacOS/terminite \
#           /usr/local/bin/terminite
#
# Skipped on purpose (would be a separate bundle):
#   - codesign / notarization — requires an Apple Developer account
#   - DMG / pkg packaging
#   - Auto-update plumbing

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

NAME="Terminite"
BUNDLE_ID="dev.danielpastor.terminite"
EXEC="terminite"
VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
MIN_OS="11.0"

DIST="dist"
APP="$DIST/$NAME.app"
CONTENTS="$APP/Contents"
MACOS_DIR="$CONTENTS/MacOS"
RES_DIR="$CONTENTS/Resources"

echo "→ cargo build --release"
cargo build --release

echo "→ assembling $APP"
rm -rf "$APP"
mkdir -p "$MACOS_DIR" "$RES_DIR"

echo "→ copying binary"
cp "target/release/$EXEC" "$MACOS_DIR/$EXEC"
chmod +x "$MACOS_DIR/$EXEC"

echo "→ generating .icns from logo/terminite-icon.png"
ICONSET="$DIST/terminite.iconset"
rm -rf "$ICONSET"
mkdir -p "$ICONSET"
SRC_ICON="logo/terminite-icon.png"
# Apple's standard iconset shape — every entry @1x and @2x. iconutil
# refuses an iconset missing a required size, so emit them all even
# though the source is the same square PNG.
for SIZE in 16 32 64 128 256 512; do
    sips -z "$SIZE" "$SIZE" "$SRC_ICON" --out "$ICONSET/icon_${SIZE}x${SIZE}.png" > /dev/null
    DOUBLE=$((SIZE * 2))
    sips -z "$DOUBLE" "$DOUBLE" "$SRC_ICON" --out "$ICONSET/icon_${SIZE}x${SIZE}@2x.png" > /dev/null
done
sips -z 1024 1024 "$SRC_ICON" --out "$ICONSET/icon_512x512@2x.png" > /dev/null
iconutil -c icns "$ICONSET" -o "$RES_DIR/AppIcon.icns"
rm -rf "$ICONSET"

echo "→ writing Info.plist"
cat > "$CONTENTS/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>          <string>en</string>
    <key>CFBundleDisplayName</key>                <string>$NAME</string>
    <key>CFBundleExecutable</key>                 <string>$EXEC</string>
    <key>CFBundleIconFile</key>                   <string>AppIcon</string>
    <key>CFBundleIdentifier</key>                 <string>$BUNDLE_ID</string>
    <key>CFBundleInfoDictionaryVersion</key>      <string>6.0</string>
    <key>CFBundleName</key>                       <string>$NAME</string>
    <key>CFBundlePackageType</key>                <string>APPL</string>
    <key>CFBundleShortVersionString</key>         <string>$VERSION</string>
    <key>CFBundleVersion</key>                    <string>$VERSION</string>
    <key>LSMinimumSystemVersion</key>             <string>$MIN_OS</string>
    <key>NSHighResolutionCapable</key>            <true/>
    <key>NSPrincipalClass</key>                   <string>NSApplication</string>
    <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
    <key>LSApplicationCategoryType</key>          <string>public.app-category.developer-tools</string>
</dict>
</plist>
PLIST

echo ""
echo "✓ built $APP"
echo ""
echo "  drag it to /Applications (or ~/Applications)."
echo "  launch from Spotlight / Launchpad / Dock."
echo ""
echo "  to get the CLI on PATH:"
echo "    ln -sfn /Applications/$NAME.app/Contents/MacOS/$EXEC /usr/local/bin/$EXEC"
echo ""
