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
# The CLI is wired up automatically: this script installs the bundle, puts
# its own binary dir on PATH (~/.zshrc), and removes the stale
# ~/.cargo/bin/terminite a past `cargo install` leaves behind — that copy
# sits earlier on PATH and silently shadows the app with an old build.
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

echo "→ cargo build --release --features voice"
# `voice` = terminite's own local LLM (llama.cpp behind a cargo feature).
# The shipped app can always speak; plain `cargo build` skips the C++ build.
cargo build --release --features voice

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
    <!-- A CLI running inside terminite (e.g. Claude Code voice dictation) that
         records audio has its mic request attributed to terminite by macOS.
         Without this usage string the request is denied and terminite never
         appears in System Settings > Privacy > Microphone. -->
    <key>NSMicrophoneUsageDescription</key>       <string>A program running in terminite (such as Claude Code voice dictation) is requesting microphone access.</string>
</dict>
</plist>
PLIST

echo "→ ad-hoc signing the bundle"
# Rust's linker ad-hoc-signs the inner binary but leaves the bundle's
# Info.plist + resources unbound — which reads as "damaged" once the app is
# transferred (AirDrop/LocalSend) and quarantined on another Mac. A real
# ad-hoc sign of the whole bundle binds them so it validates. It's still not
# notarized (needs a paid Developer ID), so the receiving Mac must clear
# quarantine once: xattr -dr com.apple.quarantine /Applications/Terminite.app
codesign --force --deep --sign - "$APP"
echo "✓ built $APP"

# ── install into the Applications folder ────────────────────────────
# /Applications if writable (admin users usually can), else ~/Applications.
DEST="/Applications"
if ! { mkdir -p "$DEST" 2>/dev/null && [ -w "$DEST" ]; }; then
    DEST="$HOME/Applications"
    mkdir -p "$DEST"
fi
INSTALLED="$DEST/$NAME.app"
BIN_DIR="$INSTALLED/Contents/MacOS"
echo "→ installing to $INSTALLED"
rm -rf "$INSTALLED"
cp -R "$APP" "$DEST/"
# Let Spotlight / Launchpad find it.
LSREG="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
[ -x "$LSREG" ] && "$LSREG" -f "$INSTALLED" 2>/dev/null || true

# ── CLI on PATH — one source of truth (the bundle) ──────────────────
# The binary is also the CLI (terminite tabs / blocks / --version / …).
# Instead of a sudo symlink or a `cargo install` copy — both drift and can
# shadow the app — put the bundle's own bin dir on PATH and clear the stale
# duplicate. That ~/.cargo/bin copy sits earlier on PATH and silently runs
# an OLD build; it's regenerable, so removing it is safe.
if [ -e "$HOME/.cargo/bin/$EXEC" ]; then
    rm -f "$HOME/.cargo/bin/$EXEC"
    echo "→ removed stale $HOME/.cargo/bin/$EXEC (was shadowing the app)"
fi

# Ensure the bundle bin dir is on PATH (idempotent). zsh is the macOS
# default; bash users: add the same line to ~/.bash_profile.
ZRC="$HOME/.zshrc"
if [ -f "$ZRC" ] && grep -qF "$BIN_DIR" "$ZRC"; then
    echo "→ PATH entry already present in $ZRC"
else
    printf '\n# terminite CLI — bundle binary (GUI app lives in %s)\nexport PATH="$PATH:%s"\n' \
        "$DEST" "$BIN_DIR" >> "$ZRC"
    echo "→ added terminite to PATH in $ZRC"
fi

# Warn about any *other* terminite in common PATH dirs that could shadow it.
for d in /usr/local/bin /opt/homebrew/bin "$HOME/.local/bin" /usr/bin; do
    if [ -e "$d/$EXEC" ] && [ "$d/$EXEC" != "$BIN_DIR/$EXEC" ]; then
        echo "⚠ another '$EXEC' at $d/$EXEC may shadow the app on PATH — remove it."
    fi
done

echo ""
echo "✓ installed $INSTALLED"
"$BIN_DIR/$EXEC" --version 2>/dev/null | sed 's/^/    /' || true
echo ""
echo "  GUI: launch from Spotlight / Launchpad / Dock."
echo "  CLI: open a new shell, then \`$EXEC --version\`."
echo ""
