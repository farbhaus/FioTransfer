# FioTransfer

Fast uploads to and downloads from Frame.io, for macOS, Linux and Windows.

![FioTransfer](docs/screenshot.png)

FioTransfer is a desktop app that replaces Frame.io's Transfer app. It moves files as fast as your connection allows and keeps a record of every transfer.

## What it does

- **Downloads from any Frame.io link:** share links, review links and `f.io` short links, from any account, including passphrase-protected ones. Files go to the folder you choose, with their folder structure, and you don't need to sign in.
- **Downloads from other services too:** Smash, WeTransfer, Dropbox and Google Drive links, and Portal download links. Folders arrive file by file with their structure instead of as one zip, and password-protected Smash, WeTransfer and Portal links work.
- **Uploads files and folders:** drop anything onto the window, or right-click it in Finder and choose Upload with FioTransfer. Folders are recreated in Frame.io, and one click copies a share link to what you uploaded that anyone can open. A file that's still being written waits until it's finished, and an upload counts as done only once Frame.io confirms it.
- **Runs at full speed:** each transfer is split across many parallel connections, measured at over 900 Mbit/s.
- **Picks up where it stopped:** interrupted downloads continue from the last finished piece, and failed uploads can be retried from History.
- **Watches folders:** pair a folder on your Mac with a Frame.io folder, from the app or by right-clicking the folder in Finder. New files upload once they've finished copying, download once they've finished uploading to Frame.io, or both ways with Mirror. Subfolders are kept. A re-rendered file uploads again and either overwrites its earlier copy on Frame.io or keeps both, your choice per folder. Deleting a file on one side never deletes it on the other.
- **Logs every transfer:** each one is listed in History, with a log file saved next to the media.
- **Keeps running in the menu bar:** close the window and transfers carry on. "Download from Link…" in the menu bar menu opens a small window for a link and where to save it, with the copied link already filled in. It can also start at login.
- **Updates itself:** it tells you when a new version is out and installs it with one click.
- **Works from the command line:** the `fio` tool does downloads and uploads from scripts.

## The Rust build

This is the Rust rewrite: the same app, the same engine, no webview. `fiotransfer-core` holds the engine (`core/`),
`egui-ui` draws the screens and runs it, `sys/` is the only crate allowed to be `unsafe`, and the root package is the
`fio` command line plus the bench harness. Notes for working on it — the layout, the test suites, the packaging and the
updater — are in [docs/RUST-PORT.md](docs/RUST-PORT.md).
