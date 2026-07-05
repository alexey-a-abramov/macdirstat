#!/usr/bin/env bash
#
# Build an installable MacDirStat.app bundle from the release binary.
#
#   ./scripts/bundle-mac.sh            # build only, drag the .app in yourself
#   ./scripts/bundle-mac.sh --install  # build AND copy it into /Applications, then launch it
#
# Produces target/release/bundle/MacDirStat.app.
# Requires the standard macOS tools: sips, iconutil, codesign (all built in).
set -euo pipefail

cd "$(dirname "$0")/.."

INSTALL=0
if [[ "${1:-}" == "--install" ]]; then
    INSTALL=1
fi

APP_NAME="MacDirStat"
BIN_NAME="macdirstat"
BUNDLE_ID="com.macdirstat.MacDirStat"
ICON_SRC="launcher_icon.png"
VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"

APP="target/release/bundle/${APP_NAME}.app"
CONTENTS="${APP}/Contents"

echo "==> Building release binary (LTO, this can take ~30s)…"
cargo build --release

echo "==> Assembling ${APP} (v${VERSION})…"
rm -rf "${APP}"
mkdir -p "${CONTENTS}/MacOS" "${CONTENTS}/Resources"

# Keep symbols so the panic hook's backtraces stay useful — don't strip.
cp "target/release/${BIN_NAME}" "${CONTENTS}/MacOS/${BIN_NAME}"

echo "==> Generating icon from ${ICON_SRC}…"
ICONSET="$(mktemp -d)/${APP_NAME}.iconset"
mkdir -p "${ICONSET}"
gen() { sips -z "$1" "$1" "${ICON_SRC}" --out "${ICONSET}/$2" >/dev/null; }
gen 16   icon_16x16.png
gen 32   icon_16x16@2x.png
gen 32   icon_32x32.png
gen 64   icon_32x32@2x.png
gen 128  icon_128x128.png
gen 256  icon_128x128@2x.png
gen 256  icon_256x256.png
gen 512  icon_256x256@2x.png
gen 512  icon_512x512.png
gen 1024 icon_512x512@2x.png
iconutil -c icns "${ICONSET}" -o "${CONTENTS}/Resources/${APP_NAME}.icns"

echo "==> Writing Info.plist…"
cat > "${CONTENTS}/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>            <string>${APP_NAME}</string>
    <key>CFBundleDisplayName</key>     <string>${APP_NAME}</string>
    <key>CFBundleIdentifier</key>      <string>${BUNDLE_ID}</string>
    <key>CFBundleExecutable</key>      <string>${BIN_NAME}</string>
    <key>CFBundleIconFile</key>        <string>${APP_NAME}</string>
    <key>CFBundlePackageType</key>     <string>APPL</string>
    <key>CFBundleShortVersionString</key> <string>${VERSION}</string>
    <key>CFBundleVersion</key>         <string>${VERSION}</string>
    <key>LSMinimumSystemVersion</key>  <string>11.0</string>
    <key>LSApplicationCategoryType</key> <string>public.app-category.utilities</string>
    <key>NSHighResolutionCapable</key> <true/>
    <key>NSHumanReadableCopyright</key> <string>© 2026 — GPL-3.0</string>
</dict>
</plist>
PLIST

echo "==> Ad-hoc code-signing…"
codesign --force --sign - "${APP}"

echo ""
echo "Built ${APP}"

if [[ "${INSTALL}" -eq 1 ]]; then
    echo "==> Installing to /Applications/${APP_NAME}.app…"
    rm -rf "/Applications/${APP_NAME}.app"
    cp -R "${APP}" /Applications/
    echo "==> Launching…"
    open "/Applications/${APP_NAME}.app"
    echo "First launch: if Gatekeeper blocks it, right-click the app in /Applications → Open (ad-hoc signed)."
else
    echo "Install with:  cp -R \"${APP}\" /Applications/"
    echo "First launch:  right-click → Open (ad-hoc signed, so Gatekeeper asks once)."
fi
echo "Protected folders: System Settings → Privacy & Security → Full Disk Access → add ${APP_NAME}."
