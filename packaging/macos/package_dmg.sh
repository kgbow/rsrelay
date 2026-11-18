#!/usr/bin/env bash
set -euo pipefail

# Package rsrelay desktop GUI (`rsrelay-gui`) into `rsrelay.app` and a compressed macOS `.dmg`.
#
# Usage:
#   # Quick native host-arch build & DMG (for local testing):
#   ./packaging/macos/package_dmg.sh
#
#   # Universal Binary (Apple Silicon arm64 + Intel x86_64) for release:
#   ./packaging/macos/package_dmg.sh --universal --version 0.1.0

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

UNIVERSAL=0
VERSION=""
OUT_DIR="${REPO_ROOT}/dist"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --universal)
      UNIVERSAL=1
      shift
      ;;
    --version)
      VERSION="$2"
      shift 2
      ;;
    --out-dir)
      OUT_DIR="$2"
      shift 2
      ;;
    *)
      echo "Unknown argument: $1" >&2
      exit 1
      ;;
  esac
done

cd "${REPO_ROOT}"

if [[ -z "${VERSION}" ]]; then
  VERSION="$(awk -F'"' '/^\[workspace\.package\]/{in_ws=1; next} /^\[/{in_ws=0} in_ws && /^version *=/{print $2; exit}' Cargo.toml)"
fi
VERSION="${VERSION#v}"

if [[ -z "${VERSION}" ]]; then
  echo "Failed to determine package version from Cargo.toml" >&2
  exit 1
fi

export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-13.0}"
# Force CMake static build of bundled libopus so we never link host Homebrew single-arch dylibs.
export LIBOPUS_STATIC=1
export LIBOPUS_NO_PKG=1
export CMAKE_POLICY_VERSION_MINIMUM=3.5

STAGE_ROOT="${REPO_ROOT}/target/macos-dmg-stage"
rm -rf "${STAGE_ROOT}"
mkdir -p "${STAGE_ROOT}" "${OUT_DIR}"

BIN_STAGE="${STAGE_ROOT}/rsrelay-gui"

if [[ "${UNIVERSAL}" -eq 1 ]]; then
  echo "==> Building rsrelay-gui for aarch64-apple-darwin & x86_64-apple-darwin (v${VERSION})..."
  rustup target add aarch64-apple-darwin x86_64-apple-darwin
  cargo build --release -p rsrelay-gui --target aarch64-apple-darwin
  cargo build --release -p rsrelay-gui --target x86_64-apple-darwin

  lipo -create \
    "target/aarch64-apple-darwin/release/rsrelay-gui" \
    "target/x86_64-apple-darwin/release/rsrelay-gui" \
    -output "${BIN_STAGE}"

  ARCH_TAG="universal"
else
  HOST_ARCH="$(uname -m)"
  echo "==> Building rsrelay-gui for host architecture (${HOST_ARCH}, v${VERSION})..."
  cargo build --release -p rsrelay-gui
  cp "target/release/rsrelay-gui" "${BIN_STAGE}"
  ARCH_TAG="${HOST_ARCH}"
fi

chmod 755 "${BIN_STAGE}"

echo "==> Verifying dynamic library linkage..."
OTOOL_OUT="$(otool -L "${BIN_STAGE}")"
echo "${OTOOL_OUT}"
if echo "${OTOOL_OUT}" | grep -E '/opt/homebrew|/usr/local' >/dev/null; then
  echo "ERROR: Non-system Homebrew/usr-local dylib dependency detected in rsrelay-gui!" >&2
  exit 1
fi

echo "==> Assembling rsrelay.app bundle..."
DMG_ROOT="${STAGE_ROOT}/dmg-root"
APP_DIR="${DMG_ROOT}/rsrelay.app"
mkdir -p "${APP_DIR}/Contents/MacOS" "${APP_DIR}/Contents/Resources"

cp "${BIN_STAGE}" "${APP_DIR}/Contents/MacOS/rsrelay-gui"
cp "${REPO_ROOT}/packaging/assets/AppIcon.icns" "${APP_DIR}/Contents/Resources/AppIcon.icns"

cat > "${APP_DIR}/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDisplayName</key>
    <string>rsrelay</string>
    <key>CFBundleExecutable</key>
    <string>rsrelay-gui</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundleIdentifier</key>
    <string>dev.rsrelay.app</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>rsrelay</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>rsrelay requires Microphone access to stream ultra-low-latency voice audio over LAN.</string>
    <key>NSAudioCaptureUsageDescription</key>
    <string>rsrelay requires System Audio Recording access to capture desktop audio via ScreenCaptureKit.</string>
    <key>NSScreenCaptureUsageDescription</key>
    <string>rsrelay uses ScreenCaptureKit to capture system desktop audio without virtual audio drivers.</string>
</dict>
</plist>
EOF

if [[ -n "${APPLE_SIGNING_IDENTITY:-}" ]]; then
  echo "==> Signing rsrelay.app with Developer ID: ${APPLE_SIGNING_IDENTITY}"
  codesign --force --deep --options runtime --timestamp --sign "${APPLE_SIGNING_IDENTITY}" "${APP_DIR}"
else
  echo "==> Ad-hoc signing rsrelay.app..."
  codesign --force --deep --sign - "${APP_DIR}"
fi

ln -s /Applications "${DMG_ROOT}/Applications"

DMG_NAME="rsrelay-v${VERSION}-macos-${ARCH_TAG}.dmg"
DMG_PATH="${OUT_DIR}/${DMG_NAME}"
rm -f "${DMG_PATH}"

echo "==> Creating compressed DMG: ${DMG_PATH}"
hdiutil create \
  -volname "rsrelay" \
  -srcfolder "${DMG_ROOT}" \
  -ov \
  -format UDZO \
  "${DMG_PATH}"

if [[ -n "${APPLE_SIGNING_IDENTITY:-}" ]]; then
  codesign --force --timestamp --sign "${APPLE_SIGNING_IDENTITY}" "${DMG_PATH}"
fi

if [[ -n "${APPLE_ID:-}" && -n "${APPLE_APP_SPECIFIC_PASSWORD:-}" && -n "${APPLE_TEAM_ID:-}" ]]; then
  echo "==> Submitting DMG to Apple Notary Service..."
  xcrun notarytool submit "${DMG_PATH}" \
    --apple-id "${APPLE_ID}" \
    --password "${APPLE_APP_SPECIFIC_PASSWORD}" \
    --team-id "${APPLE_TEAM_ID}" \
    --wait
  xcrun stapler staple "${DMG_PATH}"
fi

echo "==> Done: ${DMG_PATH}"
