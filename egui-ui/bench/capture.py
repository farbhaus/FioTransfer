#!/usr/bin/env python3
"""Capture the wasm build of the egui front end in headless Chromium.

A measurement apparatus: the native egui window and the webview front end are
rasterised differently, so this build puts the same `FioApp` on a 1180x780
`<canvas>` and lets one browser capture all four screens at the same size, the
same device pixel ratio and the same rasteriser.

Driven over the DevTools protocol, not `--screenshot`, because a capture must be
able to say *what* it captured:

  * `<canvas data-frames="n">` (written by `src/wasm.rs`) must be >= --frames
    (2: the first frame builds the font atlas, the second draws with it; a page
    with no fixture draws those two and then stops repainting),
  * the PNG must have non-uniform pixels (a blank canvas is not a result),
  * `document.body.dataset.fioError` must be absent,
  * the canvas must measure 1180x780 CSS px with a 1180x780 drawing buffer, and
  * the PNG must come out 1180x780.

Only the standard library is used — the PNG decoder and the websocket client
below are deliberately minimal, so this runs on any python3 without packages.

Usage (see `bench/wasm-shots.sh`, which does build + serve + capture):

    python3 bench/capture.py --base-url http://127.0.0.1:8137 \
        --out /tmp/wasm-shots --chromium "/Applications/Chromium.app/Contents/MacOS/Chromium"
"""

import argparse
import base64
import functools
import hashlib
import http.server
import json
import os
import shutil
import socket
import socketserver
import struct
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
import zlib
from urllib.parse import urlparse

VIEWS = ["transfers", "watch", "history", "settings"]

# ---------------------------------------------------------------- PNG decoding


def png_size(data):
    """(width, height) from the IHDR chunk."""
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("not a PNG")
    return struct.unpack(">II", data[16:24])


def png_rgba(data):
    """Decode an 8-bit non-interlaced RGB/RGBA PNG to (width, height, bytes)."""
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("not a PNG")
    pos = 8
    width = height = depth = colour = None
    idat = bytearray()
    while pos < len(data):
        (length,) = struct.unpack(">I", data[pos : pos + 4])
        kind = data[pos + 4 : pos + 8]
        body = data[pos + 8 : pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            width, height, depth, colour, _, _, interlace = struct.unpack(">IIBBBBB", body)
            if depth != 8 or colour not in (2, 6) or interlace != 0:
                raise ValueError(f"unsupported PNG: depth {depth} colour {colour}")
        elif kind == b"IDAT":
            idat += body
        elif kind == b"IEND":
            break
    channels = 4 if colour == 6 else 3
    raw = zlib.decompress(bytes(idat))
    stride = width * channels
    out = bytearray(width * height * 4)
    prev = bytearray(stride)
    src = 0
    for y in range(height):
        filt = raw[src]
        src += 1
        line = bytearray(raw[src : src + stride])
        src += stride
        if filt == 1:  # Sub
            for i in range(channels, stride):
                line[i] = (line[i] + line[i - channels]) & 0xFF
        elif filt == 2:  # Up
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 0xFF
        elif filt == 3:  # Average
            for i in range(stride):
                left = line[i - channels] if i >= channels else 0
                line[i] = (line[i] + ((left + prev[i]) >> 1)) & 0xFF
        elif filt == 4:  # Paeth
            for i in range(stride):
                a = line[i - channels] if i >= channels else 0
                b = prev[i]
                c = prev[i - channels] if i >= channels else 0
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pred = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                line[i] = (line[i] + pred) & 0xFF
        dst = y * width * 4
        if channels == 4:
            out[dst : dst + width * 4] = line
        else:
            for x in range(width):
                out[dst + x * 4 : dst + x * 4 + 3] = line[x * 3 : x * 3 + 3]
                out[dst + x * 4 + 3] = 0xFF
        prev = line
    return width, height, bytes(out)


def image_stats(data):
    """Enough to tell a drawn screen from a blank one."""
    width, height, px = png_rgba(data)
    colours = set()
    non_black = 0
    for i in range(0, len(px), 4):
        value = px[i : i + 4]
        colours.add(value)
        if value[0] or value[1] or value[2]:
            non_black += 1
    total = width * height
    return {
        "colours": len(colours),
        "non_black": non_black,
        "non_black_pct": round(100.0 * non_black / total, 2),
        "sha256": hashlib.sha256(data).hexdigest()[:16],
    }


# ------------------------------------------------------------- websocket client


class Closed(Exception):
    pass


class WebSocket:
    """A websocket client for exactly what the DevTools protocol needs.

    Text frames in, text frames out, ping answered, no extensions, no
    fragmentation out. Chromium's replies (a screenshot is a few hundred kB) are
    single unfragmented frames; continuation frames are still accumulated.
    """

    def __init__(self, url, timeout=60.0):
        parts = urlparse(url)
        host = parts.hostname or "127.0.0.1"
        port = parts.port or 80
        path = parts.path or "/"
        if parts.query:
            path += "?" + parts.query
        self.sock = socket.create_connection((host, port), timeout=timeout)
        self.sock.settimeout(timeout)
        key = base64.b64encode(os.urandom(16)).decode("ascii")
        request = (
            f"GET {path} HTTP/1.1\r\n"
            f"Host: {host}:{port}\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n\r\n"
        )
        self.sock.sendall(request.encode("ascii"))
        raw = b""
        while b"\r\n\r\n" not in raw:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise ConnectionError("Chromium closed the websocket during the handshake")
            raw += chunk
        head, _, self.buffer = raw.partition(b"\r\n\r\n")
        status = head.split(b"\r\n")[0]
        if b"101" not in status:
            raise ConnectionError(f"websocket handshake refused: {status!r}")

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass

    def _recv_exact(self, count):
        while len(self.buffer) < count:
            chunk = self.sock.recv(1 << 16)
            if not chunk:
                raise Closed("Chromium closed the websocket")
            self.buffer += chunk
        out, self.buffer = self.buffer[:count], self.buffer[count:]
        return out

    def send(self, text):
        payload = text.encode("utf-8")
        header = bytearray([0x81])
        length = len(payload)
        if length < 126:
            header.append(0x80 | length)
        elif length < (1 << 16):
            header.append(0x80 | 126)
            header += struct.pack(">H", length)
        else:
            header.append(0x80 | 127)
            header += struct.pack(">Q", length)
        mask = os.urandom(4)
        header += mask
        self.sock.sendall(
            bytes(header) + bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        )

    def recv(self):
        """One whole message."""
        chunks = []
        while True:
            first, second = self._recv_exact(2)
            finished = bool(first & 0x80)
            opcode = first & 0x0F
            masked = bool(second & 0x80)
            length = second & 0x7F
            if length == 126:
                (length,) = struct.unpack(">H", self._recv_exact(2))
            elif length == 127:
                (length,) = struct.unpack(">Q", self._recv_exact(8))
            mask = self._recv_exact(4) if masked else None
            payload = self._recv_exact(length)
            if mask:
                payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
            if opcode == 0x8:
                raise Closed("Chromium closed the websocket")
            if opcode == 0x9:
                self._pong(payload)
                continue
            if opcode == 0xA:
                continue
            chunks.append(payload)
            if finished:
                return b"".join(chunks).decode("utf-8")

    def _pong(self, payload):
        header = bytearray([0x8A])
        length = len(payload)
        if length < 126:
            header.append(0x80 | length)
        else:
            header.append(0x80 | 126)
            header += struct.pack(">H", length)
        mask = os.urandom(4)
        header += mask
        self.sock.sendall(
            bytes(header) + bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        )


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    """A static server that says nothing: the capture prints its own report."""

    def log_message(self, *args):
        pass


def serve_directory(directory):
    """Serve `directory` on 127.0.0.1 (any free port) for the length of the run."""
    handler = functools.partial(QuietHandler, directory=directory)
    server = socketserver.ThreadingTCPServer(("127.0.0.1", 0), handler)
    server.daemon_threads = True
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return f"http://127.0.0.1:{server.server_address[1]}", server


class CDP:
    """Browser-level session: one target, flattened, ids routed to callers."""

    def __init__(self, url):
        self.ws = WebSocket(url)
        self.next_id = 0
        self.events = []
        self.session = None

    def send(self, method, params=None, session=None):
        self.next_id += 1
        message = {"id": self.next_id, "method": method, "params": params or {}}
        if session:
            message["sessionId"] = session
        self.ws.send(json.dumps(message))
        while True:
            reply = self._read()
            if "id" in reply:
                if reply["id"] != self.next_id:
                    raise RuntimeError(f"out-of-order reply to {reply['id']}")
                if "error" in reply:
                    raise RuntimeError(f"{method}: {reply['error']}")
                return reply.get("result", {})

    def _read(self):
        message = json.loads(self.ws.recv())
        if "id" not in message:
            self.events.append(message)
        return message

    def wait_event(self, method, timeout=60.0):
        deadline = time.time() + timeout
        while True:
            for index, event in enumerate(self.events):
                if event["method"] == method:
                    return self.events.pop(index)
            if time.time() > deadline:
                raise TimeoutError(f"no {method} within {timeout:.0f}s")
            self.events.append(self._read())

    def take_console(self):
        out = []
        for event in self.events:
            method = event["method"]
            params = event.get("params", {})
            if method == "Runtime.consoleAPICalled":
                text = " ".join(
                    str(a.get("value", a.get("description", ""))) for a in params.get("args", [])
                )
                out.append(f"console.{params.get('type')}: {text}")
            elif method == "Runtime.exceptionThrown":
                details = params.get("exceptionDetails", {})
                text = details.get("text", "")
                description = details.get("exception", {}).get("description", "")
                out.append(f"exception: {text} {description}".strip())
            elif method == "Log.entryAdded":
                entry = params.get("entry", {})
                out.append(f"log[{entry.get('level')}]: {entry.get('text')}")
        self.events.clear()
        return out


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def debugger_url(port, timeout=20.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/json/version", timeout=1) as reply:
                return json.load(reply)["webSocketDebuggerUrl"]
        except Exception:
            time.sleep(0.1)
    raise TimeoutError(f"Chromium never opened its debugger on port {port}")


PROBE = """(() => {
  const c = document.getElementById('fio-canvas');
  const r = c && c.getBoundingClientRect();
  return JSON.stringify({
    frames: c ? Number(c.dataset.frames || 0) : -1,
    buffer: c ? [c.width, c.height] : null,
    rect: r ? [r.width, r.height, r.left, r.top] : null,
    error: (document.body && document.body.dataset.fioError) || null
  });
})()"""

WEBGL = """(() => {
  const c = document.createElement('canvas');
  const gl = c.getContext('webgl2') || c.getContext('webgl');
  if (!gl) return 'no webgl';
  const dbg = gl.getExtension('WEBGL_debug_renderer_info');
  return (dbg ? gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL) : gl.getParameter(gl.RENDERER))
    + ' / ' + gl.getParameter(gl.VERSION);
})()"""


def evaluate(cdp, session, expression):
    result = cdp.send(
        "Runtime.evaluate",
        {"expression": expression, "returnByValue": True},
        session=session,
    )
    if "exceptionDetails" in result:
        raise RuntimeError(f"page threw: {result['exceptionDetails']}")
    return result.get("result", {}).get("value")


def capture_view(cdp, session, view, args):
    url = f"{args.base_url}/index.html?view={view}&fixture=1"
    cdp.send("Page.navigate", {"url": url}, session=session)
    cdp.wait_event("Page.loadEventFired")

    probe = None
    started = time.time()
    while time.time() - started < args.timeout:
        time.sleep(args.interval)
        probe = json.loads(evaluate(cdp, session, PROBE))
        if probe["error"]:
            raise RuntimeError(f"{view}: the page reported {probe['error']!r}")
        if probe["frames"] < args.frames:
            continue
        shot = cdp.send(
            "Page.captureScreenshot",
            {
                "format": "png",
                "captureBeyondViewport": False,
                "clip": {
                    "x": 0,
                    "y": 0,
                    "width": args.width,
                    "height": args.height,
                    "scale": 1,
                },
            },
            session=session,
        )
        data = base64.b64decode(shot["data"])
        stats = image_stats(data)
        if stats["colours"] < args.min_colours:
            print(f"{view}: {stats['colours']} colours after {probe['frames']} frames, waiting", flush=True)
            continue
        width, height = png_size(data)
        path = os.path.join(args.out, f"{view}.png")
        with open(path, "wb") as handle:
            handle.write(data)
        return {
            "view": view,
            "path": path,
            "bytes": len(data),
            "width": width,
            "height": height,
            "frames": probe["frames"],
            "buffer": probe["buffer"],
            "rect": probe["rect"],
            "colours": stats["colours"],
            "non_black_pct": stats["non_black_pct"],
            "sha256": stats["sha256"],
            "seconds": round(time.time() - started, 2),
        }
    raise TimeoutError(
        f"{view}: no drawn frame within {args.timeout:.0f}s "
        f"(last probe {probe}; is WebGL running?)"
    )


DEFAULT_FLAGS = [
    # SwiftShader is the only GL Chromium hands a headless page on macOS, and
    # since Chrome 129 the software fallback has to be asked for by name.
    "--enable-unsafe-swiftshader",
    "--use-gl=angle",
    "--use-angle=swiftshader",
]


def run(args):
    httpd = None
    if args.base_url is None:
        if not args.dist:
            raise SystemExit("give either --dist <built directory> or --base-url <url>")
        args.base_url, httpd = serve_directory(args.dist)
        print(f"serving {args.dist} on {args.base_url}", flush=True)

    profile = tempfile.mkdtemp(prefix="fio-wasm-shots-")
    port = free_port()
    command = [
        args.chromium,
        "--headless=new",
        f"--remote-debugging-port={port}",
        f"--user-data-dir={profile}",
        f"--window-size={args.width},{args.height}",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-background-networking",
        "--disable-extensions",
        "--hide-scrollbars",
        "--mute-audio",
    ] + args.chromium_flags + ["about:blank"]
    print("chromium: " + " ".join(command), flush=True)
    browser = subprocess.Popen(
        command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True
    )
    shots = []
    webgl = None
    try:
        cdp = CDP(debugger_url(port))
        target = cdp.send("Target.createTarget", {"url": "about:blank"})
        attached = cdp.send(
            "Target.attachToTarget", {"targetId": target["targetId"], "flatten": True}
        )
        session = attached["sessionId"]
        for method in ("Page.enable", "Runtime.enable", "Log.enable"):
            cdp.send(method, session=session)
        cdp.send(
            "Emulation.setDeviceMetricsOverride",
            {
                "width": args.width,
                "height": args.height,
                "deviceScaleFactor": 1,
                "mobile": False,
                "screenWidth": args.width,
                "screenHeight": args.height,
            },
            session=session,
        )
        webgl = evaluate(cdp, session, WEBGL)
        print(f"webgl: {webgl}", flush=True)

        for view in VIEWS:
            cdp.take_console()  # drop anything the previous screen left behind
            shot = capture_view(cdp, session, view, args)
            shots.append(shot)
            print(json.dumps(shot), flush=True)
            for line in cdp.take_console():
                print(f"  {view}: {line}", flush=True)
    finally:
        try:
            if "cdp" in dir():
                cdp.ws.close()
        except Exception:
            pass
        browser.terminate()
        try:
            browser.wait(timeout=10)
        except subprocess.TimeoutExpired:
            browser.kill()
        shutil.rmtree(profile, ignore_errors=True)
        if httpd is not None:
            httpd.shutdown()
            httpd.server_close()

    summary = {"webgl": webgl, "shots": shots}
    for shot in shots:
        print(
            f"{shot['view']:<10} {shot['width']}x{shot['height']}"
            f" {shot['bytes']:>7} bytes"
            f" {shot['colours']:>5} colours"
            f" {shot['non_black_pct']:>6.2f}% ink"
            f" {shot['frames']:>2} frames"
            f" {shot['seconds']:>5.2f}s  {shot['path']}",
            flush=True,
        )
    print(json.dumps(summary, indent=2))
    if args.report:
        with open(args.report, "w") as handle:
            json.dump(summary, handle, indent=2)
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--dist",
        default=None,
        help="static directory to serve (web/dist); ignored if --base-url is given",
    )
    parser.add_argument("--base-url", default=None)
    parser.add_argument("--out", default="/tmp/wasm-shots")
    parser.add_argument("--chromium", default="/Applications/Chromium.app/Contents/MacOS/Chromium")
    parser.add_argument("--width", type=int, default=1180)
    parser.add_argument("--height", type=int, default=780)
    parser.add_argument(
        "--frames",
        type=int,
        default=2,
        help="frames to wait for (2: the first builds the font atlas, the second draws with it)",
    )
    parser.add_argument("--min-colours", type=int, default=24)
    parser.add_argument("--timeout", type=float, default=40.0)
    parser.add_argument("--interval", type=float, default=0.15)
    parser.add_argument("--report", default=None, help="write the summary JSON here")
    parser.add_argument(
        "--chromium-flag",
        dest="chromium_flags",
        action="append",
        default=None,
        help="extra Chromium flag (repeatable); replaces the default GL flags",
    )
    args = parser.parse_args(argv)
    if args.chromium_flags is None:
        args.chromium_flags = DEFAULT_FLAGS
    os.makedirs(args.out, exist_ok=True)
    return run(args)


if __name__ == "__main__":
    sys.exit(main())
