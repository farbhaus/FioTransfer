#!/bin/bash
# Build FioTransfer.app: the egui application and the `fio` CLI in a bundle with
# build/darwin/Info.plist, the icon and the notices, ad-hoc signed so macOS
# opens it after a download.
#
#   scripts/bundle.sh                        ./FioTransfer.app for this Mac, version "dev"
#   BIN=… CLI=… APP=… VERSION=… NO_SIGN=1 scripts/bundle.sh
#                                            bundle binaries built elsewhere (package.sh)
#
# A "dev" build never checks for updates, so the copy in the project folder
# can't replace itself with a release.
set -euo pipefail
cd "$(dirname "$0")/.."

PROFILE=${PROFILE:-release}
APP=${APP:-FioTransfer.app}
VERSION=${VERSION:-dev}

if [ -z "${BIN:-}" ]; then
  export FIO_VERSION="$VERSION"
  cargo build --profile "$PROFILE" -p fiotransfer-egui --bin fiotransfer-egui
  # The CLI too: `docs/PORTAL.md` tells the reader that `fio` is inside the
  # bundle, at Contents/MacOS/fio.
  cargo build --profile "$PROFILE" -p fiotransfer --bin fio
  DIR=target/$PROFILE
  [ "$PROFILE" = dev ] && DIR=target/debug
  BIN=$DIR/fiotransfer-egui
  CLI=$DIR/fio
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/FioTransfer"
cp "$CLI" "$APP/Contents/MacOS/fio"
cp build/darwin/Info.plist "$APP/Contents/Info.plist"
# The updater compares CFBundleShortVersionString with the release it installs.
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString ${VERSION#v}" "$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion ${VERSION#v}" "$APP/Contents/Info.plist"
cp LICENSE "$APP/Contents/Resources/LICENSE.txt"
cp THIRD_PARTY_NOTICES.txt "$APP/Contents/Resources/"
cp build/icons/icon.icns "$APP/Contents/Resources/iconfile.icns"
cp build/appicon.png "$APP/Contents/Resources/appicon.png" 2>/dev/null || true
if [ -z "${NO_SIGN:-}" ]; then
  codesign --force --deep -s - "$APP" >/dev/null 2>&1
fi
echo "Built $APP ($VERSION)"
