"""Downloads one URL with AB Download Manager and exits when it is done: `abdm.py URL PATH`.

AB DM's command-line tool adds and starts a download in the running app but does not wait,
so this adds it and then watches the app's record of it (`~/.abdm/config/download_db`). The
tool itself takes about a second to start; the record's `dateAdded` and `completeTime` give
AB DM's own time without it, printed as `abdm-ms <milliseconds>`.
"""

import json
import os
import subprocess
import sys
import time

CLI = os.environ.get(
    "ABDM_CLI",
    os.path.join(os.environ.get("LOCALAPPDATA", ""), "ABDownloadManager", "ABDownloadManagerCli.exe"),
)
RECORDS = os.path.join(os.path.expanduser("~"), ".abdm", "config", "download_db", "downloadlist")


def read_shared(path):
    """The file's text, opened so the app can still replace it meanwhile. AB DM saves a record
    by moving `<id>.json.tmp` over `<id>.json`; a plain `open()` on Windows makes that move
    fail with AccessDenied, and AB DM shows an error window."""
    if os.name != "nt":
        with open(path, encoding="utf-8") as f:
            return f.read()
    import ctypes
    from ctypes import wintypes

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32.CreateFileW.restype = wintypes.HANDLE
    k32.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, wintypes.LPVOID,
                                wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
    GENERIC_READ, SHARE_ALL, OPEN_EXISTING = 0x80000000, 0x7, 3
    handle = k32.CreateFileW(path, GENERIC_READ, SHARE_ALL, None, OPEN_EXISTING, 0, None)
    if handle in (None, wintypes.HANDLE(-1).value):
        raise OSError(ctypes.get_last_error(), "CreateFileW", path)
    try:
        chunks = []
        buf = ctypes.create_string_buffer(65536)
        got = wintypes.DWORD()
        while k32.ReadFile(handle, buf, len(buf), ctypes.byref(got), None) and got.value:
            chunks.append(buf.raw[: got.value])
        return b"".join(chunks).decode("utf-8")
    finally:
        k32.CloseHandle(handle)


def main():
    url, path = sys.argv[1], os.path.abspath(sys.argv[2])
    folder, name = os.path.split(path)
    out = subprocess.run(
        [CLI, "download", "add", "http", "--link", url, "--folder", folder, "--name", name, "--start"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.split()
    record = os.path.join(RECORDS, f"{out[-1]}.json")
    while True:
        try:
            d = json.loads(read_shared(record))
        except (OSError, ValueError):
            d = {}
        status = d.get("status")
        if status == "Completed":
            print(f"abdm-ms {d['completeTime'] - d['dateAdded']}")
            return 0
        if status in ("Error", "Canceled"):
            print(f"abdm: download {status.lower()}", file=sys.stderr)
            return 1
        time.sleep(0.02)


if __name__ == "__main__":
    sys.exit(main())
