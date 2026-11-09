#!/usr/bin/env python3
"""A tiny static server for the web bundle — the smoke test's server.

It serves ONE folder and nothing else:

* The MIME types are right. `application/wasm` is sent for the module,
  which Python's own `http.server` does NOT do — and the page's
  streaming instantiation (`WebAssembly.instantiateStreaming`) refuses
  anything else.
* `/wait?ms=N` answers after N milliseconds. The smoke test's shim
  keeps a request to it pending while the parade boots: a pending
  fetch PAUSES the browser's virtual clock, so the headless run cannot
  burn its virtual-time budget before the first frame is on the
  canvas. Without that, the screenshot races the first present and
  catches a black rectangle half the time.

    python3 tools/serve.py <folder> <port>

It is a smoke-test server, not a production one: no caching headers,
no directory listing, no range requests.
"""

import os
import sys
import time
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs


class Handler(SimpleHTTPRequestHandler):
    """The static handler, with the wasm MIME type and the delay route."""

    extensions_map = dict(SimpleHTTPRequestHandler.extensions_map)
    extensions_map[".wasm"] = "application/wasm"
    extensions_map[".js"] = "text/javascript"

    def do_GET(self):
        """Serve the folder, or the delay route."""
        parsed = urlparse(self.path)
        if parsed.path == "/wait":
            self._wait(parse_qs(parsed.query))
            return
        super().do_GET()

    def _wait(self, query):
        """Answer after the asked-for delay."""
        try:
            milliseconds = int(query.get("ms", ["1000"])[0])
        except ValueError:
            milliseconds = 1000
        milliseconds = max(0, min(milliseconds, 120000))
        time.sleep(milliseconds / 1000.0)
        body = ("waited %d ms\n" % milliseconds).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def end_headers(self):
        """No caching: the smoke test always wants the fresh file."""
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def log_message(self, fmt, *args):
        """Quiet: the CI log has enough to read already."""


def main(argv):
    if len(argv) != 3:
        sys.stderr.write("usage: serve.py <folder> <port>\n")
        return 2

    folder = argv[1]
    port = int(argv[2])
    if not os.path.isdir(folder):
        sys.stderr.write("serve: %s is not a folder\n" % folder)
        return 1

    os.chdir(folder)
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    sys.stderr.write("serve: %s on http://127.0.0.1:%d/\n" % (folder, port))
    sys.stderr.flush()
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
