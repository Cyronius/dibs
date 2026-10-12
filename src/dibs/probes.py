import os, re, subprocess, threading, time

from . import core, system

GPU_IGNORE = {"dwm.exe", "explorer.exe", "code.exe", "chrome.exe", "msedge.exe", "firefox.exe", "claude.exe"}


def cpu_pct() -> float:
    # one reading shared by every session's hook, so most calls cost no sleep
    now = system.cpu_sample()
    if now is None:
        return system.cpu_busy(None, None)
    cache = core.read_json(core.PRESSURE)
    cache = cache if isinstance(cache, dict) and isinstance(cache.get("s"), list) else None
    if cache and now[0] - cache["s"][0] < 2:
        return cache["pct"]
    prev = cache["s"] if cache and now[0] - cache["s"][0] <= 60 else None
    if prev is None:
        prev = now
        time.sleep(0.3)
        now = system.cpu_sample()
    pct = system.cpu_busy(prev, now)
    try:
        core.write_json(core.PRESSURE, {"s": now, "pct": pct})
    except OSError:
        pass
    return pct


def pressure(cfg) -> list:
    m, out = cfg["machine"], []
    avail, commit = system.mem()
    cpu = cpu_pct()
    if cpu > m["cpu_max"]:
        out.append(f"CPU {cpu:.0f}% (limit {m['cpu_max']}%)")
    if avail is not None and avail < m["ram_floor_gb"]:
        out.append(f"free RAM {avail:.1f} GB (floor {m['ram_floor_gb']} GB)")
    if commit is not None and commit > m["commit_max"]:
        out.append(f"memory commit {commit:.0f}% (limit {m['commit_max']}%)")
    return out


def _command_users(p: dict, mine: set) -> list:
    try:
        out = subprocess.run(p["command"], shell=isinstance(p["command"], str), capture_output=True, text=True,
                             timeout=p.get("timeout_s", 60)).stdout
    except Exception:
        return []
    ignore = re.compile("|".join(p.get("ignore", [])) or r"$^", re.I)
    parser, found = p.get("parser", "lines"), []
    if parser == "xrt":                              # `xrt-smi examine -r all`: a |pid |ctx| row, then the process name
        lines = out.splitlines()
        for i, ln in enumerate(lines[:-1]):
            m = re.match(r"\s*\|(\d+)\s*\|(\d+)\s*\|", ln)
            if m and int(m.group(1)) not in mine:
                name = lines[i + 1].strip().strip("|").split("|")[0].strip()
                found.append(f"{name}:{m.group(1)} NPU context")
    elif parser == "nvidia":                         # nvidia-smi --query-compute-apps=pid,process_name --format=csv,noheader
        for ln in out.splitlines():
            pid, _, name = ln.partition(",")
            if pid.strip().isdigit() and int(pid) not in mine:
                found.append(f"{os.path.basename(name.strip())}:{pid.strip()}")
    else:
        found = [ln.strip()[:80] for ln in out.splitlines() if ln.strip()]
    return sorted({f for f in found if not ignore.search(f)})


def activity(cfg, names, window: float, quiet_cpu=None) -> list:
    specs = {n: cfg["resource"].get(n, {}) for n in names}
    specs = {n: s for n, s in specs.items() if s.get("probe") or s.get("resident")}
    if not specs and quiet_cpu is None:
        return []
    table = system.procs()
    mine = system.tree(os.getpid(), table) | system.ancestors(table)
    need = any((s.get("probe") or {}).get("kind") == "process" for s in specs.values())
    a, s0 = (system.proc_times() if need else {}), system.cpu_sample()
    time.sleep(window)
    b, s1 = (system.proc_times() if need else {}), system.cpu_sample()
    found = []
    for n, s in specs.items():
        p = s.get("probe") or {}
        kind = p.get("kind") or ("command" if p.get("command") else None)
        if kind == "process":
            rx = re.compile("|".join(p.get("names", [])) or r"$^", re.I)
            for pid, (exe, t) in b.items():
                if pid not in mine and rx.search(exe) and pid in a and t - a[pid][1] > p.get("min_pct", 5) / 100 * window:
                    found.append(f"{n}: {exe}:{pid} {100 * (t - a[pid][1]) / window:.0f}% CPU")
        elif kind == "command":
            found += [f"{n}: {u}" for u in _command_users(p, mine)]
        elif kind == "gpu-counters":
            ign = GPU_IGNORE | {x.lower() for x in p.get("ignore", [])}
            for pid, v in system.gpu_by_pid().items():
                exe = table.get(pid, (0, "?"))[1]
                if v >= p.get("min_pct", 20) and pid not in mine and exe not in ign:
                    found.append(f"{n}: {exe}:{pid} GPU {v:.0f}%")
        elif kind == "net-counters":
            mbps = system.net_mbps()
            if mbps is not None and mbps >= p.get("busy_mbps", 400):
                found.append(f"{n}: link busy at {mbps:.0f} Mbit/s")
        res = {x.lower() for x in s.get("resident", [])}
        found += [f"{n}: {exe}:{pid} resident" for pid, (_, exe) in table.items() if exe in res and pid not in mine]
    if quiet_cpu is not None:
        busy = system.cpu_busy(s0, s1)
        if busy > quiet_cpu:
            found.append(f"total CPU {busy:.0f}% (quiet means under {quiet_cpu}%)")
    return sorted(set(found))


def wait_quiet(cfg, names, quiet_cpu=None) -> None:
    last = 0.0
    while r := activity(cfg, names, 10 if quiet_cpu is not None else 3, quiet_cpu):
        if time.time() - last > 300:
            print(f"dibs: waiting for other work to finish: {'; '.join(r)}", flush=True)
            last = time.time()


class Monitor:
    def __init__(self, cfg, names, every: float = 30.0):
        self.cfg, self.names, self.every = cfg, names, every
        self.found, self.stop = set(), threading.Event()
        self.t = threading.Thread(target=self.run, daemon=True)

    def run(self):
        ignore = {x.lower() for x in self.cfg["measure"]["ignore"]}
        limit = self.cfg["measure"]["noise_core_pct"] / 100 * self.every
        prev = system.proc_times()
        while not self.stop.wait(self.every):
            table = system.procs()
            mine = system.tree(os.getpid(), table) | system.ancestors(table)
            cur = system.proc_times()
            for pid, (exe, t) in cur.items():
                if pid not in mine and exe not in ignore and pid in prev and t - prev[pid][1] > limit:
                    self.found.add(f"{exe}:{pid}")
            self.found.update(activity(self.cfg, self.names, 0.5))
            prev = cur

    def __enter__(self):
        self.t.start()
        return self

    def __exit__(self, *exc):
        self.stop.set()
        self.t.join(timeout=120)
