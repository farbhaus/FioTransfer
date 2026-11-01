#!/bin/bash
# Build a universal FioTransfer.app and package it in dist/ as
# FioTransfer-<version>.dmg, FioTransfer-<version>-macOS.zip and SHA256SUMS.txt.
# The same script CI uses.
#
#   scripts/package.sh [version]
#
# The zip keeps the Go releases' name: the Go app's updater (v1.3.0 and older)
# and scripts/install.sh look for "-macOS.zip" and its line in SHA256SUMS.txt.
#
# Optional signing/notarization (all must be set to notarize):
#   MACOS_SIGN_IDENTITY   "Developer ID Application: Name (TEAMID)"; ad-hoc signed if unset
#   APPLE_ID, APPLE_TEAM_ID, APPLE_APP_PASSWORD   for notarytool
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="${1:-dev}"
NAME="FioTransfer-${VERSION}"
APP="build/bin/FioTransfer.app"
TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)

export FIO_VERSION="$VERSION"
export MACOSX_DEPLOYMENT_TARGET=13.0
for t in "${TARGETS[@]}"; do
  cargo build --release --locked --target "$t" -p fiotransfer-egui --bin fiotransfer-egui
  cargo build --release --locked --target "$t" -p fiotransfer --bin fio
done
OUT=target/universal-apple-darwin/release
mkdir -p "$OUT"
for b in fiotransfer-egui fio; do
  lipo -create -output "$OUT/$b" "target/${TARGETS[0]}/release/$b" "target/${TARGETS[1]}/release/$b"
done

# The bundle ships THIRD_PARTY_NOTICES.txt; it has to list what this build
# compiled in.
if ! python3 scripts/notices.py --check; then
  echo "::warning::THIRD_PARTY_NOTICES.txt is out of date; run scripts/notices.py > THIRD_PARTY_NOTICES.txt"
fi

mkdir -p build/bin
BIN="$OUT/fiotransfer-egui" CLI="$OUT/fio" APP="$APP" VERSION="$VERSION" NO_SIGN=1 scripts/bundle.sh

NOTARIZE=false
if [[ -n "${MACOS_SIGN_IDENTITY:-}" && -n "${APPLE_ID:-}" && -n "${APPLE_TEAM_ID:-}" && -n "${APPLE_APP_PASSWORD:-}" ]]; then
  NOTARIZE=true
fi

# Submit to Apple's notary service; print the log and fail unless Accepted.
notarize() {
  local out id status
  out="$(xcrun notarytool submit "$1" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
    --password "$APPLE_APP_PASSWORD" --wait --output-format json)"
  id="$(plutil -extract id raw -o - - <<<"$out")"
  status="$(plutil -extract status raw -o - - <<<"$out")"
  echo "notarization $id: $status"
  if [[ "$status" != "Accepted" ]]; then
    xcrun notarytool log "$id" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD"
    exit 1
  fi
}

if [[ -n "${MACOS_SIGN_IDENTITY:-}" ]]; then
  codesign --force --deep --options runtime --timestamp --sign "$MACOS_SIGN_IDENTITY" "$APP"
else
  codesign --force --deep --sign - "$APP"
fi
codesign --verify --deep --strict "$APP"

# Notarize and staple the app itself first, so the copy in the .dmg and the
# .zip both carry the ticket and open without an online check.
if $NOTARIZE; then
  rm -rf build/notarize && mkdir -p build/notarize
  ditto -c -k --keepParent "$APP" build/notarize/FioTransfer.zip
  notarize build/notarize/FioTransfer.zip
  xcrun stapler staple "$APP"
  rm -rf build/notarize
fi

rm -rf dist && mkdir -p dist/dmg
ditto "$APP" "dist/dmg/FioTransfer.app"
ln -s /Applications dist/dmg/Applications
hdiutil create -quiet -volname "FioTransfer" -srcfolder dist/dmg -ov -format UDZO "dist/${NAME}.dmg"
rm -rf dist/dmg
ditto -c -k --keepParent "$APP" "dist/${NAME}-macOS.zip"

if $NOTARIZE; then
  codesign --force --sign "$MACOS_SIGN_IDENTITY" --timestamp "dist/${NAME}.dmg"
  notarize "dist/${NAME}.dmg"
  xcrun stapler staple "dist/${NAME}.dmg"
  spctl -a -vv -t exec "$APP"
  spctl -a -vv -t open --context context:primary-signature "dist/${NAME}.dmg"
fi

(cd dist && shasum -a 256 * > SHA256SUMS.txt)
ls -lh dist
