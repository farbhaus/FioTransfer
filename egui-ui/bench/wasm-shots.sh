#!/usr/bin/env bash
# Build the egui front end for the browser, serve it, capture the four screens at
# 1180x780 with deviceScaleFactor 1, and print what came out.
#
#     egui-ui/bench/wasm-shots.sh            # release build (what the README quotes)
#     PROFILE=debug egui-ui/bench/wasm-shots.sh
#     OUT=/tmp/other-shots egui-ui/bench/wasm-shots.sh
#
# This is a measurement apparatus: it exists so the egui front end can be
# screenshotted by the same Chromium, at the same size and pixel ratio, as the
# webview front end. It is not a shipping target. See bench/README.md.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
crate="$(cd "$here/.." && pwd)"
out="${OUT:-/tmp/wasm-shots}"
chromium="${CHROMIUM:-/Applications/Chromium.app/Contents/MacOS/Chromium}"
profile="${PROFILE:-release}"

die() {
    printf 'wasm-shots: %s\n' "$*" >&2
    exit 1
}

[ -x "$chromium" ] || die "no Chromium at $chromium (set CHROMIUM=/path/to/Chromium)"
command -v python3 >/dev/null 2>&1 || die "python3 is not on PATH"
if ! command -v trunk >/dev/null 2>&1; then
    die "trunk is not on PATH; bench/README.md says which build and where it came from"
fi
if ! rustup target list --installed 2>/dev/null | grep -qx 'wasm32-unknown-unknown'; then
    die "rustup target add wasm32-unknown-unknown"
fi

cd "$crate"
mkdir -p "$out"

case "$profile" in
    release) trunk build --release ;;
    debug) trunk build ;;
    *) die "PROFILE is 'release' or 'debug', not '$profile'" ;;
esac

# capture.py serves web/dist itself on a free port, waits for the canvas to have
# drawn (data-frames) and for the PNG to have non-uniform pixels, then writes
# <out>/<view>.png and <out>/summary.json.
python3 "$here/capture.py" \
    --dist "$crate/web/dist" \
    --out "$out" \
    --chromium "$chromium" \
    --report "$out/summary.json"
