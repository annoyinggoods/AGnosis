#!/usr/bin/env python3
"""Write a copy of the page that waits for the parade and forces a back end.

WHY this exists: the CI has to exercise the *forced* back ends (the
roadmap's acceptance criterion — "the fallback is exercised by a forced
flag").

The page now has a ``?backend=`` URL parameter of its own (the wasm
boots asynchronously, so the page polls until a back end is reported
and only then asks for the forced one). The shim below is still what
the CI uses, because it also **waits** for the page to come up and to
carry ink — which is what makes the screenshot and the DOM dump show
the parade instead of a black rectangle and a "starting..." status.

So the CI drives the page's **own** back-end selector: the shim below
sets the ``<select id="backend">`` of the top bar and fires its
``change`` event, which is exactly what a user's click produces. The
switch therefore goes through the page's public path
(``set_backend`` in the wasm) and nothing in the wasm is special-cased.

The shim is a **module** script, and that is the point: the browser
delays the ``load`` event until every module script has finished
evaluating. The shim therefore keeps the load event pending until the
wasm has booted — which is what makes ``--dump-dom`` see the page's
final state (its status line, and anything it wrote into ``#diag``)
instead of the "starting..." the page shows before the first frame.
Chrome dumps the DOM at the load event, so this is the only way to
read the page *after* it ran.

    python3 tools/force-backend.py index.html auto      smoke-auto.html
    python3 tools/force-backend.py index.html canvas2d  smoke-canvas2d.html

The copy is written NEXT TO the original, so the page's relative import
of ``./agnosis_web.js`` — and the ``.wasm`` beside it — still resolve. (A
copy in a sub-folder would need the import path rewritten.)

The back end may be ``auto``: the shim then only waits for the page to
come up, which is what the default leg needs.
"""

import sys


# The shim, as it is injected into the page. `BACKEND` is replaced by
# the back end's name, or by an empty string for the "auto" leg.
SHIM = """
<script type="module">
/* The CI's smoke-test shim (crates/agnosis-web/tools/force-backend.py).
   It waits for the parade to come up AND for the canvas to carry ink
   — which keeps the load event pending, so the screenshot and the DOM
   dump see the page's final state — and then drives the page's own
   back-end selector, so the switch is the one a user's click makes.

   WHY a MutationObserver and not a timer: a headless run with
   --virtual-time-budget races the clock through every pending timer,
   so a polling shim gives up long before the wasm has booted in real
   time. The observer fires on the page's own status refresh instead,
   which is the very moment the parade is up.

   WHY the ink: the parade draws its first frame on an animation
   frame, and Chrome takes the screenshot when the page has settled.
   Waiting for the canvas to carry something is what makes the
   screenshot show the parade instead of a black rectangle. The probe
   copies the canvas into a 2D canvas, which works for every back end
   (a WebGL canvas hands over its presented frame). */
import { backend_name, set_backend } from './agnosis_web.js';

const want = "BACKEND";

/* The pending request holds the browser's virtual clock. A headless
   run with --virtual-time-budget burns its budget long before the
   wasm has booted in real time, so the screenshot would catch a black
   canvas; a pending fetch pauses the clock instead. The smoke test's
   server answers /wait after the delay (tools/serve.py). */
const hold = fetch('wait?ms=4000').catch(() => null);

const statusEl = document.getElementById('status');
const selectEl = document.getElementById('backend');

/* The page is up when its status line names a back end: the page
   writes "starting" until the wasm has booted. */
function isUp() {
  if (!statusEl) { return false; }
  const text = (statusEl.textContent || '').trim();
  return text !== '' && text.indexOf('starting') !== 0;
}

/* Does the canvas carry anything? -1 when it cannot be read. */
function ink() {
  try {
    const canvas = document.getElementById('canvas');
    if (!canvas || !canvas.width || !canvas.height) { return -1; }
    const probe = document.createElement('canvas');
    probe.width = canvas.width;
    probe.height = canvas.height;
    const ctx = probe.getContext('2d');
    if (!ctx) { return -1; }
    ctx.drawImage(canvas, 0, 0);
    const data = ctx.getImageData(0, 0, probe.width, probe.height).data;
    let count = 0;
    for (let i = 0; i < data.length; i += 4) {
      if (data[i] || data[i + 1] || data[i + 2]) { count += 1; }
    }
    return count;
  } catch (e) {
    return -1;
  }
}

await new Promise((resolve) => {
  let forced = false;
  let ticks = 0;
  const observer = new MutationObserver(() => {
    if (!isUp()) { return; }
    if (!want || forced) {
      /* The parade is up: wait for a frame that carries ink. Two
         status refreshes without any ink mean the canvas cannot be
         read — then the page's own state is all the CI gets. */
      ticks += 1;
      if (ticks > 2 || ink() > 0) {
        observer.disconnect();
        resolve();
        return;
      }
      return;
    }
    forced = true;
    try {
      selectEl.value = want;
      selectEl.dispatchEvent(new Event('change'));
    } catch (e) { /* the page reports it in its diagnostic box */ }
  });
  observer.observe(statusEl, { childList: true, characterData: true, subtree: true });
});

/* The clock only starts running again when the request is answered. */
await hold;
</script>
"""


def main(argv):
    if len(argv) != 4:
        sys.stderr.write("usage: force-backend.py <index.html> <backend> <out.html>\n")
        return 2

    source, backend, destination = argv[1], argv[2], argv[3]

    with open(source, encoding="utf-8") as handle:
        html = handle.read()

    if "<body" not in html:
        sys.stderr.write("force-backend: %s has no <body>\n" % source)
        return 1

    # "auto" means: do not switch at all, only wait for the page.
    shim = SHIM.replace("BACKEND", "" if backend == "auto" else backend)
    patched = html.replace("</body>", shim + "</body>", 1)
    if patched == html:
        sys.stderr.write("force-backend: %s has no </body> to append to\n" % source)
        return 1

    with open(destination, "w", encoding="utf-8") as handle:
        handle.write(patched)

    print("force-backend: %s waits for the page and forces %s" % (destination, backend))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
