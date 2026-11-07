"""Drives Camoufox (through its Playwright integration) for Craft Anything.

Started as `<python> -I camoufox_helper.py` with one JSON request per stdin line and
one JSON response per stdout line. Requests carry an `id` the response repeats. A reply
payload over 1 MiB is written to `<work_dir>/resp-<id>.json` and the reply names it
(`"_file"`); screenshots are always files (`"file"`). Everything that can grow —
requests, payloads, screenshots — travels in lines only when small, and in the work
folder when not.

Ops:
  {"id": N, "op": "open", "headless": true, "width": W, "height": H, "os": "macos"}
      -> {"product": "Camoufox", "version": "...", "user_agent": "..."}
  {"id": N, "op": "goto", "url": "...", "settle_ms": MS}
      -> {"loaded": true, "events": [{"method": "...", "params": {...}}, ...]}
  {"id": N, "op": "eval", "script": "..."}        -> {"value": ...}
  {"id": N, "op": "screenshot", "mode": "full_page" | "viewport"}
      -> {"file": "resp-<id>.png"}  (the PNG's bytes in the work folder)
  {"id": N, "op": "viewport", "width": W, "height": H} -> {}
  {"id": N, "op": "key", "key": "Escape"}         -> {}
  {"id": N, "op": "sleep", "ms": MS}              -> {}
  {"id": N, "op": "close"}                        -> {}

The events of a `goto` are the browser's own, shaped like the DevTools events the rest
of Craft Anything reads (page health, failed requests), so the same reader sees the
same facts whichever browser produced them. Never read from the page beyond the fixed
scripts the caller sends: no cookies, storage or form values are touched here.
"""

import json
import struct
import sys
import traceback

# A JSON reply bigger than this arrives as a file instead of in the line.
PAYLOAD_FILE_MIN = 1 << 20
# Most events kept for one page load (a busy page sends thousands).
MAX_EVENTS = 20_000
# Biggest screenshot the caller will read back.
MAX_SCREENSHOT_BYTES = 64 << 20
# How long a page load may take before it is reported as not loaded, in ms.
GOTO_TIMEOUT_MS = 45_000

# Playwright's resource types mapped to the DevTools names the readers expect.
RESOURCE_TYPES = {
    "document": "Document",
    "stylesheet": "Stylesheet",
    "image": "Image",
    "media": "Media",
    "font": "Font",
    "script": "Script",
    "texttrack": "TextTrack",
    "xhr": "XHR",
    "fetch": "Fetch",
    "preflight": "Preflight",
    "eventsource": "EventSource",
    "websocket": "WebSocket",
    "manifest": "Manifest",
    "other": "Other",
}


class Helper:
    def __init__(self, work_dir):
        self.work_dir = work_dir
        self.next_file = 0
        self.browser = None
        self.context = None
        self.page = None
        self.events = []
        self.request_ids = {}
        self.next_request_id = 0

    # ---- replies ---------------------------------------------------------------------------

    def reply(self, rid, ok=None, error=None):
        if error is not None:
            out = {"id": rid, "error": {"message": str(error)[:2000]}}
        else:
            payload = ok if ok is not None else {}
            text = json.dumps(payload, default=str)
            if len(text.encode("utf-8", "replace")) > PAYLOAD_FILE_MIN:
                name = self.store_bytes("resp-%d.json" % rid, text.encode("utf-8"))
                out = {"id": rid, "ok": {"_file": name}}
            else:
                out = {"id": rid, "ok": payload}
        sys.stdout.buffer.write(json.dumps(out, default=str).encode("utf-8") + b"\n")
        sys.stdout.buffer.flush()

    def store_bytes(self, name, data):
        import os
        path = os.path.join(self.work_dir, name)
        with open(path, "wb") as f:
            f.write(data)
        return name

    # ---- events ----------------------------------------------------------------------------

    def reset_events(self):
        self.events = []
        self.request_ids = {}

    def push_event(self, method, params):
        if len(self.events) < MAX_EVENTS:
            self.events.append({"method": method, "params": params})

    def attach_events(self):
        page = self.page

        def on_request(req):
            self.next_request_id += 1
            rid = str(self.next_request_id)
            # Bound the mapping: only live requests of this load are interesting.
            if len(self.request_ids) < MAX_EVENTS:
                self.request_ids[id(req)] = rid
            self.push_event("Network.requestWillBeSent", {"requestId": rid, "request": {"url": req.url}})

        def on_response(resp):
            rid = self.request_ids.get(id(resp.request), "")
            req = resp.request
            self.push_event(
                "Network.responseReceived",
                {
                    "requestId": rid,
                    "type": RESOURCE_TYPES.get(req.resource_type, "Other"),
                    "response": {"url": resp.url, "status": resp.status},
                },
            )

        def on_failed(req):
            rid = self.request_ids.get(id(req), "")
            try:
                why = req.failure or "load failed"
            except Exception:
                why = "load failed"
            self.push_event(
                "Network.loadingFailed",
                {
                    "requestId": rid,
                    "type": RESOURCE_TYPES.get(req.resource_type, "Other"),
                    "errorText": str(why)[:300],
                },
            )

        def on_console(msg):
            if msg.type in ("error", "assert"):
                self.push_event(
                    "Runtime.consoleAPICalled",
                    {"type": msg.type, "args": [{"value": str(msg.text)[:2000]}]},
                )

        def on_page_error(err):
            self.push_event(
                "Runtime.exceptionThrown",
                {"exceptionDetails": {"exception": {"description": str(err)[:2000]}, "url": ""}},
            )

        page.on("request", on_request)
        page.on("response", on_response)
        page.on("requestfailed", on_failed)
        page.on("console", on_console)
        page.on("pageerror", on_page_error)

    # ---- ops -------------------------------------------------------------------------------

    def op_open(self, req):
        from camoufox import Camoufox

        os_name = req.get("os") or None
        headless = bool(req.get("headless", True))
        width = int(req.get("width") or 1280)
        height = int(req.get("height") or 900)
        self.camoufox = Camoufox(headless=headless, os=os_name, humanize=True, geoip=False)
        self.browser = self.camoufox.__enter__()
        self.context = self.browser.new_context(
            viewport={"width": width, "height": height}, color_scheme="light"
        )
        self.page = self.context.new_page()
        self.attach_events()
        ua = self.page.evaluate("navigator.userAgent")
        return {"product": "Camoufox", "version": str(self.browser.version), "user_agent": str(ua)}

    def op_goto(self, req):
        from playwright.sync_api import Error as PwError
        from playwright.sync_api import TimeoutError as PwTimeout

        self.reset_events()
        url = str(req.get("url") or "")
        settle_ms = max(0, min(int(req.get("settle_ms") or 0), 30_000))
        loaded = True
        try:
            self.page.goto(url, wait_until="load", timeout=GOTO_TIMEOUT_MS)
        except PwTimeout:
            loaded = False
        except PwError as e:
            # The page didn't load at all; say so in the page's own words.
            raise RuntimeError("the page didn't load: %s" % str(e)[:300])
        if settle_ms:
            self.page.wait_for_timeout(settle_ms)
        return {"loaded": loaded, "events": self.events}

    def op_eval(self, req):
        value = self.page.evaluate(str(req.get("script") or ""))
        return {"value": value}

    def op_screenshot(self, req):
        mode = str(req.get("mode") or "viewport")
        if mode == "full_page":
            data = self.page.screenshot(type="png", full_page=True)
        else:
            data = self.page.screenshot(type="png")
        if len(data) > MAX_SCREENSHOT_BYTES:
            raise RuntimeError("the screenshot is over %d bytes" % MAX_SCREENSHOT_BYTES)
        self.next_file += 1
        name = self.store_bytes("shot-%d.png" % self.next_file, data)
        w, h = struct.unpack(">II", data[16:24])
        return {"file": name, "width": w, "height": h}

    def op_viewport(self, req):
        width = max(1, int(req.get("width") or 1280))
        height = max(1, int(req.get("height") or 900))
        self.page.set_viewport_size({"width": width, "height": height})
        self.page.emulate_media(color_scheme="light")
        return {}

    def op_key(self, req):
        self.page.keyboard.press(str(req.get("key") or "Escape"))
        return {}

    def op_sleep(self, req):
        ms = max(0, min(int(req.get("ms") or 0), 60_000))
        if ms:
            self.page.wait_for_timeout(ms)
        return {}

    def op_close(self, req):
        self.close()
        return {}

    def close(self):
        for closer in (
            lambda: self.context and self.context.close(),
            lambda: self.browser and self.browser.close(),
            lambda: self.camoufox and self.camoufox.__exit__(None, None, None),
        ):
            try:
                closer()
            except Exception:
                pass
        self.context = None
        self.browser = None
        self.camoufox = None

    # ---- dispatch --------------------------------------------------------------------------

    def handle(self, req):
        op = str(req.get("op") or "")
        fn = getattr(self, "op_" + op, None)
        if fn is None:
            raise RuntimeError("unknown op: %s" % op[:50])
        return fn(req)


def main():
    work_dir = None
    for arg in sys.argv[1:]:
        if not arg.startswith("-"):
            work_dir = arg
    if not work_dir:
        sys.stderr.write("camoufox_helper: no work folder given\n")
        return 2
    helper = Helper(work_dir)
    try:
        for raw in sys.stdin.buffer:
            line = raw.decode("utf-8", "replace").strip()
            if not line:
                continue
            try:
                req = json.loads(line)
            except Exception:
                continue
            rid = req.get("id")
            try:
                if req.get("op") == "open":
                    import camoufox  # noqa: F401  (clear error before the browser starts)
                helper.reply(rid, ok=helper.handle(req))
                if req.get("op") == "close":
                    break
            except Exception as e:
                traceback.print_exc(file=sys.stderr)
                helper.reply(rid, error=e)
    finally:
        helper.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
