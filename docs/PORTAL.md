# Portal

Sync a Portal upload link to FioTransfer, and FioTransfer gives it a project folder on your disk with up to
two folders in it, one per lane switched on in Portal. **Imports**: what the client uploads comes down.
**Exports**: what you put there goes back up. Portal only says which links and which Frame.io folders; the
files move between FioTransfer and Frame.io.

```
~/Downloads/
  ACME_SPOT/          ← the link's project folder (Portal's project name)
    1_DRIVE/          ← Imports: Frame.io → disk
    0_EXPORTS/        ← Exports: disk → Frame.io
```

Both names come from Portal: its **Settings → FioTransfer** sets the defaults, and a lane follows its
Frame.io folder's name unless you type one on the link. FioTransfer uses them as sent.

## Rules

- **Only synced links.** A link without a sync is left alone.
- **A lane switched off is off.** No Imports, nothing comes down; no Exports, nothing goes up.
- **Files already there** when a folder starts syncing (the client's earlier uploads, files already in the
  local folder) are left alone, unless **Include files already there** is on, as for any watch folder.
  Imports skips a file already on disk with the same name and size.
- **Deletions don't sync.** Deleting a file on one side never deletes it on the other. Each folder moves
  files one way only.
- **A re-render replaces its earlier copy.** When a file in Exports changes, the new version uploads and the
  copy FioTransfer uploaded before is deleted from Frame.io once the new one is confirmed. A same-named file
  put there some other way stays.
- **When a link closes, or a lane is switched off**, its folder stops syncing and stays on disk with
  everything in it.
- **Renaming** a project or a lane starts a new folder; the old one stays.
- **Links can share a project folder**, each with its own folders inside. Links that send the same Exports
  folder to the same Frame.io folder share one watch.
- **Edit links in Portal.** Portal's folders are read-only in Watch Folders. Pausing one is fine.

## Setting it up

Settings → **Portal**:

| Field | |
| --- | --- |
| **Server URL** | Your Portal, e.g. `https://portal.example.com`. |
| **Sign in** | Your Portal admin email and password (and the six-digit code if the account has one). Only Portal's session cookie is kept, in the Keychain; the password is not stored. |
| **Access token** | Instead of signing in: the deployment's `PORTAL_API_TOKEN`, for a machine without a person at it. A stored token wins over a session. |
| **Local folder** | Where project folders go. Default `~/Downloads`. |
| **Include files already there** | A project folder that starts syncing also takes what's already there. Off by default. Folders already syncing aren't affected. |
| **Lane labels** | What Watch Folders calls each project's two lanes. Default `Inbound` and `Outbound`. Only the words on screen; no folder moves. |
| **Enabled** | Checks Portal every 30 s. Off by default: nothing is contacted until you turn it on. |

**Reconcile now** checks at once. The status line says what the last check did, or why it failed.

## What Portal sends

FioTransfer reads `GET /api/upload-links`. A synced link carries a `turbosort` and a `sync` object inside its
`folder`:

```json
{
  "id": "…", "name": "Acme Spot 30s", "state": "ok", "turbosort": "ACME_SPOT",
  "folder": {
    "account_id": "…", "folder_id": "…", "turbosort": "ACME_SPOT",
    "sync": {
      "mode": "add_only",
      "drive":   {"local": "1_DRIVE"},
      "exports": {"local": "0_EXPORTS", "folder_id": "…"}
    }
  }
}
```

| Field | Meaning | Absent |
| --- | --- | --- |
| `turbosort` | The project folder, `~/Downloads/<turbosort>` | (always sent with `sync`) |
| `sync.mode` | `add_only`: nothing is deleted at either end | `add_only` |
| `sync.drive` | Imports: Frame.io → `<project>/<local>` | Imports is off |
| `sync.drive.folder_id` | Where Imports come from | the link's `folder_id` |
| `sync.exports` | Exports: `<project>/<local>` → Frame.io | Exports is off |
| `sync.exports.folder_id` | Where Exports go | (always sent with `exports`) |
| `*.local` | The lane's folder name, used as sent | (always sent) |
| `state` | `ok` while the link works; `expired` or `revoked` stop its folders | live |

Folder names are one path segment: no `/`, `\`, `%`, control characters, `.` or `..`; a bad one is refused
with a reason. Names that differ only in case are one folder. Two links naming one folder for different
Frame.io folders: the lower id gets it, the other is skipped with a reason.

Rows saved by Portal 2.2 or 2.3 (`download`, `upload`, `sync.name`, or `"sync": {}`) are read the way
Portal's migration rewrites them: they always have Imports, `sync.name` or the link's name is the project
folder, and an unnamed lane is `IN` or `OUT`. A link with a `turbosort` and no `sync`
is one two-way folder, `~/Downloads/<turbosort>`; it needs that folder to itself.

## What FioTransfer tells Portal

After every check, and 5 s after launch, FioTransfer posts one heartbeat so Portal can show which machines
are connected and which folders each one keeps in step. It's the only thing the app writes to Portal, and
it changes nothing on either side:

```json
POST /api/live-work/heartbeat
{
  "client_id": "0f0c1c1e-…", "name": "studio-mac", "app_version": "2.1.0", "platform": "macos-aarch64",
  "lanes": [{"link_id": "L1", "project": "ACME_SPOT", "local": "1_DRIVE", "direction": "download",
             "folder_id": "…", "path": "/Users/you/Downloads/ACME_SPOT/1_DRIVE"}]
}
```

- One lane per folder FioTransfer actually made for Portal (a skipped link has none; your own watch
  folders are never reported).
- `client_id` is this machine, made once and kept in `settings.json` (`portal_client_id`).
- Not sent while Portal is off, has no address or no credential. A failure is silent and isn't retried until
  the next check; `FIO_TRACE=1` prints `[portal heartbeat] …` lines on stderr.

## From a terminal

```sh
fio portal signin https://portal.example.com you@example.com   # the password from $FIO_PORTAL_PASSWORD or stdin
fio portal reconcile --dry-run                                 # what it would do; changes nothing
fio portal reconcile                                           # do it
fio portal status
fio portal signout
fio portal token <token>                                       # instead of signing in
```

A one-time code goes in `--totp <code>` or `$FIO_PORTAL_TOTP`. Exit codes: `0` done, `1` refused or failed,
`2` Portal wants a one-time code. `reconcile` prints what it did, one line per skipped link with the reason,
and the folders it keeps:

```
links seen: 3
kept in step: 1
removed: 0
skipped 8177b73e-… "../etc": the folder name "../etc" contains '/': one path segment only
watch folders:
  /Users/you/Downloads/ACME_SPOT/1_DRIVE ← acct-1/folder-1
  /Users/you/Downloads/ACME_SPOT/0_EXPORTS → acct-1/folder-2
```

Where `fio` is: `/usr/bin/fio` (Debian package), `FioTransfer.app/Contents/MacOS/fio` (macOS),
`fio.exe` next to `FioTransfer.exe` (Windows), inside the AppImage (`--appimage-extract`), or
`cargo build --release -p fiotransfer --bin fio` from a checkout.

## When it says no

| Message | Do this |
| --- | --- |
| `no Portal URL is set` | Add the Server URL. |
| `no Portal token or signed-in session is stored` | Sign in, or paste a token. |
| `Portal refused the sign-in (401)` | Check the email and password. |
| `Portal wants a one-time code` | Type the six digits (or `--totp`). |
| `Portal refused the token (401)` | The token is wrong, or the deployment has no `PORTAL_API_TOKEN` at all. Sign in instead. |
| `the Portal session expired` | Sign in again (sessions last 14 days). |
| `Portal has no /api/upload-links (404)` | The URL isn't a Portal; `/api/health` should answer. |
| `can't reach Portal: …` | Network, DNS or TLS: try `curl <url>/api/health`. |
| `skipped …: …` | One link couldn't get a folder; the reason says why. Fix it in Portal. |

## Testing against a real Portal

```sh
PORTAL_LIVE_URL=http://127.0.0.1:18080 PORTAL_LIVE_TOKEN=… \
  cargo test --offline -p fiotransfer --test portal_live -- --nocapture
```

With `PORTAL_LIVE_EMAIL` and `PORTAL_LIVE_PASSWORD` instead of a token it also tests signing in, and
turns TOTP on and off again on that account (the steps to undo it by hand are at the top of
`tests/portal_live.rs`). Each case is skipped unless its variables are set.
