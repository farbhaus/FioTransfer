#!/bin/bash
# Build FioTransfer.app into the project folder: the egui application, in a
# bundle with the Go app's Info.plist bytes, the icon and the notices, ad-hoc
# signed so macOS opens it after a download.
set -euo pipefail
cd "$(dirname "$0")/.."

PROFILE=${PROFILE:-release}
APP=FioTransfer.app

cargo build --profile "$PROFILE" -p fiotransfer-egui --bin fiotransfer-egui
# The CLI too: `docs/PORTAL.md` tells the reader that `fio` is inside the bundle,
# at Contents/MacOS/fio. Same as the packaged bundle — `egui-ui/Cargo.toml` lists
# both binaries for cargo-packager.
cargo build --profile "$PROFILE" -p fiotransfer --bin fio

BIN=target/$PROFILE/fiotransfer-egui
CLI=target/$PROFILE/fio
[ "$PROFILE" = dev ] && BIN=target/debug/fiotransfer-egui && CLI=target/debug/fio

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/FioTransfer"
cp "$CLI" "$APP/Contents/MacOS/fio"
# The Info.plist is the Go build's own file, byte for byte (minus the Finder
# services, which went with the webview shell).
cp build/darwin/Info.plist "$APP/Contents/Info.plist"
cp LICENSE "$APP/Contents/Resources/LICENSE.txt"
cp THIRD_PARTY_NOTICES.txt "$APP/Contents/Resources/"
cp build/icons/icon.icns "$APP/Contents/Resources/iconfile.icns"
cp build/appicon.png "$APP/Contents/Resources/appicon.png" 2>/dev/null || true
codesign --force --deep -s - "$APP" >/dev/null 2>&1
echo "Built ./$APP"
