#!/usr/bin/env python3
"""Build the `latest.json` feed that cargo-packager-updater 0.2.3 reads.

The updater decides what it may install from one JSON document. This script
turns a release's own assets into that document, so the feed can never name a
file the release does not carry, and can never repeat a signature made for a
different file.

The shape is not free-form; it is read back by `RemoteRelease` in
cargo-packager-updater 0.2.3 (`src/lib.rs` + `src/custom_serialization.rs`):

  * `version`   -- required, a semver; a leading `v` is trimmed by the parser.
  * `notes`     -- optional string.
  * `pub_date`  -- optional RFC 3339 string; a malformed one is a parse error.
  * `platforms` -- a map keyed `<os>-<arch>`, each value
                   `{"url": Url, "signature": String, "format": ...}`.

`format` is NOT optional. `ReleaseManifestPlatform` derives Deserialize with all
three of `url`, `signature` and `format` required, and the value must be one of
`app`, `appimage`, `nsis`, `wix` (case-insensitive). An entry without `format`
fails to parse with "missing field `format`".

The `<os>` half of the key is what `target()` in the same crate returns:
`linux`, `macos` or `windows` -- `macos`, not `darwin`, although the doc comment
in `lib.rs` still shows `darwin-x86_64` in its example. This script therefore
also writes a `darwin-<arch>` alias for every macOS entry: the runtime only asks
for its own `target()`, and the parser ignores keys it was not asked for.

Which key applies to which asset is recorded by the CI job that built the
artifact, not guessed here: every packaging job writes `updater-target-<os>.txt`
holding its key (for example `macos-aarch64`), because the architecture is a
property of the runner that produced the bytes.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import os
import re
import sys
from pathlib import Path

# The release names its assets `FioTransfer-<version>-<suffix>`, see
# .github/workflows/packager.yml. The macOS bundle exists in two shapes:
# a zip, which a "download the app" reader expects, and a .tar.gz, which is what
# cargo-packager-updater's own macOS installer can unpack (it feeds the bytes to
# a GzDecoder + tar::Archive and rejects a zip with "invalid gzip header").
# `--macos-artifact` picks which one the feed points at.
SUFFIXES: dict[str, list[str]] = {
    "macos": ["-macos-app.tar.gz", "-macos-app.zip"],
    "linux": ["-linux.AppImage"],
    "windows": ["-windows-setup.exe"],
}
FORMATS = {"macos": "app", "linux": "appimage", "windows": "nsis"}

SEMVER = re.compile(r"^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.\-+]+)?$")


def die(msg: str) -> "NoReturn":  # noqa: F821
    print(f"latest-json: error: {msg}", file=sys.stderr)
    raise SystemExit(1)


def warn(msg: str) -> None:
    print(f"::warning title=latest.json::{msg}")
    print(f"latest-json: warning: {msg}", file=sys.stderr)


def find_assets(directory: Path, version: str, preferred: dict[str, str]) -> dict[str, Path]:
    """Map platform -> the one asset of that platform, by its versioned name."""
    found: dict[str, Path] = {}
    for platform, suffixes in SUFFIXES.items():
        order = list(suffixes)
        chosen = preferred.get(platform)
        if chosen and chosen in order:
            order.remove(chosen)
            order.insert(0, chosen)
        for suffix in order:
            candidate = directory / f"FioTransfer-{version}{suffix}"
            if candidate.is_file():
                found[platform] = candidate
                break
        else:
            # A different version in the name is a mistake worth naming, not a
            # file to pick up silently: the feed would point at a stale bundle.
            loose = sorted(
                p for suffix in suffixes for p in directory.glob(f"*{suffix}") if p.is_file()
            )
            if loose:
                die(
                    f"{directory} holds {', '.join(p.name for p in loose)} but the feed is for "
                    f"{version}; expected {directory / f'FioTransfer-{version}{suffixes[0]}'}"
                )
    return found


def read_target_keys(directory: Path, extra: list[str]) -> list[str]:
    """The `<os>-<arch>` keys this release can serve, as recorded by the jobs."""
    keys: list[str] = []
    for path in sorted(directory.glob("updater-target-*.txt")):
        key = path.read_text(encoding="utf-8").strip()
        if key:
            keys.append(key)
    keys.extend(extra)
    seen: list[str] = []
    for key in keys:
        os_name, _, arch = key.partition("-")
        if os_name not in SUFFIXES or not arch:
            die(f"{key!r} is not an <os>-<arch> updater target (os is one of {', '.join(SUFFIXES)})")
        if key not in seen:
            seen.append(key)
    return seen


def rfc3339_utc(value: str | None) -> str:
    """`2026-10-30T12:00:00Z`, which `time`'s RFC 3339 parser accepts."""
    if value:
        text = value.strip()
        try:
            parsed = _dt.datetime.fromisoformat(text.replace("Z", "+00:00"))
        except ValueError:
            die(f"--pub-date {value!r} is not an ISO 8601 timestamp")
        if parsed.tzinfo is None:
            parsed = parsed.replace(tzinfo=_dt.timezone.utc)
        return parsed.astimezone(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    return _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def one_line(text: str, limit: int = 200) -> str:
    """A release body squeezed to one line, for when no body is given."""
    flat = " ".join(text.split())
    return flat if len(flat) <= limit else flat[: limit - 1].rstrip() + "…"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--dir", default="artifacts", help="directory holding the release assets")
    ap.add_argument("--version", required=True, help="release version, e.g. 1.5.0")
    ap.add_argument(
        "--repo",
        default=os.environ.get("GITHUB_REPOSITORY", "chaos-dotcom/FioTransfer"),
        help="owner/name whose release carries the assets",
    )
    ap.add_argument("--tag", default=None, help="release tag (default: v<version>)")
    ap.add_argument("--notes", default=None, help="the release body")
    ap.add_argument("--notes-file", default=None, help="a file holding the release body")
    ap.add_argument("--pub-date", default=None, help="RFC 3339 timestamp (default: now, UTC)")
    ap.add_argument("--out", default=None, help="where to write the JSON (default: <dir>/latest.json)")
    ap.add_argument(
        "--macos-artifact",
        choices=("zip", "tar.gz"),
        default="zip",
        help="which macOS asset the feed points at (default: zip, the app archive a "
        "reader downloads; tar.gz is what cargo-packager-updater's macOS installer unpacks)",
    )
    ap.add_argument(
        "--target",
        action="append",
        default=[],
        metavar="OS-ARCH",
        help="add an <os>-<arch> key (repeatable); CI records these in <dir>/updater-target-*.txt",
    )
    args = ap.parse_args(argv)

    version = args.version.strip().lstrip("v")
    if not SEMVER.match(version):
        die(f"--version {args.version!r} is not a semver (the updater parses it as one)")
    tag = args.tag or f"v{version}"
    directory = Path(args.dir)
    if not directory.is_dir():
        die(f"{directory} is not a directory")

    notes = args.notes
    if args.notes_file:
        notes = Path(args.notes_file).read_text(encoding="utf-8")
    if notes is None or not notes.strip():
        notes = one_line(f"FioTransfer {version}")

    preferred = {"macos": f"-macos-app.{args.macos_artifact}"}
    assets = find_assets(directory, version, preferred)
    if not assets:
        die(f"{directory} holds no FioTransfer-{version}-* asset")

    keys = read_target_keys(directory, args.target)
    if not keys:
        die(
            f"no updater target recorded: expected updater-target-<os>.txt in {directory} "
            "or at least one --target OS-ARCH"
        )

    platforms: dict[str, dict[str, str]] = {}
    for key in keys:
        os_name, _, arch = key.partition("-")
        asset = assets.get(os_name)
        if asset is None:
            want = " / ".join(f"FioTransfer-{version}{s}" for s in SUFFIXES[os_name])
            warn(f"{key}: the release carries no {want}; leaving it out of the feed")
            continue
        sig_path = asset.with_name(asset.name + ".sig")
        if not sig_path.is_file():
            warn(f"{key}: {asset.name} has no signature ({sig_path.name}); leaving it out of the feed")
            continue
        signature = sig_path.read_text(encoding="utf-8").strip()
        if not signature:
            warn(f"{key}: {sig_path.name} is empty; leaving it out of the feed")
            continue
        entry = {
            "signature": signature,
            "url": f"https://github.com/{args.repo}/releases/download/{tag}/{asset.name}",
            "format": FORMATS[os_name],
        }
        platforms[key] = entry
        if os_name == "macos":
            # The crate's own doc example spells macOS `darwin`; `target()`
            # spells it `macos`. Serve both spellings, pointing at the same
            # bytes: a client that hardcoded either name finds the same file.
            platforms[f"darwin-{arch}"] = dict(entry)

    if not platforms:
        die("no platform entry could be built: every asset is missing or unsigned")

    feed = {
        "version": version,
        "notes": notes if notes.endswith("\n") else notes + "\n",
        "pub_date": rfc3339_utc(args.pub_date),
        "platforms": platforms,
    }
    out = Path(args.out) if args.out else directory / "latest.json"
    out.write_text(json.dumps(feed, indent=2) + "\n", encoding="utf-8")
    print(f"wrote {out} for {tag} with {len(platforms)} platform(s): {', '.join(platforms)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
