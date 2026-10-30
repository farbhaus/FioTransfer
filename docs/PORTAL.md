# Connecting FioTransfer to Portal

Portal names the projects; FioTransfer does the moving. Files still flow only between
FioTransfer and Frame.io — Portal never carries a byte of media.

## What Portal has to say

Portal marks an upload link's Frame.io folder with a `.turbosort` file whose content is
the `Live_Work` folder name that project belongs to. Its admin API serves the same value
as `turbosort` on each row of `GET /api/upload-links`, alongside the `folder` object
(`account_id`, `folder_id`, `folder_name`) it publishes into.

FioTransfer reads that list and makes sure a **mirrored watch folder** exists for every
link that carries a usable marker:

    ~/Live_Work/<turbosort>   <->   Frame.io / <folder_name>

Two-way: uploads and downloads both follow changes. The local root is yours — FioTransfer
owns the path and creates the folder; Portal's own `destination_path` / `path_template`
are deliberately ignored, because the NAS layout Portal writes for Project Zeus is not
the layout of this machine.

## Two one-way folders, instead of one mirror

A project can be split in two: the client sends files to us, and we send finished work
back. Portal says so by adding a `sync` object **inside the link's existing `folder`** —
it rides on the row `GET /api/upload-links` already returns, so there is no new endpoint and
nothing to configure in FioTransfer:

```json
"folder": {
  "type": "frameio", "account_id": "…", "folder_id": "…", "path": […],
  "turbosort": "20261028_YML-XMAS-Luc",
  "sync": {
    "mode": "add_only",
    "drive":   {"local": "1_DRIVE",   "folder_id": "…"},
    "exports": {"local": "0_EXPORTS", "folder_id": "…"}
  }
}
```

That link gets **two watches**, both under its marker folder and both owned by the link:

| Local folder | Frame.io folder | Direction |
| --- | --- | --- |
| `~/Live_Work/20261028_YML-XMAS-Luc/1_DRIVE` | `sync.drive.folder_id`, or the link's own folder | **down**: Frame.io → local |
| `~/Live_Work/20261028_YML-XMAS-Luc/0_EXPORTS` | `sync.exports.folder_id` | **up**: local → Frame.io |

`exports` is optional: without it there is no upload folder, only the download one. `local` must be
exactly one path segment — the same rule the marker follows — so a mapping can never point outside
`~/Live_Work/<marker>/`; a bad one is refused with a stated reason and nothing is created.
`mode` is `add_only`, and that is the only mode there is: an unknown mode is read as `add_only` and
appears in the report as a `note:` line instead of failing the link.

**Deletions never propagate, in either direction.** A one-way watch only ever moves files one way, so
nothing that disappears on one side can take anything off the other: a `download` watch never uploads,
an `upload` watch never downloads, and no part of the watch engine calls Frame.io's delete API. A file
you delete locally stays on Frame.io, and a file deleted on Frame.io stays on your disk.

Withdrawing a mapping — clearing `exports` in Portal, or removing the whole `sync` object — removes
**exactly that watch** and leaves the folder on disk with everything in it. That is the rule that makes
it safe to clear Frame.io space: the local folder becomes an ordinary folder again, not a hole.

A link with no `sync` in its `folder` is unchanged: one mirrored watch at `~/Live_Work/<marker>`.

## What happens the first time it runs

A marker names a `Live_Work` folder that may already hold media — from Project Zeus, a previous app, or your own
work. So a watch made by the reconciler does **not** start by downloading the project.

The folder is walked once and what is already there is recorded as known, both ways: existing local files are
baselined rather than uploaded again, and the remote listing records matches rather than fetching them. Files that
appear afterwards sync normally in both directions.

That is deliberate: connecting FioTransfer to a Portal with forty populated project folders must not re-download
forty projects, and must not push a second copy of everything you already have.

## Setting it up

Settings → **Portal**:

| Field | What goes in it |
| --- | --- |
| **Server URL** | Your Portal, e.g. `https://portal.yourdomain.com`. Must be `http://` or `https://` with a host. |
| **Access token** | The `PORTAL_API_TOKEN` from Portal's `.env` (generate with `openssl rand -base64 32 \| tr '+/' '-_' \| tr -d '='`). Saved to the login keychain; never written to `settings.json`, never displayed again. Optional: see "Signing in instead" below. |
| **Sign in instead** | Your Portal admin email and password. FioTransfer calls Portal's own login, keeps the session cookie, and throws the password away. Use this when the deployment has no `PORTAL_API_TOKEN`. If the account has TOTP on, a **One-time code** row appears for the six digits, on the same session. |
| **Local folder** | Where the project folders live. Default `~/Live_Work`. |
| **Enabled** | Off by default: nothing is contacted until you turn it on. |

### Signing in instead of a token

`PORTAL_API_TOKEN` is optional in Portal and `deploy.sh` never writes one. While it is unset,
every `/api` endpoint accepts only a browser session cookie, so no bearer token can work however
correct it is. On such a deployment, sign in:

1. **Server URL**: your Portal's address.
2. **Sign in instead**: the admin email and password you use in Portal's own login screen, then
   **Sign in**.
3. The row then reads **Signed in as <email>** and a **Sign out** button replaces the fields.

What is kept: the session cookie Portal sets (`portal_session`, fourteen days), in the login
keychain under its own item, and your email in `settings.json` so the field is prefilled next
time. What is not kept, anywhere: the password. It exists for the length of that one request, is
never written to `settings.json`, never logged, and never shown by the app again.

Precedence: if a bearer token is stored, it wins — that is what a headless install has. Otherwise
the session is used. A session Portal stops accepting is reported as *"the Portal session
expired"*, which is a different message from *"Portal refused the token"*: the fix is to sign in
again, not to look for a token.

### An account with a second factor

If the admin account has TOTP turned on in Portal, the password is only half of the sign-in: Portal
answers `totp_required` and keeps a *pending* uid in the same session until the six digits arrive. FioTransfer
finishes it. The card then shows a **One-time code** row under the two fields — six digits, one **Sign in** —
and the sign-in completes on the same session the password step opened. Nothing is stored until the code is
accepted: a pending session is not a credential, and the app never keeps it.

What is not kept or shown: the code. It is typed into the row (or given to `fio` on the command line), sent
once, and dropped; it is not written to `settings.json`, not put in a log, and not part of any message.

A wrong code is refused with Portal's own sentence — *"Portal refused the one-time code (401): Invalid code"*. If
the message says *"your login session expired — start again"* instead, the pending step has timed out (Portal
allows five minutes, and five wrong codes), and pressing **Sign in** with the password starts a new one.

Passkeys are not covered: a passkey sign-in is a browser ceremony, so on an account that has one and no password,
sign in with a browser and store a bearer token.

Portal marks its session cookie `Secure` even when it is reached over plain http
(`SESSION_COOKIE_SECURE` defaults to true). Over `https://` the cookie is sent exactly as Portal
wrote it. Over plain `http://` to another machine it is sent too, without the `Secure` attribute:
the password already crossed that same connection. Use `https://` for any Portal that is not on
this machine.

### If the token is refused

`PORTAL_API_TOKEN` is **optional in Portal itself**: `deploy.sh` writes a `.env` without it, and
while it is unset every `/api` endpoint accepts only a browser session cookie — a bearer token
cannot work, however correct it is. Even then Portal answers `401 Authentication required`, which
looks identical to a wrong token.

So if the status line says *"Portal refused the token"*, either **sign in** with the admin email
and password (see above), or make sure the deployment has a token:

```sh
ssh you@your-portal-host
cd /path/to/portal            # where deploy.sh and .env live
grep -c '^PORTAL_API_TOKEN=' .env || echo 'PORTAL_API_TOKEN=…' >> .env
./deploy.sh --update          # pulls, recreates; data and transfers survive
```

Generate the value with `openssl rand -base64 32 | tr '+/' '-_' | tr -d '='`. That same string goes
into FioTransfer's **Access token** field. There is no admin screen that mints one: the bearer token
is the single value in Portal's `.env`, on purpose.

Then press **Reconcile now**. The status line tells you what happened:

    2 min ago · 2 links, 3 folders kept in step, 1 removed
    Failed 2 min ago: can't reach Portal: connection refused

Folder counts appear only when the last reconcile succeeded — a failure is never dressed
up as a success. While **Enabled** is on, FioTransfer re-checks every 30 seconds
(`portal_every_secs`, clamped to 5 s – 24 h; a value below the floor means the floor, not the
default); while it is off, nothing goes out.

## From a terminal

The same four steps without opening the window — for a machine reached over ssh, or a cron job.
Below, `fio` is a plain command; the subsection after it says where that command lives on each kind
of install:

```sh
fio portal signin https://portal.shed.gay you@yourdomain     # session only; the password is not stored
fio portal reconcile --dry-run                                # what it WOULD do, and changes nothing
fio portal reconcile                                          # what it did, and what it refused
fio portal status                                             # the sentence the Settings card shows
fio portal signout
fio portal token <token>                                      # instead: a deployment's PORTAL_API_TOKEN, or a passkey-only account
```

### Where `fio` comes from

`fio` is the command line front end of the same app and ships in the same package as the window, so
there is nothing extra to install:

| Installed how | Where `fio` is | On your `PATH`? |
| --- | --- | --- |
| Debian package | `/usr/bin/fio`, beside `/usr/bin/fiotransfer-egui` | yes |
| AppImage | `usr/bin/fio` inside the image — `--appimage-extract` writes it out | no: an AppImage installs nothing |
| macOS `.app` | `/Applications/FioTransfer.app/Contents/MacOS/fio` | no: a `.app` is not on `PATH` |
| Windows installer | `fio.exe` in the install folder, beside `FioTransfer.exe` | no: the installer leaves `PATH` alone |
| A checkout | `target/release/fio`, from `cargo build --release -p fiotransfer --bin fio` | only if you add it |

Only the Debian package puts `fio` on `PATH`. Everywhere else the executable is inside the thing
you installed — an `.app`, an AppImage, the installer's own folder — so the full path in the table
is the command; those formats are self-contained and never touch `PATH`. If `fio` is not where the
table says, the package predates the CLI: build it from a checkout with
`cargo build --release -p fiotransfer --bin fio`. That is the root package, it links the engine
without the window, and it needs no GUI dependencies.

`signin` takes the password from `$FIO_PORTAL_PASSWORD` when set, otherwise reads one line — and on a terminal it
says plainly that the typing is visible, because there is no no-echo helper here.

On an account with TOTP, the same command takes the six digits: `--totp <code>`, then `$FIO_PORTAL_TOTP`, then one
line from stdin (the prompt says the typing is visible too). The code is sent on the session the password step
opened, and is never printed or written anywhere:

```sh
FIO_PORTAL_PASSWORD=… fio portal signin https://portal.shed.gay you@yourdomain --totp 123456
```

`reconcile` prints the counts, then one line per skipped marker **with the reason**, then the folders it keeps in
step:

```
links seen: 3
kept in step: 1
removed: 0
skipped 8177b73e-… "../etc": the turbosort marker "../etc" contains '/': one path segment only
skipped 99715dbe-… ".": the turbosort marker "." is not a folder name
watch folders:
  ~/Live_Work/Farbhaus ← acct-1/folder-1
```

Exit codes are meant for scripts: `0` done, `1` refused or failed (with the sentence you would have seen on
screen), `2` Portal wants a one-time code and none was given — pass `--totp`, set `$FIO_PORTAL_TOTP`, or let the
prompt read one. A code Portal then refuses is `1`, with Portal's sentence.

A second `reconcile` reporting `kept in step: 0` is the system saying the first one already did the work — that is
the idempotence rule, visible.

## What FioTransfer will and will not do

* It only ever touches watches it created. Those carry `origin = portal:<link id>`; a
  watch you made by hand has an empty origin and is never edited or removed.
* A marker must be exactly one path segment. `.`, `..`, `a/b`, `a\b`, `a%2f` and control
  characters are refused with a stated reason, and nothing is created on disk. Portal
  enforces the same rule when the marker is written, so the two sides agree by
  construction.
* When a link disappears, is switched off, or its marker is cleared, the folder it
  created is **unwatched — not deleted**. Removing local media is always your decision.
* A `sync` link's two folders are watched one way each (`1_DRIVE` down, `0_EXPORTS` up), and
  a deletion is never copied in either direction. Withdrawing one mapping removes that one
  watch and no other: clearing `exports` leaves the drive watch running.
* Reconciling twice changes nothing the second time.

## When it says no

Every refusal is a sentence you can act on. The status line, the CLI and the logs use the same words, so
this table works for all three.

| What you see | What it means | What to do |
| --- | --- | --- |
| `Not set up: no Portal address and no token yet.` | Nothing is configured. | Fill in the address, then sign in or paste a token. |
| `No Portal address yet: add one above.` | A credential is stored, but no URL. | Add the URL, e.g. `https://portal.shed.gay`. |
| `No access token yet: paste one above and press Save.` | URL set, no credential. | Sign in with the admin email and password, or paste a token. |
| `Portal refused the sign-in (401): Invalid email or password` | The email or the password is wrong. | Check both against Portal's `.env` (`ADMIN_EMAIL`). |
| `Portal wants a one-time code…` | The account has a second factor. | Type the six digits — in Settings, or `--totp`/`$FIO_PORTAL_TOTP` for the CLI. |
| `Portal refused the one-time code (401): Invalid code` | The code was wrong or expired (30 s window). | Wait for the next one and retype it. A phone whose clock is far out will not work. |
| `Portal refused the token (401). It may be wrong, or Portal's .env may set none at all` | The bearer token is wrong **or does not exist** — Portal answers both the same way. | Check `PORTAL_API_TOKEN` on the server; while it is unset no token can work. Or sign in instead. |
| `the Portal session expired (401): sign in again…` | The session cookie aged out (14 days) or was replaced. | Sign in again; nothing else needs redoing. |
| `Portal has no /api/upload-links (404): is that the Portal address?` | The URL is not a Portal, or not this version of one. | Check the address; `/api/health` should answer `{"status":"ok"…}`. |
| `can't reach Portal: …` | DNS, TLS or the network. | `curl https://your-host/api/health` from the same machine. |
| `Configured. Press Reconcile now, or switch it on to check every 30 s.` | Nothing has run yet. | Press the button, or switch **Enabled** on. |
| `just now · 3 links, 0 folders kept in step, 0 removed, 2 skipped` | It ran, found nothing new to do. | Normal on a second run. Read the skip lines below it. |
| `skipped …: the turbosort marker "…" contains '/': one path segment only` | A marker in Portal is not a single folder name. | Fix the link in Portal; FioTransfer refuses it rather than writing outside the root. |

Two things that are *not* errors, though they look like them: a second reconcile reporting `0 folders kept in
step` is idempotence working, and `2 skipped` on a Portal that has links without markers is the reconciler
correctly ignoring them.

## Checking it without your Portal

```sh
# a real instance, local, no Frame.io needed for the reconcile itself
PORTAL_LIVE_URL=http://127.0.0.1:18080 PORTAL_LIVE_TOKEN=… \
  cargo test --offline -p fiotransfer --test portal_live -- --nocapture

# the sign-in path, against a Portal that holds no token
PORTAL_LIVE_URL=http://127.0.0.1:18080 PORTAL_LIVE_EMAIL=… PORTAL_LIVE_PASSWORD=… \
  cargo test --offline -p fiotransfer --test portal_live -- --nocapture
```

That test serves three links (one good marker, one `../etc`, one `.`) and asserts one
mirrored watch, two stated refusals, and an idempotent second run; with the sign-in
variables it also asserts the sign-in itself, a refused password that stores nothing, and
a session whose cookies are gone reading as expired rather than as a refused token. Each
case is skipped unless its variables are set, so ordinary test runs stay offline. The
password is read from the environment only: it is never a constant in the test file.

The second-factor case goes further, on the same variables: it turns TOTP **on** through
Portal's own `/api/settings/totp/*` routes (computing the codes itself, RFC 6238), proves a
sign-in that stops at a code, a wrong code refused and nothing stored, the right code
signing in, and `GET /api/upload-links` with the session it produced — then turns TOTP **off**
again. The restore runs whether the body passed, failed or panicked, and the case ends by
asserting that the password alone signs in again. If the process is killed before that, the
manual step is at the head of the case in `tests/portal_live.rs`:
`POST /api/settings/totp/disable` with the admin's password.

## What it does not do yet

* Portal's sync rules are read for reference only; FioTransfer does not adopt them.
* The `sync` object has one mode, `add_only`. Anything else is read as `add_only` and noted in
  the report; a mode that means something different needs a version that implements it.
* A `sync` mapping starts, like every Portal watch, by recording what is already there rather
  than moving it. Files the client sent *before* the mapping existed stay on Frame.io: add the
  folder by hand — or pause the watch, empty its record and resume — if they have to come down.
* Nothing is sent back to Portal — a link's state in Portal is unaffected by what
  FioTransfer does with its folder.
* The token cannot be rotated from the app; change it in Portal and re-enter it here.
* A passkey sign-in is not something the app can complete: sign in with a browser and use a
  bearer token on such a deployment. TOTP is: see "An account with a second factor".
* Nothing about the second factor is configurable here: the app assumes the six digits and the
  30 s step that Portal's own login asks for, and has no recovery-code path (Portal's recovery
  codes work only in its own login screen).
