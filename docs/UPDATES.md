# Updates: the signed feed

An update is two things a client must agree on: **which file to fetch** and **who
says that file is the one we built**. GitHub's release page answers neither
question by itself — a tag, an asset and an HTTP 200 can all be replaced by
anything that can write to the release. So every release now also carries:

* `<asset>.sig` — a minisign signature over each artifact's bytes;
* `latest.json` — one document naming the version, the release notes and, per
  platform, the artifact's URL, its signature and its format.

The feed is written for `cargo-packager-updater` 0.2.3, which is what the app's
updater is being moved onto: it reads `latest.json`, picks the entry for its own
`<os>-<arch>`, downloads that URL, and checks the bytes against the signature
**before** it installs anything (`Update::download` → `verify_signature`). A file
that fails the check is not installed, and neither is a file whose signature the
client cannot find. This document describes the release side of that contract;
the half in `core/` is a separate change, and until it lands the app still asks
the GitHub API for the newest release. Nothing here depends on that half.

## The endpoint

    https://github.com/chaos-dotcom/FioTransfer/releases/latest/download/latest.json

Nothing is hosted for this. `/releases/latest/download/<name>` is a redirect to
the `<name>` asset of the newest published release, so the URL is stable and its
content changes the moment a newer release exists. Two consequences worth
knowing:

* a **draft** release is not "latest", and neither is a pre-release — a release
  cut that way is invisible to the updater until it is published;
* the URL is a redirect to the exact tag's file (`.../download/v1.5.0/latest.json`),
  so a client that caches it will keep seeing an old version.

A pull request builds the packages but publishes nothing, and a push to main only
releases when `[package] version` in `Cargo.toml` names a tag that does not exist
yet — unchanged behaviour, see `.github/workflows/packager.yml`.

## What a release carries

| asset | what it is | in the feed |
| --- | --- | --- |
| `FioTransfer-<v>-macos-app.zip` | the `.app`, ad-hoc signed, zipped | yes, `macos-<arch>` (default) |
| `FioTransfer-<v>-macos-app.tar.gz` | the same `.app` as a gzip'd tar | `--macos-artifact tar.gz` only |
| `FioTransfer-<v>-linux.deb` | Debian package | no — the updater installs the AppImage |
| `FioTransfer-<v>-linux.AppImage` | AppImage | yes, `linux-x86_64` |
| `FioTransfer-<v>-windows-setup.exe` | NSIS installer | yes, `windows-x86_64` |
| `<asset>.sig` | minisign signature of the file next to it | its text is *inside* the feed |
| `latest.json` | the feed | — |

The `.deb` is signed like everything else so a human can check it, but no feed
entry points at it: `cargo-packager-updater` installs an AppImage on Linux.

Two macOS artifacts exist because the two readers want different things. A
person downloading the app wants a zip. `cargo-packager-updater`'s own macOS
installer does not: it feeds the downloaded bytes to a `GzDecoder` wrapped in a
`tar::Archive` and fails on a zip with `invalid gzip header`. Its tar also drops
the archive's first path component before renaming what is left over the old
bundle, so `FioTransfer-<v>-macos-app.tar.gz` holds exactly one top-level entry,
`FioTransfer.app`. The feed points at the **zip** by default; one flag in the
release job (`--macos-artifact tar.gz`) points it at the tarball instead, for a
client that uses the crate's own installer rather than its own.

## The JSON, exactly

`latest.json` is not free-form: it is read by `RemoteRelease` in
`cargo-packager-updater-0.2.3/src/lib.rs`, whose `Deserialize` lives in
`src/custom_serialization.rs`. Here is a real one, produced by
`scripts/latest-json.py` for a `1.5.0` release (signatures shortened):

```json
{
  "version": "1.5.0",
  "notes": "Unsigned builds of FioTransfer 1.5.0, from Cargo.toml.\n\nOne app, three platforms\u2026",
  "pub_date": "2026-10-30T12:04:53Z",
  "platforms": {
    "linux-x86_64": {
      "signature": "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIGNhcmdvLXBhY2thZ2Vy…",
      "url": "https://github.com/chaos-dotcom/FioTransfer/releases/download/v1.5.0/FioTransfer-1.5.0-linux.AppImage",
      "format": "appimage"
    },
    "macos-aarch64": {
      "signature": "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIGNhcmdvLXBhY2thZ2Vy…",
      "url": "https://github.com/chaos-dotcom/FioTransfer/releases/download/v1.5.0/FioTransfer-1.5.0-macos-app.zip",
      "format": "app"
    },
    "darwin-aarch64": {
      "signature": "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIGNhcmdvLXBhY2thZ2Vy…",
      "url": "https://github.com/chaos-dotcom/FioTransfer/releases/download/v1.5.0/FioTransfer-1.5.0-macos-app.zip",
      "format": "app"
    },
    "windows-x86_64": {
      "signature": "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIGNhcmdvLXBhY2thZ2Vy…",
      "url": "https://github.com/chaos-dotcom/FioTransfer/releases/download/v1.5.0/FioTransfer-1.5.0-windows-setup.exe",
      "format": "nsis"
    }
  }
}
```

The rules that are easy to get wrong, all of them from the crate's own code:

* **`format` is required.** `ReleaseManifestPlatform` derives `Deserialize` with
  `url`, `signature` and `format` all required, and the value must be one of
  `app`, `appimage`, `nsis`, `wix`. Leave it out and the whole document is
  rejected with ``missing field `format` `` — not just that one entry.
* **`url` and `signature` live under `platforms.<os>-<arch>`.** A top-level
  `url`/`signature`/`format` is the single-platform ("dynamic") shape, which the
  parser accepts only when `platforms` is absent. Do not mix the two.
* **`<os>` is `macos`, not `darwin`.** `target()` in the same crate returns
  `linux`, `macos` or `windows` and joins it with the arch as `<os>-<arch>`.
  The crate's own doc comment still shows `darwin-x86_64` in its example, so the
  feed writes a `darwin-<arch>` alias next to every `macos-<arch>` entry, with
  the same URL and signature. The runtime only asks for its own `target()`, so
  the alias is never consulted by a correct client and costs nothing.
* **`version`** is parsed as a semver; a leading `v` is trimmed.
* **`pub_date`** must be RFC 3339 (`2026-10-30T12:04:53Z`). Anything else fails
  with ``invalid value for `pub_date` ``.
* **`signature`** is the *text of the `.sig` file*, verbatim: base64 of a minisign
  signature box. The client base64-decodes it again.
* **`notes`** is free text. It is the release body, so the release page and the
  update dialog say the same thing.

`darwin-x86_64` is **not** written: no Intel macOS artifact is built, so there is
no entry for it. An Intel Mac (or a Rosetta-only one) finds no macOS entry at all
and the updater reports `TargetNotFound`.

## The keys

The signature is minisign (ed25519 over a BLAKE2b prehash), made by
`cargo packager signer`. One key pair, generated once:

```sh
# never inside a repository
mkdir -p ~/.config/fiotransfer
cargo packager signer generate --path ~/.config/fiotransfer/updater.key --ci
```

That writes two files and refuses to overwrite an existing one without
`--force`:

| file | contents | what to do with it |
| --- | --- | --- |
| `updater.key` | base64 of the minisign **secret** key box, 348 bytes, no trailing newline | GitHub secret `CARGO_PACKAGER_SIGN_PRIVATE_KEY`. Never committed, never printed. |
| `updater.key.pub` | base64 of the minisign **public** key box, 152 bytes | compiled into the client |

The current public key (this is safe to publish; it only *verifies*):

```
dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDZGRTZEQ0E0Q0Q1NjM5MjUKUldRbE9WYk5wTnptYjhlZ1pEck52Y3VvaHY1M0drUE1tUDhXT2dyUHdyUWNNbDBXdFZCY3N4bmoK
```

and what is inside it, since a minisign key is itself base64 text:

```
untrusted comment: minisign public key: 6FE6DCA4CD563925
RWQlOVbNpNzmb8egZDrNvcuohv53GkPMmP8WOgrPwrQcMl0WtVBcsxnj
```

`6FE6DCA4CD563925` is the key id: it is printed next to every signature, and a
signature made by a different key fails verification even when both keys belong
to this project.

### Storing the private key

```sh
gh secret set CARGO_PACKAGER_SIGN_PRIVATE_KEY < ~/.config/fiotransfer/updater.key
```

`gh secret set` from standard input is the safe form: the value never appears in
a shell history, and the file has no trailing newline to strip. **A trailing
newline breaks signing.** `echo` and `printf` add one, and base64 then rejects it:

```
ERROR Invalid symbol 10, offset 348.
```

If the key was generated with a password, put it in
`CARGO_PACKAGER_SIGN_PRIVATE_KEY_PASSWORD` as well; the key above was generated
with `--ci` and no password, so that secret can stay unset.

The variable names are not ours — they are cargo-packager's own
(`--private-key`'s `env =` in `src/cli/signer/sign.rs`). That is why the key is
never written to a file on the runner and never appears on a command line.

## What CI does

Per platform job, after the artifacts are collected into `out/`:

1. **Record the target.** Each job writes `out/updater-target-<os>.txt` holding
   its own key (`macos-aarch64`, `linux-x86_64`, `windows-x86_64`), derived from
   `uname -m` on the machine that built the bytes. The architecture is a property
   of the runner, so the feed does not guess it; a switch from `macos-latest` to
   `macos-13` moves the key from `macos-aarch64` to `macos-x86_64` by itself.
2. **Sign every artifact** with `cargo packager signer sign`, one `.sig` each,
   and print a SHA-256 of each file so the log records what was signed.
   Without the secret the step prints
   `::warning title=Updater signatures skipped::…` and exits 0: a fork PR has no
   secrets and must still build.
3. **Upload** `out/*` — the artifacts, their `.sig` files and the target record.

Then the `release` job:

1. writes the release notes to `release-notes.md`, so the release page and the
   feed's `notes` are the same text;
2. runs `scripts/latest-json.py`, which builds the feed **from the assets that
   are actually present** — it cannot name a file the release does not carry, and
   it cannot repeat a signature made for a different file;
3. deletes the `updater-target-*.txt` records, which are build notes and not
   assets;
4. creates the release with `gh release create v<version> artifacts/*`, so the
   feed and the signatures are published with the artifacts.

If no `.sig` exists, the release still happens but carries no `latest.json`, and
the step says so with a warning and in the run summary. A feed nobody can verify
is not published: an entry without a signature only makes a client download the
whole artifact and then fail.

## Rotating a key

A version of the client can only verify signatures made with the key its binary
carries. **Rotating the key strands every installed application**: an old app
sees the new release, downloads it, and rejects it, because the release is signed
with a key the app has never heard of. It also cannot install an update to learn
the new key, so the only way forward for those users is to download the new build
by hand once.

Do it in this order:

1. `cargo packager signer generate --path ~/.config/fiotransfer/updater-2026.new --ci`
   — a new path, so the current key keeps working until the switch.
2. Ship a release **signed with the current key** whose client carries **both**
   public keys (the old one to verify that very release, the new one to verify
   the next). That is a client change; until it exists, any rotation strands
   everyone.
3. Put the new private key in the secret:
   `gh secret set CARGO_PACKAGER_SIGN_PRIVATE_KEY < ~/.config/fiotransfer/updater-2026.new`
   (and update `CARGO_PACKAGER_SIGN_PRIVATE_KEY_PASSWORD` if the new key has one).
4. Cut the next release normally. It is signed with the new key and only apps
   that took step 2 can install it.
5. Keep the old key until no supported version still carries only the old public
   key. Deleting it early only makes the past unverifiable; it cannot rescue an
   app that is stranded.

Rotating a key is a source change to the client (the embedded public key), which
means a version bump, a release, and the same gradual rollout as any other
change. There is no way to rotate a signing key faster than you can ship code.

## Checking a feed by hand

```sh
curl -sSL https://github.com/chaos-dotcom/FioTransfer/releases/latest/download/latest.json \
  | python3 -m json.tool
```

Then read it, rather than trusting it:

* `version` is the version you meant to ship;
* every `platforms` entry has `url`, `signature` and `format`;
* each `url` names an asset that exists on that release — the last path segment
  is the file name (the release page lists them);
* `darwin-x86_64` is absent, as it should be while no Intel macOS build exists.

To check a signature, download the file and its `.sig`, and note that **a
signature cannot be compared, only verified**: minisign 0.7.9 signs with 64 bytes
of fresh randomness, so signing the same file twice gives two different `.sig`
files that both verify. The `untrusted comment:`/`trusted comment:` lines even
carry a timestamp. Anything that "checks" a `.sig` by diffing it against a fresh
one is wrong.

`cargo-packager` 0.11.8 has **no** `signer verify` subcommand — `signer` offers
`sign` and `generate` only. Two ways to verify instead:

```sh
# 1. the key, unwrapped once: the published blob is base64 of a minisign .pub
printf '%s' '<the base64 above>' | base64 -d > /tmp/updater.pub

# 2. a downloaded signature, unwrapped the same way
curl -sSLO https://github.com/chaos-dotcom/FioTransfer/releases/download/v1.5.0/FioTransfer-1.5.0-macos-app.zip
curl -sSLO https://github.com/chaos-dotcom/FioTransfer/releases/download/v1.5.0/FioTransfer-1.5.0-macos-app.zip.sig
base64 -d < FioTransfer-1.5.0-macos-app.zip.sig > /tmp/fio.minisig

# 3. verify (brew install minisign)
minisign -Vm FioTransfer-1.5.0-macos-app.zip -x /tmp/fio.minisig -p /tmp/updater.pub
```

`minisign` answers with its exit status: it reports the signature and the comment
signature as verified, and echoes the trusted comment (`timestamp:… file:…`).

The other way is the crate itself, which is the authority a release actually has
to satisfy: `serde_json::from_str::<cargo_packager_updater::RemoteRelease>(text)`
in a scratch crate, then `download_url(target)`, `signature(target)` and
`format(target)` for `cargo_packager_updater::target()`, and `PublicKey::verify`
over the file. That is a compile-and-run check, not a one-liner, which is why
`scripts/latest-json.py` is written to make documents that parser accepts rather
than to check them afterwards.

On a maintainer's machine the whole feed can be rebuilt from a downloaded
release, which is a cheap way to see whether the workflow is still producing what
the script expects:

```sh
gh release download v1.5.0 --repo chaos-dotcom/FioTransfer --dir /tmp/rel
python3 scripts/latest-json.py --dir /tmp/rel --version 1.5.0 \
  --target macos-aarch64 --target linux-x86_64 --target windows-x86_64 \
  --out /tmp/latest.json
```

Only `notes` and `pub_date` differ from the published file, unless something else
changed.

## What is not verified

* **The macOS bundle is ad-hoc signed, not signed with a certificate.** It is not
  notarised. Gatekeeper will still ask the user to confirm the first launch of a
  downloaded build. The minisign signature says "this is the file the project
  built"; it says nothing to Gatekeeper.
* **The Windows installer has no Authenticode signature**, so SmartScreen may
  warn. Its minisign signature is what the updater checks.
* **The first download is the weak link only in the sense that it is a download.**
  A user who installs by hand has no updater to check the file for them; they
  have to verify the `.sig` themselves (above), or trust TLS.
* **Nothing verifies the `.deb`.** It ships a `.sig` for a human who wants it;
  no client reads it.
