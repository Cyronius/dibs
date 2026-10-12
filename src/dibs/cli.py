import os, shutil, subprocess, sys, time
from pathlib import Path

from . import __version__, core, locks, probes, system
from .core import MACHINE, Problem, describe, hhmm

USAGE = f"""dibs {__version__}: take turns with shared resources on this machine.

  dibs run <res ...> [--label <what>] [--tag <worker>] [--eta 40m] [--new] -- <cmd ...>
        run <cmd> holding the resources; they free when it exits
  dibs run --measure [<res ...>] [--eta 30m] -- <cmd ...>
        take the whole machine, wait until it's quiet, run <cmd>, print CLEAN or NOISY
  dibs hold <res ...> | --measure  --eta 45m [--label <what>]     keep resources across several commands
  dibs release <res ...> | --measure | --all [--force]           end a hold (--force: someone else's)
  dibs extend <res ...> --eta 20m                                push out your hold's ETA
  dibs wait <res ...> | --measure | --pressure | --disk <GB>     block until free, print one FREE line
  dibs status [--activity]                                       who holds what, until when, who waits
  dibs disk claim <GB> --eta 2h [--path <dir>] | check <GB> | release [--all]
  dibs doctor                                                    check the setup
  dibs nice [--all]                                              lower Claude Code's CPU priority

A resource is any name: gpu, npu, network, testdb, port-3000. Your session's name is recorded for you.
Config (optional): {core.USER_CONFIG} and .dibs.toml in a repo.
"""
VALUE_FLAGS = {"--label", "--tag", "--eta", "--path", "--disk"}


def opt(a: list, name: str, default=None):
    return a[a.index(name) + 1] if name in a and a.index(name) + 1 < len(a) else default


def positional(a: list) -> list:
    out, skip = [], False
    for x in a:
        if skip:
            skip = False
        elif x in VALUE_FLAGS:
            skip = True
        elif not x.startswith("-"):
            out.append(x)
    return out


def me_from(a: list) -> dict:
    return core.who(opt(a, "--label"), opt(a, "--tag"))


def eta_from(a: list, required=False):
    if opt(a, "--eta"):
        return core.parse_dur(opt(a, "--eta"))
    if required:
        raise Problem("a hold needs --eta, e.g. --eta 40m")
    return None


def targets(a: list, cfg) -> list:
    names = core.resolve(positional(a), cfg, "--new" in a)
    if "--measure" in a:
        every = sorted(set(names) | set(cfg["resource"]) | {MACHINE})
        return [(n, core.capacity(cfg, n), True) for n in every]
    if not names:
        raise Problem("name at least one resource, e.g. `dibs run gpu -- python train.py`")
    return [(n, core.capacity(cfg, n), False) for n in names]


def wait_for_machine() -> None:
    last = 0.0
    while b := locks.blocking(MACHINE, 1):
        if time.time() - last > 300:
            print(f"dibs: waiting, the machine is reserved for a measurement by {describe(b[0])}", flush=True)
            last = time.time()
        time.sleep(5)


def resolve_exe(cmd: list) -> list:
    # Windows won't find npm.cmd or a venv's script from a bare name without a PATHEXT lookup
    return [shutil.which(cmd[0]) or cmd[0], *cmd[1:]] if system.WINDOWS else cmd


def cmd_run(a: list) -> int:
    if "--" not in a or a.index("--") == len(a) - 1:
        raise Problem("put the command after --, e.g. `dibs run gpu -- python train.py`")
    pre, cmd = a[:a.index("--")], a[a.index("--") + 1:]
    cfg = core.load_config()
    measure = "--measure" in pre
    tg, me, eta = targets(pre, cfg), me_from(pre), eta_from(pre)
    names = [n for n, _, _ in tg if n != MACHINE]
    quiet = cfg["machine"]["quiet_cpu_max"] if measure else None
    held = []
    try:
        while True:
            if not measure:
                wait_for_machine()
            probes.wait_quiet(cfg, names, quiet)
            for n, cap, ex in tg:
                held += locks.acquire(n, me, cmd, cap, eta, kind="measure" if measure else "run", exclusive=ex)
            r = probes.activity(cfg, names, 10 if measure else 3, quiet)
            if not r:
                break
            print(f"dibs: other work started meanwhile ({'; '.join(r)}); backing off", flush=True)
            locks.release(held)
            held = []
        if not measure:
            return subprocess.run(resolve_exe(cmd)).returncode
        print("dibs: CLEAN", flush=True)
        with probes.Monitor(cfg, names) as mon:
            rc = subprocess.run(resolve_exe(cmd), creationflags=system.normal_priority_flags()).returncode
        r = sorted(set(probes.activity(cfg, names, 10, quiet)) | mon.found)
        print("dibs: CLEAN after" if not r else f"dibs: NOISY after {r}", flush=True)
        return rc
    finally:
        locks.release(held)


def cmd_hold(a: list) -> int:
    cfg = core.load_config()
    tg, me, eta = targets(a, cfg), me_from(a), eta_from(a, required=True)
    have = {h.get("resource") for h in locks.session_holds(me["session"])}
    if have and not have >= {n for n, _, _ in tg}:
        print(f"dibs: warning: this session already holds {sorted(have)} and now asks for more. Take a hold's "
              "whole set at once, or two sessions can deadlock until an ETA passes.", flush=True)
    for n, cap, ex in tg:
        locks.acquire(n, me, ["hold"], cap, eta, hold=True, kind="measure" if ex else "hold", exclusive=ex)
    shown = "--measure" if "--measure" in a else " ".join(n for n, _, _ in tg)
    print(f"HELD: {', '.join(n for n, _, _ in tg)} until ~{hhmm(time.time() + eta)}. "
          f"Run `dibs release {shown}` when done.")
    return 0


def release_names(a: list, cfg) -> list:
    names = [n.lower() for n in positional(a)]
    if "--measure" in a:
        names += [MACHINE, *cfg["resource"]]
    if "--all" in a:
        names += locks.held_names()
    return sorted(set(names))


def cmd_release(a: list) -> int:
    cfg, sid = core.load_config(), core.my_session()
    names = release_names(a, cfg)
    if not names:
        raise Problem("name what to release, or pass --measure or --all")
    for n in names:
        hs = locks.holders(n)
        ours = [h for h in hs if h.get("session") == sid or "--force" in a]
        locks.release(h["_dir"] for h in ours)
        if ours:
            print(f"{n}: released")
        elif hs and "--all" not in a:
            print(f"{n}: held by {describe(hs[0])}; only that session, or you with --force, can release it")
        elif "--all" not in a:
            print(f"{n}: already free")
    return 0


def cmd_extend(a: list) -> int:
    eta, sid = core.parse_dur(opt(a, "--eta", "20m")), core.my_session()
    for n in [x.lower() for x in positional(a)]:
        ours = [h for h in locks.holders(n) if h.get("session") == sid]
        for h in ours:
            d = h.pop("_dir")
            h["eta"] = time.time() + eta
            core.write_json(d / "owner.json", h)
        print(f"{n}: ETA now ~{hhmm(time.time() + eta)}" if ours else f"{n}: not held by this session")
    return 0


def disk_path(cfg) -> str:
    return cfg["machine"]["disk"].get("path") or str(Path.home())


def claims(exclude_session=None) -> list:
    out = []
    for p in core.DISK.glob("*.json") if core.DISK.is_dir() else []:
        c = core.read_json(p)
        if not c or (c.get("claude_pid") and not system.alive(c["claude_pid"])) or time.time() > (core.stale_at(c) or 0):
            p.unlink(missing_ok=True)
            continue
        c["_file"] = p
        got = 0.0
        if c.get("path") and Path(c["path"]).exists():
            got = sum(f.stat().st_size for f in Path(c["path"]).rglob("*") if f.is_file()) / 2**30
        c["outstanding"] = max(0.0, c["gb"] - got)
        if not exclude_session or c.get("session") != exclude_session:
            out.append(c)
    return out


def disk_room(cfg, gb: float):
    free = shutil.disk_usage(disk_path(cfg)).free / 2**30
    others = sum(c["outstanding"] for c in claims(exclude_session=core.my_session()))
    floor = cfg["machine"]["disk"].get("floor_gb", 15)
    return free - others - floor >= gb, free, others, floor


def cmd_disk(a: list) -> int:
    cfg = core.load_config()
    sub = a[0] if a else ""
    if sub in ("claim", "check"):
        if len(a) < 2:
            raise Problem(f"usage: dibs disk {sub} <GB>")
        gb = float(a[1])
        ok, free, others, floor = disk_room(cfg, gb)
        msg = (f"{gb:.0f} GB needed: {free:.0f} GB free on {disk_path(cfg)}, {others:.0f} GB claimed by others, "
               f"{floor} GB floor")
        if not ok:
            print(f"WAIT: not enough disk. {msg}. Run in a background Bash: dibs wait --disk {gb:.0f}, then retry.")
            return 3
        if sub == "check":
            print(f"OK: {msg}")
            return 0
        me, path = me_from(a), opt(a, "--path")
        core.write_json(core.DISK / f"{os.getpid()}-{time.time_ns()}.json", {
            **me, "gb": gb, "path": str(Path(path).expanduser().resolve()) if path else None,
            "t0": time.time(), "eta": time.time() + core.parse_dur(opt(a, "--eta", "2h"))})
        print(f"CLAIMED: {msg}")
        return 0
    if sub == "release":
        n = 0
        for c in claims():
            if "--all" in a or c.get("session") == core.my_session():
                c["_file"].unlink(missing_ok=True)
                n += 1
        print(f"released {n} claim(s)")
        return 0
    print(USAGE)
    return 2


def cmd_wait(a: list) -> int:
    cfg = core.load_config()
    if "--disk" in a:
        gb = float(opt(a, "--disk"))
        while not disk_room(cfg, gb)[0]:
            time.sleep(30)
        print(f"FREE: {gb:.0f} GB of disk available")
        return 0
    if "--pressure" in a:
        calm = None
        while True:
            if probes.pressure(cfg):
                calm = None
            elif calm is None:
                calm = time.time()
            elif time.time() - calm >= 30:
                print("FREE: CPU and memory have been under the limits for 30 s")
                return 0
            time.sleep(5)
    names = core.resolve(positional(a), cfg, "--new" in a)
    if not names and "--measure" not in a:
        raise Problem("name what to wait for, e.g. `dibs wait gpu`")
    me = me_from(a)
    ws = [locks.waiter(n, me) for n in names]
    try:
        while [n for n in names if locks.blocking(n, core.capacity(cfg, n))] or locks.blocking(MACHINE, 1):
            time.sleep(5)
        print(f"FREE: {', '.join(names) or 'the machine'}")
        return 0
    finally:
        for w in ws:
            w.unlink(missing_ok=True)


def cmd_status(a: list) -> int:
    cfg = core.load_config()
    held = locks.held_names()
    for n in ([MACHINE] if MACHINE in held else []) + sorted((set(cfg["resource"]) | set(held)) - {MACHINE}):
        hs, cap, ws = locks.holders(n), core.capacity(cfg, n), locks.waiters(n)
        if n == MACHINE and not hs:
            continue
        label = f"{n} {len(hs)}/{cap}" if cap > 1 else n
        first = (describe(hs[0]) + (" [hold]" if hs[0].get("hold") else "")) if hs else "free"
        print(f"{label:16} {first}" + (f"   waiting: {', '.join(ws)}" if ws else ""))
        for h in hs[1:]:
            print(f"{'':16} {describe(h)}" + (" [hold]" if h.get("hold") else ""))
    for c in claims():
        why = f" ({c['purpose']!r})" if c.get("purpose") else ""
        print(f"{'disk':16} {core.display(c)}{why}: {c['outstanding']:.0f} of {c['gb']:.0f} GB outstanding, ETA {hhmm(c['eta'])}"
              + (f" -> {c['path']}" if c.get("path") else ""))
    avail, commit = system.mem()
    parts = [f"CPU {probes.cpu_pct():.0f}%"]
    if avail is not None:
        parts.append(f"free RAM {avail:.1f} GB")
    if commit is not None:
        parts.append(f"commit {commit:.0f}%")
    parts.append(f"disk free {shutil.disk_usage(disk_path(cfg)).free / 2**30:.0f} GB")
    p = probes.pressure(cfg)
    print(f"{'now ' + hhmm(time.time()):16} {'  '.join(parts)}   pressure: {', '.join(p) if p else 'ok'}")
    if "--activity" in a:
        r = probes.activity(cfg, list(cfg["resource"]), 10)
        print(f"{'activity':16} {'; '.join(r) if r else 'quiet'}")
    return 0


def cmd_nice(a: list) -> int:
    table = system.procs()
    pids = [p for p, (_, exe) in table.items() if exe in core.CLAUDE_EXES] if "--all" in a else [core.claude_pid(table)]
    for p in filter(None, pids):
        print(f"claude {p}: {'lowered' if system.lower_priority(p) else 'FAILED'}")
    return 0


def checks(cfg) -> list:
    problems = []
    try:
        core.HELD.mkdir(parents=True, exist_ok=True)
        core.write_json(core.HOME / "state" / "probe.json", {"t": time.time()})
    except OSError as e:
        problems.append(f"can't write {core.HOME}: {e}")
    return problems


def cmd_session_start(a: list) -> int:
    try:
        cfg = core.load_config()
    except SystemExit as e:
        print(f"{e} (dibs is using defaults until this is fixed)")
        cfg = core.DEFAULTS
    if cfg["machine"]["lower_claude_priority"]:
        cpid = core.claude_pid()
        if cpid:
            system.lower_priority(cpid)
    problems = checks(cfg)
    if problems:
        print(f"dibs: {'; '.join(problems)}. Run `dibs doctor` for details.")
    return 0


def cmd_doctor(a: list) -> int:
    def row(k, v):
        print(f"{k:10} {v}")
    row("dibs", f"{__version__}  Python {sys.version.split()[0]} ({sys.executable})")
    row("home", str(core.HOME))
    try:
        cfg = core.load_config()
        row("config", ", ".join(cfg["_files"]) or f"none, using defaults (create {core.USER_CONFIG} to add some)")
    except SystemExit as e:
        row("config", f"ERROR {e}")
        cfg = core.DEFAULTS
    for p in checks(cfg):
        row("state", f"ERROR {p}")
    if core.in_claude():
        me = core.who()
        row("session", f"{me['name']} (claude pid {me['claude_pid'] or 'not found'}, session {me['session'][:8]})")
        last = (core.read_json(core.HOOK_SEEN) or {}).get(me["session"])
        row("hook", f"ran {time.time() - last:.0f} s ago in this session" if last else
            "hasn't run in this session yet: is the dibs plugin enabled? (`/plugin`)")
    else:
        row("session", f"not inside Claude Code; holds belong to {core.my_session()} (set DIBS_SESSION to split)")
    row("psutil", "installed" if system.psutil else "not installed (optional; the built-in readers are used)")
    for n, r in sorted(cfg["resource"].items()):
        p = r.get("probe") or {}
        tool = p.get("command", "")
        tool = tool.split()[0].strip("\"'") if isinstance(tool, str) and tool else ""
        bits = [f"capacity {r.get('capacity', 1)}"] + (["match"] if r.get("match") else [])
        if p:
            kind = p.get("kind") or "command"
            ok = "" if not tool or shutil.which(tool) or Path(tool).exists() else f" (MISSING: {tool})"
            bits.append(f"probe {kind}{ok}")
        row(f"  {n}", ", ".join(bits))
    p = probes.pressure(cfg)
    row("pressure", ", ".join(p) if p else "ok")
    return 0


def main(argv=None) -> int:
    a = sys.argv[1:] if argv is None else argv
    if not a or a[0] in ("-h", "--help", "help"):
        print(USAGE)
        return 0 if a else 2
    if a[0] in ("version", "--version", "-V"):
        print(__version__)
        return 0
    if a[0] == "hook":
        from . import hook
        return hook.main()
    table = {"run": cmd_run, "hold": cmd_hold, "release": cmd_release, "extend": cmd_extend, "wait": cmd_wait,
             "status": cmd_status, "disk": cmd_disk, "nice": cmd_nice, "doctor": cmd_doctor,
             "session-start": cmd_session_start}
    if a[0] not in table:
        print(f"dibs: unknown command {a[0]!r}\n\n{USAGE}", file=sys.stderr)
        return 2
    try:
        return table[a[0]](a[1:]) or 0
    except KeyboardInterrupt:
        return 130
