"""Downloads one URL with Gopeed and exits when it is done: `gopeed.py URL PATH [API]`.

Gopeed has no command that waits for a download, so this adds the task through its REST API
and polls it. The server must already run (`gopeed -A 127.0.0.1 -P 9999 -w <out dir>`, see
bench/README.md); its start-up is not measured, as Gopeed would already be open in use. `API` (default
`GOPEED_API` or http://127.0.0.1:9999) picks the server, so two versions can be compared.
"""

import json
import os
import sys
import time
import urllib.request

API = os.environ.get("GOPEED_API", "http://127.0.0.1:9999")


def call(method, path, body=None):
    req = urllib.request.Request(
        API + path,
        method=method,
        data=json.dumps(body).encode() if body is not None else None,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=30) as r:
        reply = json.load(r)
    if reply.get("code") != 0:
        raise RuntimeError(reply.get("msg"))
    return reply["data"]


def main():
    global API
    url, path = sys.argv[1], os.path.abspath(sys.argv[2])
    if len(sys.argv) > 3:
        API = sys.argv[3]
    folder, name = os.path.split(path)
    task = call("POST", "/api/v1/tasks", {"req": {"url": url}, "opts": {"name": name, "path": folder}})
    while True:
        status = call("GET", f"/api/v1/tasks/{task}")["status"]
        if status == "done":
            call("DELETE", f"/api/v1/tasks/{task}?force=false")
            return 0
        if status == "error":
            print("gopeed: download failed", file=sys.stderr)
            return 1
        time.sleep(0.02)


if __name__ == "__main__":
    sys.exit(main())
