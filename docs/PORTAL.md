# Portal

Tick **Sync to FioTransfer** on a Portal upload link, and FioTransfer gives that link a project folder on
your disk. What the client uploads comes down into it. If the link has a return folder, what you put there
goes back up. Portal only says which links and which Frame.io folders; the files move between FioTransfer
and Frame.io.

```
~/Portal/
  Acme Spot 30s/      ← one folder per synced link, named after the link
    IN/               ← what the client uploads (Frame.io → disk)
    OUT/              ← what you send back (disk → Frame.io), only if the link has a return folder
```

## Rules

- **Only ticked links.** A link without Sync to FioTransfer is left alone.
- **IN takes everything the client sent**, including files uploaded before the folder existed. A file
  already on disk with the same name and size is skipped.
- **OUT sends what you add to it** after the folder exists.
- **Nothing is ever deleted**, on either side. Each folder moves files one way only.
- **When a link closes or is unticked**, its folders stop syncing and stay on disk with everything in them.
- **Renaming** a link, or the IN/OUT names, starts a new folder; the old one stays.
- **Edit links in Portal.** Portal's folders are read-only in Watch Folders. Pausing one is fine.

## Setting it up

Settings → **Portal**:

| Field | |
| --- | --- |
| **Server URL** | Your Portal, e.g. `https://portal.example.com`. |
| **Sign in** | Your Portal admin email and password (and the six-digit code if the account has one). Only Portal's session cookie is kept, in the Keychain; the password is not stored. |
| **Access token** | Instead of signing in: the deployment's `PORTAL_API_TOKEN`, for a machine without a person at it. A stored token wins over a session. |
| **Local folder** | Where project folders go. Default `~/Portal`. |
| **Folder names** | What IN and OUT are called in each project. Default `IN` and `OUT`. Portal can name them per link. |
| **Enabled** | Checks Portal every 30 s. Off by default: nothing is contacted until you turn it on. |

**Reconcile now** checks at once. The status line says what the last check did, or why it failed.

## What Portal sends

FioTransfer reads `GET /api/upload-links`. A synced link carries a `sync` object inside its `folder`.
Its presence is the tick; every field in it is optional:

```json
{
  "id": "…", "name": "Acme Spot 30s", "state": "ok",
  "folder": {
    "account_id": "…", "folder_id": "…",
    "sync": {
      "name": "ACME_SPOT_30",
      "download": {"local": "IN"},
      "upload":   {"local": "OUT", "folder_id": "…"}
    }
  }
}
```

| Field | Meaning | Default |
| --- | --- | --- |
| `sync` | The link is synced. `"sync": {}` is enough. | not synced |
| `sync.name` | The project folder's name on disk | the link's `name` |
| `sync.download.folder_id` | The Frame.io folder IN comes from | the link's `folder_id` |
| `sync.download.local` | IN's name | the user's IN name |
| `sync.upload.folder_id` | The Frame.io folder OUT goes to. Without it there is no OUT. | none |
| `sync.upload.local` | OUT's name | the user's OUT name |
| `state` | `ok` while the link works; `expired` or `revoked` stop its folders | live |

Folder names are one path segment: no `/`, `\`, `%`, control characters, `.` or `..`. A link's own name is
cleaned up to fit (`/` becomes `-`); a name Portal sets is refused with a reason instead. Two links that
end up with the same folder name: the lower id gets it, the other is skipped with a reason.

Older Portals that call the sides `drive` and `exports` are still read. A link with only a `turbosort`
value and no `sync` is one two-way folder, `~/Portal/<turbosort>`, as before.

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
  /Users/you/Portal/Acme Spot 30s/IN ← acct-1/folder-1
  /Users/you/Portal/Acme Spot 30s/OUT → acct-1/folder-2
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
