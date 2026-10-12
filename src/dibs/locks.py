import os, shutil, sys, time

from . import core, system
from .core import HELD, describe, mine, read_json, write_json


def rdir(name: str):
    return HELD / name


def is_stale(d, own) -> bool:
    if own is None:                                  # owner.json not written yet, or a crash right after mkdir
        try:
            return time.time() - d.stat().st_mtime > 30
        except FileNotFoundError:
            return False
    if own.get("hold"):
        return bool(own.get("claude_pid") and not system.alive(own["claude_pid"])) \
            or time.time() > (core.stale_at(own) or 9e99)
    return not system.alive(own.get("pid"))


def break_lock(d) -> bool:
    grave = d.with_name(f"{d.name}.stale.{os.getpid()}.{time.time_ns()}")
    try:
        os.rename(d, grave)
    except OSError:
        return False
    if not is_stale(grave, read_json(grave / "owner.json")):   # someone re-took it between our read and rename
        try:
            os.rename(grave, d)
            return False
        except OSError:
            pass
    shutil.rmtree(grave, ignore_errors=True)
    return True


def holders(name: str) -> list:
    rd, out = rdir(name), []
    if not rd.is_dir():
        return out
    for d in sorted(rd.iterdir()):
        if not d.is_dir() or not d.name.isdigit():
            continue
        own = read_json(d / "owner.json")
        if is_stale(d, own):
            if own:
                print(f"dibs: {name} was held by {core.display(own)}, which is gone; taking it over",
                      file=sys.stderr, flush=True)
            break_lock(d)
            continue
        own = dict(own or {"name": "?", "t0": time.time()})
        own["_dir"] = d
        out.append(own)
    return out


def held_names() -> list:
    return sorted(p.name for p in HELD.iterdir() if p.is_dir()) if HELD.is_dir() else []


def blocking(name: str, cap: int, sid=None, table=None) -> list:
    # holders that stop this session from getting a slot now
    hs = holders(name)
    others = [h for h in hs if not mine(h, sid, table)]
    return others if others and len(hs) >= cap else []


def session_holds(sid: str) -> list:
    if not sid:
        return []
    return [h for n in held_names() for h in holders(n) if h.get("session") == sid]


def waiter(name: str, me: dict):
    p = rdir(name) / f"wait.{os.getpid()}.json"
    write_json(p, {**me, "pid": os.getpid(), "t0": time.time()})
    return p


def waiters(name: str) -> list:
    out = []
    for p in rdir(name).glob("wait.*.json") if rdir(name).is_dir() else []:
        w = read_json(p)
        if w and system.alive(w.get("pid")):
            out.append(core.display(w))
        else:
            p.unlink(missing_ok=True)
    return out


def acquire(name: str, me: dict, cmd, cap: int, eta=None, hold=False, kind="run", exclusive=False) -> list:
    rd = rdir(name)
    rd.mkdir(parents=True, exist_ok=True)
    owner = {**me, "pid": me["claude_pid"] if hold else os.getpid(), "resource": name, "kind": kind, "hold": hold,
             "t0": time.time(), "eta": time.time() + eta if eta else None,
             "cmd": " ".join(cmd)[:300] if cmd else ""}

    def take(i):
        d = rd / str(i)
        try:
            d.mkdir()
        except FileExistsError:
            return None
        write_json(d / "owner.json", owner)
        return d

    t0, last, w, got, todo = time.time(), 0.0, None, [], list(range(cap))
    try:
        while True:
            hs, table = [h for h in holders(name) if h["_dir"] not in got], system.procs()
            if any(mine(h, None, table) for h in hs):
                return got                           # re-entrant: our session's hold, or our parent's run
            if exclusive:
                while todo and (d := take(todo[0])):
                    got.append(d)
                    todo.pop(0)
                if not todo:
                    break
            else:
                d = next(filter(None, (take(i) for i in range(cap))), None)
                if d:
                    got.append(d)
                    break
            w = w or waiter(name, me)
            if hs and time.time() - last > 120:
                print(f"dibs: waiting for {name}, held by {describe(hs[0])}", flush=True)
                last = time.time()
            time.sleep(2)
        if time.time() - t0 > 5:
            print(f"dibs: got {name} after {time.time() - t0:.0f} s", flush=True)
        core.remember([name])
        return got
    except BaseException:
        release(got)
        raise
    finally:
        if w:
            w.unlink(missing_ok=True)


def release(dirs) -> None:
    for d in reversed(list(dirs or [])):
        (d / "owner.json").unlink(missing_ok=True)
        try:
            d.rmdir()
        except OSError:
            pass
