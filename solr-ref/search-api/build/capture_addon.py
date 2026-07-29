"""mitmproxy addon: dump every request/response pair that passes through the
reverse proxy in front of the capture-only Solr container, as one JSON file
per flow in /captures. Used to build the search_api_solr HTTP trace for
issue #55 -- this observes real traffic, it does not synthesize it.
"""
import json
import os
import time

COUNTER_FILE = "/captures/_counter"


def _next_seq():
    n = 0
    if os.path.exists(COUNTER_FILE):
        with open(COUNTER_FILE) as f:
            n = int(f.read().strip() or 0)
    n += 1
    with open(COUNTER_FILE, "w") as f:
        f.write(str(n))
    return n


def response(flow):
    seq = _next_seq()
    req = flow.request
    resp = flow.response

    def body_text(msg):
        try:
            return msg.get_text(strict=False)
        except Exception:
            return None

    record = {
        "seq": seq,
        "timestamp": time.time(),
        "request": {
            "method": req.method,
            "path": req.path,  # includes query string, core-relative after host:port
            "headers": dict(req.headers),
            "body": body_text(req),
        },
        "response": {
            "status_code": resp.status_code,
            "headers": dict(resp.headers),
            "body": body_text(resp),
        },
    }
    fname = f"/captures/{seq:05d}.json"
    with open(fname, "w") as f:
        json.dump(record, f, indent=2, sort_keys=True)
        f.write("\n")
