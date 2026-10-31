"""Turns measure.py's JSON into Markdown tables of medians: `python summarize.py memory.json`."""

import json
import statistics
import sys

MB = 1e6


def med(values):
    values = [v for v in values if v is not None]
    return statistics.median(values) if values else None


def mb(v):
    return "–" if v is None else f"{v / MB:.0f}"


def main():
    with open(sys.argv[1], encoding="utf-8") as f:
        data = json.load(f)
    clients = {k: v for k, v in data.items() if isinstance(v, dict) and "runs" in v}

    def state(key, field):
        return {c: med([r.get(key, {}).get(field) for r in v["runs"]]) for c, v in clients.items()}

    print("Medians of", ", ".join(f"{len(v['runs'])} runs of {v['name']}" for v in clients.values()) + ".")
    print("Memory in MB (10^6 bytes), summed over each client's process tree.\n")

    print("| Client | Cold start to window, s | Idle, private | Idle, working set | Idle, private working set | Processes |")
    print("|---|---|---|---|---|---|")
    for c, v in clients.items():
        start = med([r.get("start_s") for r in v["runs"]])
        page = med([r.get("start_page_s") for r in v["runs"]])
        s = f"{start:.2f}" + (f" (page ready {page:.2f})" if page else "")
        print(f"| {v['name']} | {s} | {mb(state('idle', 'private')[c])} | "
              f"{mb(state('idle', 'wset')[c])} | {mb(state('idle', 'pwset')[c])} | {state('idle', 'procs')[c]:.0f} |")

    for key, title in (("minimized", "Minimized, after 60 s"), ("tray", "Window closed to the tray, after 60 s")):
        rows = [(c, v) for c, v in clients.items() if any(key in r for r in v["runs"])]
        if not rows:
            continue
        print(f"\n**{title}**\n")
        print("| Client | Private | Working set | Private working set | Processes |")
        print("|---|---|---|---|---|")
        for c, v in rows:
            print(f"| {v['name']} | {mb(state(key, 'private')[c])} | {mb(state(key, 'wset')[c])} | "
                  f"{mb(state(key, 'pwset')[c])} | {state(key, 'procs')[c]:.0f} |")

    for key, title in (("dl_unlimited", "Downloading 1 GiB from the local test server, unlimited"),
                       ("dl_10mib", "Downloading 1 GiB at 10 MiB/s")):
        rows = [(c, v) for c, v in clients.items() if any(key in r for r in v["runs"])]
        if not rows:
            continue
        print(f"\n**{title}**\n")
        print("| Client | Time, s | Private peak | Private average | Working set peak | Working set average | Private WS peak | Private WS average | Processes |")
        print("|---|---|---|---|---|---|---|---|---|")
        for c, v in rows:
            print(f"| {v['name']} | {state(key, 'seconds')[c]:.1f} | {mb(state(key, 'peak_private')[c])} | "
                  f"{mb(state(key, 'avg_private')[c])} | {mb(state(key, 'peak_wset')[c])} | "
                  f"{mb(state(key, 'avg_wset')[c])} | {mb(state(key, 'peak_pwset')[c])} | "
                  f"{mb(state(key, 'avg_pwset')[c])} | {state(key, 'procs')[c]:.0f} |")

    if "shardload_list500" in data:
        runs = data["shardload_list500"]
        print("\n**Shardload with 500 downloads in the list (window open, idle 60 s)**\n")
        print("| Page ready, s | Private | Working set | Private working set | Processes |")
        print("|---|---|---|---|---|")
        print(f"| {med([r['start_page_s'] for r in runs]):.2f} | {mb(med([r['idle']['private'] for r in runs]))} | "
              f"{mb(med([r['idle']['wset'] for r in runs]))} | {mb(med([r['idle'].get('pwset') for r in runs]))} | "
              f"{med([r['idle']['procs'] for r in runs]):.0f} |")


if __name__ == "__main__":
    main()
