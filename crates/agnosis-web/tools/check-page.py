#!/usr/bin/env python3
"""Assert that the page came up and reported no failure.

The page collects `error`, `unhandledrejection` and `console.error` into
its diagnostic box (``#diag``) — the element is empty (and invisible)
when nothing went wrong. A wasm build that compiles but fails at
runtime leaves its failure there, so the smoke test asserts that the
box is EMPTY: that is the "the page did not break" half of the check
(the other half is the screenshot, which must not be a black
rectangle).

The page's status line is the other half of "it came up": it reads
"starting" until the wasm has booted, so a page that never booted is a
failure too — and a *silent* one, because its diagnostic box is empty
as well.

    python3 tools/check-page.py page.dom

``page.dom`` is the DOM Chrome dumped (``--dump-dom``). The tool prints
the page's status line — the back end that is running, the grid size —
so the CI log says which back end the leg actually got, and exits 1
when the diagnostic box carries anything or the page never came up.
"""

import html as html_module
import re
import sys

DIAG_PATTERN = re.compile(r'<pre[^>]*\bid="diag"[^>]*>(.*?)</pre>', re.S)
STATUS_PATTERN = re.compile(r'<span[^>]*\bid="status"[^>]*>(.*?)</span>', re.S)


def text_of(pattern, dom):
    """The text of the first element the pattern matches."""
    match = pattern.search(dom)
    if match is None:
        return None
    return html_module.unescape(match.group(1)).strip()


def main(argv):
    if len(argv) != 2:
        sys.stderr.write("usage: check-page.py <dumped-dom.html>\n")
        return 2

    with open(argv[1], encoding="utf-8", errors="replace") as handle:
        dom = handle.read()

    diag = text_of(DIAG_PATTERN, dom)
    if diag is None:
        sys.stderr.write("check-page: the page has no #diag element (the diagnostic box was renamed?)\n")
        return 1

    status = text_of(STATUS_PATTERN, dom)
    if status is None:
        sys.stderr.write("check-page: the page has no #status element (the status line was renamed?)\n")
        return 1

    print("status: %s" % status)

    if diag:
        sys.stderr.write("check-page: the page reported:\n%s\n" % diag)
        return 1

    if not status or status.lower().startswith("starting"):
        sys.stderr.write("check-page: the page never came up (the status line still reads %r)\n" % status)
        return 1

    print("#diag is empty: the page reported no failure")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
