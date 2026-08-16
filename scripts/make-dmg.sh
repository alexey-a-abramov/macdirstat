#!/usr/bin/env bash
#
# Build a distributable MacDirStat disk image from the .app bundle.
#
#   ./scripts/make-dmg.sh           # -> target/release/bundle/MacDirStat-v<version>-macos.dmg
#   ./scripts/make-dmg.sh v0.5.1    # -> target/release/bundle/MacDirStat-v0.5.1-macos.dmg
#
# The image contains MacDirStat.app next to a symlink to /Applications, so
# opening it gives the familiar drag-to-install window.
#
# Always rebuilds the .app first via bundle-mac.sh, so the image can never ship a
# stale bundle. Requires only built-in macOS tools (hdiutil).
set -euo pipefail

cd "$(dirname "$0")/.."

APP_NAME="MacDirStat"
VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
# Release workflow passes the git tag (e.g. v0.5.1) so the asset name matches it.
LABEL="${1:-v${VERSION}}"

APP="target/release/bundle/${APP_NAME}.app"
DMG="target/release/bundle/${APP_NAME}-${LABEL}-macos.dmg"

# Always rebuild the .app rather than reusing whatever is on disk: a stale bundle
# from an earlier version would otherwise be shipped inside a correctly-named DMG.
# `cargo build --release` is a no-op when nothing changed, so this stays cheap
# even though the release workflow has already run bundle-mac.sh.
echo "==> Building ${APP_NAME}.app…"
./scripts/bundle-mac.sh >/dev/null

echo "==> Staging disk image contents…"
STAGING="$(mktemp -d)"
trap 'rm -rf "${STAGING}"' EXIT
# -R preserves the bundle's symlinks and the ad-hoc signature.
cp -R "${APP}" "${STAGING}/"
ln -s /Applications "${STAGING}/Applications"

echo "==> Creating ${DMG}…"
rm -f "${DMG}"
hdiutil create \
    -volname "${APP_NAME}" \
    -srcfolder "${STAGING}" \
    -fs HFS+ \
    -format UDZO \
    -imagekey zlib-level=9 \
    -quiet \
    "${DMG}"

echo "==> Verifying…"
hdiutil verify -quiet "${DMG}"

echo ""
echo "Built ${DMG} ($(du -h "${DMG}" | cut -f1))"
echo "Open it and drag ${APP_NAME} onto Applications."
echo "First launch: ad-hoc signed, not notarized — macOS blocks it once. Allow it via"
echo "  System Settings → Privacy & Security → Open Anyway, or clear the quarantine:"
echo "  xattr -dr com.apple.quarantine /Applications/${APP_NAME}.app"
