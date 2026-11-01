#!/bin/sh
# Install or update FioTransfer from the latest GitHub release:
#
#   curl -fsSL https://raw.githubusercontent.com/farbhaus/FioTransfer/main/scripts/install.sh | sh
#
# Files downloaded with curl aren't quarantined, so Gatekeeper doesn't block
# the ad-hoc signed app the way it does after a browser download.
set -eu

REPO=farbhaus/FioTransfer
DEST=/Applications
[ -w "$DEST" ] || DEST="$HOME/Applications"
mkdir -p "$DEST"

json=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest")
asset() {
  printf '%s\n' "$json" | grep -o "\"browser_download_url\": *\"[^\"]*$1\"" | cut -d'"' -f4 | head -n 1
}
zip_url=$(asset '-macOS\.zip')
sums_url=$(asset '/SHA256SUMS\.txt')
if [ -z "$zip_url" ]; then
  echo "No macOS .zip in the latest release." >&2
  exit 1
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

zip=$(basename "$zip_url")
echo "Downloading $zip"
curl -fL --progress-bar -o "$tmp/$zip" "$zip_url"
if [ -n "$sums_url" ]; then
  curl -fsSL -o "$tmp/SHA256SUMS.txt" "$sums_url"
  (cd "$tmp" && grep " $zip\$" SHA256SUMS.txt | shasum -a 256 -c -)
fi
ditto -x -k "$tmp/$zip" "$tmp"

if pgrep -xq FioTransfer; then
  echo "Quitting FioTransfer"
  osascript -e 'quit app "FioTransfer"' >/dev/null 2>&1 || true
  i=0
  while pgrep -xq FioTransfer; do
    i=$((i + 1))
    if [ "$i" -gt 30 ]; then
      echo "FioTransfer is still running. Quit it and run this again." >&2
      exit 1
    fi
    sleep 1
  done
fi

rm -rf "$DEST/FioTransfer.app"
mv "$tmp/FioTransfer.app" "$DEST/"
echo "Installed $DEST/FioTransfer.app"
open "$DEST/FioTransfer.app"
