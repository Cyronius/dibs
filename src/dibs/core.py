import getpass, json, os, re, shutil, subprocess, sys, time
from pathlib import Path

from . import system

NAME_RE = re.compile(r"[a-z0-9][a-z0-9._-]{0,63}")
MACHINE = "machine"                              # reserved: held by --measure, blocks everything else
CLAUDE_EXES = ("claude.exe", "claude")


def default_home() -> Path:
    if os.environ.get("DIBS_HOME"):
        return Path(os.environ["DIBS_HOME"]).expanduser()
    if system.WINDOWS:
        return Path(os.environ.get("LOCALAPPDATA") or Path.home() / "AppData" / "Local") / "dibs"
    if system.MAC:
        return Path.home() / "Library" / "Application Support" / "dibs"
    return Path(os.environ.get("XDG_STATE_HOME") or Path.home() / ".local" / "state") / "dibs"


HOME = default_home()
HELD, DISK = HOME / "state" / "held", HOME / "state" / "disk"
PRESSURE, KNOWN, HOOK_SEEN = HOME / "state" / "pressure.json", HOME / "state" / "known.json", HOME / "state" / "hook.json"
HOLD_CALLS = HOME / "state" / "hold-calls"
USER_CONFIG = HOME / "config.toml"
REPO_CONFIG = ".dibs.toml"


class Problem(SystemExit):
    def __init__(self, msg: str):
        super().__init__(f"dibs: {msg}")


def read_json(p: Path):
    try:
        return json.loads(p.read_text(encoding="utf-8"))
    except Exception:
        return None


def write_json(p: Path, obj) -> None:
    p.parent.mkdir(parents=True, exist_ok=True)
    tmp = p.with_name(f"{p.name}.{os.getpid()}.tmp")
    tmp.write_text(json.dumps(obj), encoding="utf-8")
    for _ in range(50):
        try:
            os.replace(tmp, p)
            return
        except PermissionError:                      # Windows: a reader has it open for a moment
            time.sleep(0.02)
    tmp.unlink(missing_ok=True)


def parse_dur(s: str) -> float:
    m = re.fullmatch(r"(\d+(?:\.\d+)?)\s*([smhd]?)", (s or "").strip().lower())
    if not m:
        raise Problem(f"bad duration {s!r}: use e.g. 90s, 40m, 2h")
    return float(m.group(1)) * {"": 60, "s": 1, "m": 60, "h": 3600, "d": 86400}[m.group(2)]


def hhmm(t) -> str:
    return time.strftime("%H:%M", time.localtime(t)) if t else "?"


# ---- config --------------------------------------------------------------------------------------

DEFAULTS = {
    "machine": {"cpu_max": 90, "ram_floor_gb": 8, "commit_max": 95, "quiet_cpu_max": 25,
                "lower_claude_priority": system.WINDOWS, "disk": {"path": str(Path.home()), "floor_gb": 15}},
    "hook": {"enabled": True, "pressure": True, "wait_inline_s": 240},
    "measure": {"noise_core_pct": 50, "ignore": ["system idle process", "idle", "kernel_task", "dwm.exe"]},
    "resource": {},
}


def merge(a: dict, b: dict) -> dict:
    out = dict(a)
    for k, v in b.items():
        out[k] = merge(out[k], v) if isinstance(v, dict) and isinstance(out.get(k), dict) else v
    return out


def find_repo_config(start) -> Path | None:
    p = Path(start or os.getcwd()).resolve()
    for d in (p, *p.parents):
        f = d / REPO_CONFIG
        if f.is_file():
            return f
    return None


def _load_toml(p: Path) -> dict:
    try:
        import tomllib
    except ImportError:
        raise Problem(f"reading {p} needs Python 3.11+ (found {sys.version.split()[0]})")
    try:
        return tomllib.loads(p.read_text(encoding="utf-8"))
    except tomllib.TOMLDecodeError as e:
        raise Problem(f"{p}: {e}")


def load_config(cwd=None) -> dict:
    cfg, files = DEFAULTS, []
    for p in (USER_CONFIG, find_repo_config(cwd)):
        if p and p.is_file():
            cfg = merge(cfg, _load_toml(p))
            files.append(str(p))
    for name, r in cfg["resource"].items():
        if not NAME_RE.fullmatch(name) or name == MACHINE:
            raise Problem(f"resource name {name!r} in config: use lowercase letters, digits, '.', '_', '-'"
                          + (f"; {MACHINE!r} is reserved" if name == MACHINE else ""))
        if not isinstance(r, dict):
            raise Problem(f"[resource.{name}] must be a table")
        cap = r.get("capacity", 1)
        if not isinstance(cap, int) or cap < 1:
            raise Problem(f"[resource.{name}] capacity must be a whole number of 1 or more")
        if r.get("match"):
            try:
                r["_match"] = re.compile(r["match"], re.I)
            except re.error as e:
                raise Problem(f"[resource.{name}] match: {e}")
    cfg["_files"] = files
    return cfg


def capacity(cfg, name: str) -> int:
    return 1 if name == MACHINE else cfg["resource"].get(name, {}).get("capacity", 1)


# ---- names ---------------------------------------------------------------------------------------

def lev(a: str, b: str) -> int:
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (ca != cb)))
        prev = cur
    return prev[-1]


def known_names(cfg) -> set:
    known = set(cfg["resource"]) | set((read_json(KNOWN) or {}).keys())
    if HELD.exists():
        known |= {p.name for p in HELD.iterdir() if p.is_dir()}
    return known - {MACHINE}


def remember(names) -> None:
    k = read_json(KNOWN) or {}
    cutoff = time.time() - 90 * 86400
    k = {n: t for n, t in k.items() if t > cutoff}
    k.update({n: time.time() for n in names if n != MACHINE})
    write_json(KNOWN, k)


def resolve(raw: list, cfg, allow_new=False) -> list:
    out = []
    for r in raw:
        n = r.lower()
        if not NAME_RE.fullmatch(n):
            raise Problem(f"{r!r} isn't a resource name: use lowercase letters, digits, '.', '_', '-' (max 64)")
        if n == MACHINE:
            raise Problem(f"{MACHINE!r} is reserved for --measure")
        known = known_names(cfg)
        if n not in known and not allow_new:
            close = sorted(k for k in known if lev(n, k) <= (1 if len(k) <= 4 else 2))
            if close:
                raise Problem(f"there's no resource {n!r} yet. Did you mean {close[0]!r}? "
                              f"Pass --new to use {n!r} anyway.")
        out.append(n)
    return sorted(set(out))


# ---- identity ------------------------------------------------------------------------------------

def my_session() -> str:
    # outside Claude, all of one user's processes form one session unless DIBS_SESSION splits them
    return os.environ.get("CLAUDE_CODE_SESSION_ID") or os.environ.get("DIBS_SESSION") \
        or f"user-{getpass.getuser()}"


def my_agent() -> str:
    return os.environ.get("DIBS_AGENT", "").strip()


def covers(own, sid, agent) -> bool:
    # a main-agent hold covers its whole session; a subagent's hold covers only that subagent
    return bool(sid) and own.get("session") == sid and (not own.get("agent") or own["agent"] == agent)


def controls(own, sid, agent) -> bool:
    # who may end or extend a hold: its own agent, or the main agent of its session
    return own.get("session") == sid and (not agent or own.get("agent") == agent)


def in_claude() -> bool:
    return bool(os.environ.get("CLAUDE_CODE_SESSION_ID"))


def claude_pid(table=None) -> int:
    env = os.environ.get("CLAUDE_PID", "")
    if env.isdigit() and system.alive(int(env)):
        return int(env)
    table, pid = table or system.procs(), os.getpid()
    for _ in range(30):
        ppid, exe = table.get(pid, (0, ""))
        if exe in CLAUDE_EXES:
            return pid
        if not ppid or ppid == pid:
            break
        pid = ppid
    return 0


def sessions_dir() -> Path:
    return Path(os.environ.get("CLAUDE_CONFIG_DIR") or Path.home() / ".claude") / "sessions"


_agents = None


def _agents_by_session() -> dict:
    # documented fallback for when Claude Code's internal sessions file moves or changes shape
    global _agents
    if _agents is None:
        _agents = {}
        exe = shutil.which("claude")
        try:
            out = subprocess.run([exe, "agents", "--json"], capture_output=True, text=True, timeout=15).stdout \
                if exe else "[]"
            for s in json.loads(out or "[]"):
                if s.get("sessionId") and s.get("name"):
                    _agents[s["sessionId"]] = s
        except Exception:
            pass
    return _agents


def session_info(cpid, sid) -> dict:
    info = read_json(sessions_dir() / f"{cpid}.json") if cpid else None
    if isinstance(info, dict) and info.get("name") and (not sid or info.get("sessionId") == sid):
        return info
    if sid and os.environ.get("DIBS_AGENTS_LOOKUP", "1") != "0":
        return _agents_by_session().get(sid, {})
    return {}


def split_label(label: str, name: str) -> tuple:
    # agents often write "<session>/<worker>: <purpose>"; keep the worker and the purpose
    who_, sep, what = label.partition(":")
    first, _, worker = who_.strip().partition("/")
    if sep and len(first) >= 4 and " " not in who_.strip() and name.lower().startswith(first.lower()):
        return worker.strip() or None, what.strip()
    return None, label.strip()


def who(label=None, tag=None) -> dict:
    sid = my_session()
    cpid = claude_pid() if in_claude() else 0
    info = session_info(cpid, sid) if in_claude() else {}
    project = Path(info.get("cwd") or os.getcwd()).name
    if info.get("name"):
        name = info["name"]
    elif in_claude():
        name = f"{sid[:8]} ({project})"
    else:
        name = os.environ.get("DIBS_NAME") or tag or f"{getpass.getuser()} ({project})"
        tag = None if name == tag else tag
    t2, purpose = split_label((label or "").strip(), name)
    agent = my_agent()
    return {"claude_pid": cpid, "session": sid, "agent": agent, "name": name, "project": project,
            "tag": tag or t2 or agent[:7] or None, "purpose": purpose[:80]}


def display(own) -> str:
    name = (session_info(own.get("claude_pid"), own.get("session")).get("name") if own.get("claude_pid") else None) \
        or own.get("name") or "?"
    tag = own.get("tag")
    return f"{name}/{tag}" if tag and tag != name else name


def short_cmd(cmd: str) -> str:
    # a long scratchpad path tells a waiter nothing; the file name and the arguments do
    return " ".join(re.split(r"[/\\]", t.strip("\"'"))[-1] or t for t in (cmd or "").split())


def stale_at(own):
    if not own.get("eta"):
        return None
    span = max(0.0, own["eta"] - own.get("t0", own["eta"]))
    return own["eta"] + max(600.0, 0.25 * span)


def describe(own) -> str:
    what = own.get("purpose") or short_cmd(own.get("cmd", ""))
    when = f"until ~{hhmm(own['eta'])} (stale at {hhmm(stale_at(own))})" if own.get("eta") \
        else f"since {hhmm(own.get('t0'))}"
    return f"{display(own)}{f' ({what[:60]!r})' if what else ''} {when}"


def mine(own, sid=None, table=None, agent=None) -> bool:
    # subagents share the session id, so a sibling's run lock isn't ours; a covering hold or an ancestor's run lock is
    sid = my_session() if sid is None else sid
    if not sid or own.get("session") != sid:
        return False
    if own.get("hold"):
        return covers(own, sid, my_agent() if agent is None else agent)
    return own.get("pid") in system.ancestors(table)
