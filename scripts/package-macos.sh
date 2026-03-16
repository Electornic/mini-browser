#!/bin/zsh

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
APP_NAME="mini-browser"
APP_BUNDLE="${APP_NAME}.app"
DIST_DIR="${ROOT_DIR}/dist"
APP_DIR="${DIST_DIR}/${APP_BUNDLE}"
STAGING_DIR="${DIST_DIR}/dmg-root"
DMG_PATH="${DIST_DIR}/${APP_NAME}.dmg"
PLIST_SOURCE="${ROOT_DIR}/packaging/macos/Info.plist"
RELEASE_BINARY="${ROOT_DIR}/target/release/${APP_NAME}"

echo "Building release binary..."
cargo build --release --manifest-path "${ROOT_DIR}/Cargo.toml"

echo "Preparing app bundle..."
rm -rf "${APP_DIR}" "${STAGING_DIR}" "${DMG_PATH}"
mkdir -p "${APP_DIR}/Contents/MacOS" "${APP_DIR}/Contents/Resources" "${STAGING_DIR}"

cp "${RELEASE_BINARY}" "${APP_DIR}/Contents/MacOS/${APP_NAME}"
cp "${PLIST_SOURCE}" "${APP_DIR}/Contents/Info.plist"
chmod +x "${APP_DIR}/Contents/MacOS/${APP_NAME}"

echo "Preparing DMG staging folder..."
cp -R "${APP_DIR}" "${STAGING_DIR}/${APP_BUNDLE}"
ln -s /Applications "${STAGING_DIR}/Applications"

echo "Creating DMG..."
hdiutil create \
  -volname "${APP_NAME}" \
  -srcfolder "${STAGING_DIR}" \
  -ov \
  -format UDZO \
  "${DMG_PATH}" >/dev/null

rm -rf "${STAGING_DIR}"

echo "Created:"
echo "  App: ${APP_DIR}"
echo "  DMG: ${DMG_PATH}"
