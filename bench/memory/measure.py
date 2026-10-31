"""Memory of download managers, measured from the outside: the sum over each client's whole
process tree (the app and every process it started, e.g. Shardload's WebView2) of private bytes
(commit), working set and private working set (the part of the working set that is the
process's own), and the number of processes.

    python measure.py --clients shardload,idm,abdm,fdm,gopeed --runs 3 --out result.json

For each client and run: a cold start (to a visible window), 60 s idle with the window open,
60 s minimized, a 1 GiB download from the local test server unlimited and at 10 MiB/s (peak and
average while it runs), then 60 s with the window closed to the tray. `--list500` measures
Shardload with a 500-download profile (made by `--make-list500`). Needs psutil; Windows only.
See bench/README.md.
"""

import argparse
import ctypes
import json
import os
import statistics
import subprocess
import sys
import threading
import time
import urllib.request
from ctypes import wintypes

import psutil

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
WORK = os.environ.get("MEMTEST_DIR", r"C:\tmp\memtest")
DL = os.path.join(WORK, "dl")
SERVER = "http://127.0.0.1:8771"
GIB = 1 << 30
# A release build of Shardload; one built with another identifier (see bench/README.md) runs
# next to an installed Shardload.
SHARDLOAD_BIN = os.environ.get("MEMTEST_SHARDLOAD_BIN", os.path.join(REPO, "target", "release"))
IDLE = int(os.environ.get("MEMTEST_IDLE", "60"))

user32 = ctypes.WinDLL("user32", use_last_error=True)
WNDENUMPROC = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
SW_MINIMIZE, SW_RESTORE, WM_CLOSE = 6, 9, 0x0010
GW_OWNER = 4


# ---- Measuring ------------------------------------------------------------------------------

def norm(path):
    return os.path.normcase(os.path.abspath(path)) if path else ""


def tree(spec):
    """The client's processes: those whose image is one of its names, and their descendants."""
    found = {}
    for p in psutil.process_iter(["name", "exe"]):
        try:
            if (p.info["name"] or "").lower() in spec["names"]:
                if spec.get("exe") and norm(p.info["exe"] or "") != norm(spec["exe"]):
                    continue
                found[p.pid] = p
                for c in p.children(recursive=True):
                    found[c.pid] = c
        except psutil.Error:
            pass
    return list(found.values())


class COUNTERS(ctypes.Structure):
    """PROCESS_MEMORY_COUNTERS_EX2: also the private working set (Windows 10 1809 and later)."""
    _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD)] + [
        (n, ctypes.c_size_t) for n in (
            "PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage",
            "QuotaPagedPoolUsage", "QuotaPeakNonPagedPoolUsage", "QuotaNonPagedPoolUsage",
            "PagefileUsage", "PeakPagefileUsage", "PrivateUsage", "PrivateWorkingSetSize",
            "SharedCommitUsage")]


kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
psapi = ctypes.WinDLL("psapi", use_last_error=True)
kernel32.OpenProcess.restype = wintypes.HANDLE


def private_wset(pid):
    """Bytes of the process's working set that are its own, not shared."""
    h = kernel32.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
    if not h:
        return 0
    try:
        c = COUNTERS()
        c.cb = ctypes.sizeof(c)
        if not psapi.GetProcessMemoryInfo(h, ctypes.byref(c), c.cb):
            return 0
        return c.PrivateWorkingSetSize
    finally:
        kernel32.CloseHandle(h)


def sample(spec):
    private = wset = pwset = n = 0
    for p in tree(spec):
        try:
            m = p.memory_info()
            private += m.private
            wset += m.rss
            pwset += private_wset(p.pid)
            n += 1
        except psutil.Error:
            pass
    return {"private": private, "wset": wset, "pwset": pwset, "procs": n}


def settle(spec, seconds):
    """Waits `seconds`, then the median of samples over the last 10 of them."""
    time.sleep(max(0, seconds - 10))
    got = []
    for _ in range(10):
        got.append(sample(spec))
        time.sleep(1)
    return {k: statistics.median(s[k] for s in got) for k in ("private", "wset", "pwset", "procs")}


class Recorder:
    """Samples four times a second in the background: peak and average."""

    def __init__(self, spec):
        self.spec, self.samples, self.stop = spec, [], threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def run(self):
        while not self.stop.is_set():
            self.samples.append(sample(self.spec))
            time.sleep(0.25)

    def finish(self):
        self.stop.set()
        self.thread.join()
        s = self.samples or [sample(self.spec)]
        return {
            "peak_private": max(x["private"] for x in s),
            "avg_private": statistics.mean(x["private"] for x in s),
            "peak_wset": max(x["wset"] for x in s),
            "avg_wset": statistics.mean(x["wset"] for x in s),
            "peak_pwset": max(x["pwset"] for x in s),
            "avg_pwset": statistics.mean(x["pwset"] for x in s),
            "procs": max(x["procs"] for x in s),
        }


# ---- Windows --------------------------------------------------------------------------------

def windows(spec):
    """Visible, unowned top-level windows with a title, of the client's processes."""
    pids = {p.pid for p in tree(spec)}
    out = []

    def each(h, _):
        pid = wintypes.DWORD()
        user32.GetWindowThreadProcessId(h, ctypes.byref(pid))
        if pid.value in pids and user32.IsWindowVisible(h) and not user32.GetWindow(h, GW_OWNER):
            n = user32.GetWindowTextLengthW(h)
            if n:
                buf = ctypes.create_unicode_buffer(n + 1)
                user32.GetWindowTextW(h, buf, n + 1)
                out.append((h, buf.value))
        return True

    user32.EnumWindows(WNDENUMPROC(each), 0)
    return out


def main_window(spec):
    ws = windows(spec)
    title = (spec.get("title") or "").lower()
    exact = [h for h, t in ws if t.lower() == title]
    near = [h for h, t in ws if title in t.lower() and not t.lower().endswith("-siw")]
    return (exact or near or [None])[0]


def wait(check, what, timeout):
    end = time.time() + timeout
    while time.time() < end:
        v = check()
        if v:
            return v
        time.sleep(0.05)
    raise TimeoutError(what)


# ---- Clients --------------------------------------------------------------------------------

STARTED = time.time()


def ours(spec):
    """Whether every running process of the client was started after this script: a client the
    user already had running is never stopped."""
    try:
        return all(p.create_time() >= STARTED - 1 for p in tree(spec))
    except psutil.Error:
        return True


def kill(spec):
    if not ours(spec):
        raise RuntimeError(f"{spec['name']} was already running; close it first")
    for p in tree(spec):
        try:
            p.kill()
        except psutil.Error:
            pass
    wait(lambda: not tree(spec), "the client to stop", 30)
    time.sleep(1)


class Mcp:
    """Shardload's MCP server over stdio."""

    def __init__(self, env):
        exe = os.path.join(SHARDLOAD_BIN, "shardload-mcp.exe")
        self.p = subprocess.Popen([exe], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, text=True, encoding="utf-8")
        self.id = 0
        self.call("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                 "clientInfo": {"name": "memtest", "version": "1"}})
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
        self.p.stdin.flush()

    def call(self, method, params):
        self.id += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.id, "method": method,
                                       "params": params}) + "\n")
        self.p.stdin.flush()
        while True:
            m = json.loads(self.p.stdout.readline())
            if m.get("id") == self.id:
                return m

    def tool(self, name, args):
        r = self.call("tools/call", {"name": name, "arguments": args})
        text = r["result"]["content"][0]["text"]
        if r["result"].get("isError"):
            raise RuntimeError(f"{name}: {text}")
        return json.loads(text)

    def close(self):
        self.p.kill()


def tasks(mcp):
    got = mcp.tool("list_downloads", {})
    return got["downloads"] if isinstance(got, dict) else got


def shardload_spec(home):
    exe = os.path.join(SHARDLOAD_BIN, "shardload.exe")
    # With its browser host next to it, a profile registers that host with the browsers in place
    # of the installed app's; without it the app only logs that it could not.
    if os.path.exists(os.path.join(SHARDLOAD_BIN, "shardload-host.exe")):
        sys.exit(f"{SHARDLOAD_BIN} has shardload-host.exe: use a build without it")
    env = dict(os.environ, SHARDLOAD_HOME=home)

    def start():
        subprocess.Popen([exe], env=env)

    def download(url, name):
        mcp = Mcp(env)
        try:
            t = mcp.tool("add_download", {"url": url, "name": name, "allow_duplicate": True})
            while True:
                task = next(x for x in tasks(mcp) if x["id"] == t["id"])
                if task["status"] == "done":
                    return task["path"]
                if task["status"] == "error":
                    raise RuntimeError(task.get("error"))
                time.sleep(0.2)
        finally:
            mcp.close()

    def page_ready():
        logs = os.path.join(home, "logs")
        if not os.path.isdir(logs):
            return False
        return any("the window's page is ready" in open(os.path.join(logs, f), encoding="utf-8",
                                                          errors="replace").read()
                   for f in os.listdir(logs))

    return {"name": "Shardload", "names": {"shardload.exe"}, "exe": exe, "title": "Shardload",
            "start": start, "download": download, "ready_extra": page_ready, "env": env,
            "dl_dir": os.path.join(home, "downloads"), "show": start, "logs": os.path.join(home, "logs")}


IDM_EXE = r"C:\Program Files (x86)\Internet Download Manager\IDMan.exe"


def idm_spec():
    def start():
        subprocess.Popen([IDM_EXE])

    def download(url, name):
        path = os.path.join(DL, name)
        subprocess.Popen([IDM_EXE, "/d", url, "/p", DL, "/f", name, "/n"])
        return wait_file(path)

    return {"name": "IDM", "names": {"idman.exe", "iemonitor.exe"}, "title": "Internet Download Manager",
            "start": start, "download": download, "show": start}


ABDM_DIR = os.path.join(os.environ["LOCALAPPDATA"], "ABDownloadManager")


def abdm_spec():
    sys.path.insert(0, os.path.join(REPO, "bench", "clients"))
    import abdm  # noqa: E402

    def start():
        subprocess.Popen([os.path.join(ABDM_DIR, "ABDownloadManager.exe")])

    def download(url, name):
        out = subprocess.run([abdm.CLI, "download", "add", "http", "--link", url, "--folder", DL,
                              "--name", name, "--start"], capture_output=True, text=True,
                             check=True).stdout.split()
        record = os.path.join(abdm.RECORDS, f"{out[-1]}.json")
        while True:
            try:
                d = json.loads(abdm.read_shared(record))
            except (OSError, ValueError):
                d = {}
            if d.get("status") == "Completed":
                return os.path.join(DL, name)
            if d.get("status") in ("Error", "Canceled"):
                raise RuntimeError("AB DM: " + d["status"])
            time.sleep(0.2)

    return {"name": "AB DM", "names": {"abdownloadmanager.exe"}, "title": "AB Download Manager",
            "start": start, "download": download, "show": start}


FDM_EXE = os.path.join(os.environ["LOCALAPPDATA"], "Programs", "Softdeluxe", "Free Download Manager", "fdm.exe")


def fdm_spec():
    def start():
        subprocess.Popen([FDM_EXE])

    return {"name": "FDM", "names": {"fdm.exe"}, "title": "Free Download Manager", "start": start,
            "download": None, "show": start}


GOPEED_EXE = os.path.join(os.environ["LOCALAPPDATA"], "Microsoft", "WinGet", "Packages",
                          "monkeyWie.Gopeed.Web_Microsoft.Winget.Source_8wekyb3d8bbwe",
                          "gopeed-web-v1.9.3-windows-amd64", "gopeed.exe")


def gopeed_spec():
    api = "http://127.0.0.1:9997"
    data = os.path.join(WORK, "gopeed-data")

    def call(method, path, body=None):
        req = urllib.request.Request(api + path, method=method,
                                     data=json.dumps(body).encode() if body is not None else None,
                                     headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=30) as r:
            return json.load(r)["data"]

    def start():
        os.makedirs(data, exist_ok=True)
        subprocess.Popen([GOPEED_EXE, "-A", "127.0.0.1", "-P", "9997", "-d", data, "-w", DL],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def ready():
        try:
            call("GET", "/api/v1/tasks")
            return True
        except OSError:
            return False

    def download(url, name):
        task = call("POST", "/api/v1/tasks", {"req": {"url": url}, "opts": {"name": name, "path": DL}})
        while call("GET", f"/api/v1/tasks/{task}")["status"] != "done":
            time.sleep(0.2)
        return os.path.join(DL, name)

    return {"name": "Gopeed (web server, no UI)", "names": {"gopeed.exe"}, "exe": GOPEED_EXE,
            "start": start, "download": download, "headless_ready": ready}


def wait_file(path):
    """IDM writes the file under its name only when it has joined the parts."""
    def whole():
        try:
            return os.path.getsize(path) == GIB
        except OSError:
            return False
    wait(whole, "the download", 1800)
    time.sleep(1)
    return path


# ---- One run --------------------------------------------------------------------------------

def fresh_start(spec):
    """Starts the client and returns when it started; earlier logs are cleared first, so a
    page-ready line is this start's."""
    kill(spec)
    if spec.get("logs"):
        for f in os.listdir(spec["logs"]) if os.path.isdir(spec["logs"]) else []:
            os.remove(os.path.join(spec["logs"], f))
    t0 = time.time()
    spec["start"]()
    return t0


def run_once(spec, run):
    r = {}
    t0 = fresh_start(spec)
    if spec.get("headless_ready"):
        wait(spec["headless_ready"], "the server", 60)
    else:
        wait(lambda: main_window(spec), "the window", 120)
    r["start_s"] = time.time() - t0
    if spec.get("ready_extra"):
        wait(spec["ready_extra"], "the page", 120)
        r["start_page_s"] = time.time() - t0
    r["idle"] = settle(spec, IDLE)
    windowed = not spec.get("headless_ready")
    if windowed:
        h = main_window(spec)
        user32.ShowWindow(h, SW_MINIMIZE)
        r["minimized"] = settle(spec, IDLE)
        user32.ShowWindow(h, SW_RESTORE)
        time.sleep(3)
    if spec.get("download"):
        for label, query in (("dl_unlimited", ""), ("dl_10mib", "&total_rate=10M")):
            name = f"mem-{spec['name'][:3].strip().lower()}-{run}-{label}.bin"
            # A new address each time: the test server keeps a rate budget per address, and one
            # asked for again after a pause starts with what it saved up.
            url = f"{SERVER}/file/{name}?size=1G&seed={run}&t={time.time_ns()}{query}"
            rec = Recorder(spec)
            t = time.time()
            path = spec["download"](url, name)
            r[label] = rec.finish()
            r[label]["seconds"] = time.time() - t
            try:
                os.remove(path)
            except OSError:
                pass
            time.sleep(5)
    if windowed:
        h = main_window(spec)
        if h:
            user32.PostMessageW(h, WM_CLOSE, 0, 0)
        time.sleep(2)
        r["tray"] = settle(spec, IDLE)
        r["tray"]["still_running"] = bool(tree(spec))
    kill(spec)
    return r


def on_top(h):
    """Keeps the window above the others without taking the focus: a covered WebView2 window
    trims its memory, so idle with the window really in view is measured this way."""
    HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, SWP_NOACTIVATE = -1, 0x2, 0x1, 0x10
    user32.SetWindowPos(wintypes.HWND(h), wintypes.HWND(HWND_TOPMOST), 0, 0, 0, 0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE)


def visible_idle(spec, runs):
    """Cold start, then 60 s idle with the window on top."""
    out = []
    for _ in range(runs):
        t0 = fresh_start(spec)
        h = wait(lambda: main_window(spec), "the window", 120)
        r = {"start_s": time.time() - t0}
        if spec.get("ready_extra"):
            wait(spec["ready_extra"], "the page", 120)
            r["start_page_s"] = time.time() - t0
        on_top(h)
        r["idle"] = settle(spec, IDLE)
        kill(spec)
        out.append(r)
        print(json.dumps(r), flush=True)
    return out


def list500(home, runs):
    spec = shardload_spec(home)
    out = []
    for _ in range(runs):
        t0 = fresh_start(spec)
        wait(lambda: main_window(spec), "the window", 120)
        wait(spec["ready_extra"], "the page", 120)
        r = {"start_page_s": time.time() - t0, "idle": settle(spec, IDLE)}
        kill(spec)
        out.append(r)
    return out


def make_list500(home):
    """450 finished small downloads and 50 paused 100 MiB ones, added through MCP."""
    spec = shardload_spec(home)
    os.makedirs(home, exist_ok=True)
    spec["start"]()
    wait(lambda: main_window(spec), "the window", 120)
    wait(spec["ready_extra"], "the page", 120)
    mcp = Mcp(spec["env"])
    for i in range(450):
        mcp.tool("add_download", {"url": f"{SERVER}/file/item-{i:03}.zip?size=16K&seed={i}",
                                  "allow_duplicate": True})
    for i in range(50):
        mcp.tool("add_download", {"url": f"{SERVER}/file/big-{i:02}.iso?size=100M&seed={i}",
                                  "start": False, "allow_duplicate": True})
    wait(lambda: sum(t["status"] == "done" for t in tasks(mcp)) >= 450, "the small downloads", 600)
    print(len(tasks(mcp)), "downloads in the list")
    mcp.close()
    kill(spec)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--clients", default="shardload,idm,abdm,fdm,gopeed")
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--out", default=os.path.join(WORK, "memory.json"))
    ap.add_argument("--list500", help="a Shardload profile with 500 downloads")
    ap.add_argument("--make-list500", help="make that profile")
    ap.add_argument("--visible", action="store_true",
                    help="only cold start and idle with the window kept on top")
    a = ap.parse_args()
    os.makedirs(DL, exist_ok=True)
    if a.make_list500:
        make_list500(a.make_list500)
        return
    results = {}
    if os.path.exists(a.out):
        with open(a.out, encoding="utf-8") as f:
            results = json.load(f)
    specs = {"shardload": lambda: shardload_spec(os.path.join(WORK, "slprofile")),
             "idm": idm_spec, "abdm": abdm_spec, "fdm": fdm_spec, "gopeed": gopeed_spec}
    if a.visible:
        if a.list500:
            results["visible_shardload_list500"] = visible_idle(shardload_spec(a.list500), a.runs)
        for key in [c for c in a.clients.split(",") if c]:
            spec = specs[key]()
            if not ours(spec):
                print(f"skipping {spec['name']}: it was already running", flush=True)
                continue
            results[f"visible_{key}"] = {"name": spec["name"], "runs": visible_idle(spec, a.runs)}
        with open(a.out, "w", encoding="utf-8") as f:
            json.dump(results, f, indent=1)
        return
    if a.list500:
        results["shardload_list500"] = list500(a.list500, a.runs)
    for key in [c for c in a.clients.split(",") if c]:
        spec = specs[key]()
        if not ours(spec):
            print(f"skipping {spec['name']}: it was already running; close it first", flush=True)
            continue
        runs = []
        for run in range(a.runs):
            print(f"{spec['name']} run {run + 1}", flush=True)
            runs.append(run_once(spec, run))
            print(json.dumps(runs[-1]), flush=True)
        results[key] = {"name": spec["name"], "runs": runs}
        with open(a.out, "w", encoding="utf-8") as f:
            json.dump(results, f, indent=1)
    with open(a.out, "w", encoding="utf-8") as f:
        json.dump(results, f, indent=1)


if __name__ == "__main__":
    main()
