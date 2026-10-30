# A browser capture of the egui front end

The native egui window and the WKWebView front end it replaced could not be
compared fairly: the native window has its own chrome, its own rasteriser and its
own pixel ratio. This directory builds the **same** `FioApp` for the browser,
draws it on a 1180x780 `<canvas>`, and has one headless Chromium capture all four
screens — the same Chromium, the same size, the same device pixel ratio, and
(with WebGL) the same kind of rasteriser the webview had.

It is a measurement apparatus, not a shipping target. The webview front end it
was measured against is retired (the egui app is the app now), so the capture is
a browser view of the same screens rather than a comparison; the browser build
carries its own copy of the five data shapes (`src/model_browser.rs`) because the
engine does not build for wasm32.

```console
$ egui-ui/bench/wasm-shots.sh
webgl: ANGLE (Google, Vulkan 1.3.0 (SwiftShader Device (LLVM 10.0.0) (0x0000C0DE)), SwiftShader driver) / WebGL 2.0 (OpenGL ES 3.0 Chromium)
transfers  1180x780   89345 bytes  2763 colours  46.62% ink  6 frames  0.90s  /tmp/wasm-shots/transfers.png
watch      1180x780   56934 bytes  1906 colours  23.13% ink  9 frames  0.67s  /tmp/wasm-shots/watch.png
history    1180x780   66967 bytes  2066 colours  25.60% ink  7 frames  0.69s  /tmp/wasm-shots/history.png
settings   1180x780   69019 bytes  1977 colours  58.58% ink  8 frames  0.66s  /tmp/wasm-shots/settings.png
```

The dimensions and the pixel ratio are the fixed part; the byte and colour counts
move when the UI changes, because the screens are being edited. Two captures of
the same source are byte-identical.

One command: build with Trunk, serve `web/dist` on a free port, capture, print the
sizes above plus `summary.json`. `PROFILE=debug` skips `--release`; `OUT=` moves
the PNGs; `CHROMIUM=` points at another browser.

## What it is

| file | what it is |
| --- | --- |
| `../src/wasm.rs` | `wasm_bindgen(start)` entry: reads `?view=` and `?fixture=`, starts eframe on `#fio-canvas`, and writes a frame counter onto the canvas |
| `../src/fixture.rs` | the state the captures draw (`tests/snapshots.rs` renders the same one through wgpu) |
| `../web/index.html` | Trunk's HTML: a bare canvas, 1180x780, no margins, no scrollbars |
| `../Trunk.toml` | target `web/index.html`, dist `web/dist`, no default features (see below) |
| `capture.py` | drives Chromium over the DevTools protocol and checks what it captured |
| `wasm-shots.sh` | the one command |

Query parameters, all optional, none of them able to panic:

```
?view=transfers|watch|history|settings   which screen (default: transfers)
?fixture=1                               the populated fixture (default: empty state)
```

## What had to be installed

* `rustup target add wasm32-unknown-unknown` (was already installed here).
* **`trunk` 0.22.1**, as the prebuilt `trunk-aarch64-apple-darwin.tar.gz` from
  <https://github.com/trunk-rs/trunk/releases/tag/v0.22.1>, unpacked to
  `~/.local/bin/trunk`. `cargo install trunk --locked` also works but compiles
  Trunk's whole dependency tree; the prebuilt binary is a 28 MB drop-in.
* **`wasm-bindgen-cli` is not installed by hand**: Trunk downloads the version
  that `Cargo.lock` pins (0.2.129 here) into `~/.cache/trunk` on the first build.
* Nothing Python: `capture.py` is standard library only (small PNG decoder,
  small websocket client), so it runs on the `/usr/bin` python3 too.
* Nothing else: no `wasm-opt` (`data-wasm-opt="0"` in `index.html` — binaryen
  would be one more download for a build nobody ships).

## What changed outside the new files

* `Cargo.toml`: `[lib] crate-type = ["cdylib", "rlib"]` with an explicit name and
  path; `egui_kittest` and `image` moved to
  `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`; `wasm-bindgen`,
  `wasm-bindgen-futures` and `web-sys` added for `wasm32` only; and
  `[features] default = ["native"]` with `[[bin]] required-features = ["native"]`,
  because Trunk runs a plain `cargo build --target wasm32-unknown-unknown`, which
  builds every target of the package, and `main.rs` calls native-only
  `eframe::run_native`. Trunk passes `--no-default-features` (`Trunk.toml`), so
  the browser build compiles the cdylib and nothing else. The native build is
  unchanged: `cargo build`, `cargo run -p fiotransfer-egui` and `cargo test` all
  see default features.
* `src/lib.rs`: `pub mod fixture;` and `#[cfg(target_arch = "wasm32")] pub mod wasm;`.
* `tests/snapshots.rs`: the fixture moved to `src/fixture.rs` (values unchanged);
  the test now imports `fiotransfer_egui::fixture::fixture_state` instead of
  defining it. Its assertions, sizes and PNG paths are untouched.
* `main.rs`, `app.rs`, `theme.rs`, `model.rs` and `views/*` were **not** changed.

## The Chromium flags that work

```console
"$CHROMIUM" --headless=new --window-size=1180,780 --hide-scrollbars \
  --enable-unsafe-swiftshader --use-gl=angle --use-angle=swiftshader
```

`--use-angle=swiftshader` alone gives no WebGL context on this machine (macOS
headless has no GPU path), and since Chrome 129 the software fallback is off
unless `--enable-unsafe-swiftshader` asks for it. With both, the page reports:

```
ANGLE (Google, Vulkan 1.3.0 (SwiftShader Device (LLVM 10.0.0)), SwiftShader driver) / WebGL 2.0
```

`capture.py` prints that line, so a capture always says which rasteriser drew it.
The remaining console output is SwiftShader's own `GPU stall due to ReadPixels`
performance warning, which comes from the screenshot readback itself; there are no
page errors, 404s or wasm exceptions in a good run.
Drop the two GL flags (`--chromium-flag=` with a single dummy flag) and the
canvas stays blank — the script then fails with the last probe rather than
writing a black PNG.

## How a capture decides it has a result

`--screenshot` is not used: it cannot tell a drawn canvas from a blank one. The
script instead requires all of:

1. `<canvas data-frames="n">` (written by `src/wasm.rs` once per frame) is `>= 2`
   — the first frame builds the font atlas, the second draws with it, and a page
   without `?fixture=1` stops repainting there;
2. the screenshot has at least 24 distinct colours — a blank canvas has one;
3. `document.body.dataset.fioError` is unset (`src/wasm.rs` sets it, prints to the
   console and writes the message into the page when startup fails);
4. the canvas measures 1180x780 CSS px with a 1180x780 drawing buffer;
5. the PNG decodes to 1180x780.

Frames are counted rather than slept on, and the toasts are held at their
fixture age (3.5 s) while `data-frames` runs, so a slow machine cannot screenshot
a toast stack that has already aged out.

## What the release build is worth

| thing | size / time |
| --- | --- |
| `web/dist/*_bg.wasm` (wasm-bindgen output, `--release`, no `wasm-opt`) | 3,250,450 bytes |
| `web/dist/*.js` (glue) | 77,025 bytes |
| `trunk build --release`, cold | ~20 s on this 16-core machine |
| `trunk build --release`, crate touched only | 2.8 s |
| capture, one page load per screen | 0.65-0.99 s |

The debug build (no `--release`) is 10.2 MB of wasm and captures identically, so
iterate with `PROFILE=debug` if the extra 7 MB of download time is annoying.

## How close it is to the native snapshot

`cargo test -p fiotransfer-egui --test snapshots` renders the same fixture through
wgpu at the same size. Comparing its `/tmp/egui-shots/*.png` with this capture,
Channel-summed absolute difference over the whole 1180x780 frame:

| screen | mean abs diff (0-255) | pixels differing by >16 | pixels differing by >64 |
| --- | --- | --- | --- |
| transfers | 0.011 | 0.08% | 13 |
| watch | 0.015 | 0.11% | 22 |
| history | 0.008 | 0.06% | 9 |
| settings | 0.007 | 0.04% | 28 |

So the two rasterisers agree everywhere except the antialiased edge of glyphs and
1 px borders: a few dozen pixels per screen. They are not bit-identical, and the
files are not the same size (Chromium's PNG encoder against the `image` crate's),
so compare pixels, not bytes. Two consecutive captures of the same source are
byte-identical (checked: same SHA-256 per screen), so a difference between two runs
is a change in the source, not in the apparatus.

## Limits, honestly

* **WebGL, not DOM.** Both front ends are drawn by Chromium, but this one is a
  WebGL2 canvas through SwiftShader (software Vulkan) and the webview front end is
  HTML/CSS. Text is antialiased by different code: the native snapshot and this
  capture agree to a mean absolute difference of 0.01/255 on Transfers and Watch
  and 0.10/255 on History, but they are not bit-identical.
* **Fonts.** Both use the same Inter TTF through epaint, so advances match; the
  subpixel coverage differs. Glyphs are hinted identically. Nothing here proves
  the *webview's* font metrics match — that is the webview capture's job.
* **Dithering is off** (`eframe::WebOptions { dithering: false }`) because
  `egui_kittest`/wgpu does not dither; leaving it on would add noise that is not
  part of the UI.
* **Toasts are frozen.** The fixture's toasts would expire after 3.5 s of real
  time; a capture holds them at that age. The screenshot therefore shows the
  toast stack the native snapshot shows, and not the app's normal fading.
* **The fixture is the state, not the app.** No bridge, no Frame.io, no file
  I/O: `?fixture=1` is `src/fixture.rs` and nothing else.
* **One page per screen.** Screens are separate loads, so scroll offset, hover and
  focus state are the initial ones. No interaction is replayed.
* **This build is not a deployment target.** `cdylib` + `rlib`, no `wasm-opt`,
  no caching story, no persistence.
