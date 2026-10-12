import json, re, shlex, sys, time

from . import core, locks, probes, system
from .core import MACHINE

TRIVIAL = re.compile(r"^\s*(?:cd|ls|dir|cat|type|head|tail|grep|rg|find|wc|pwd|echo|which|where|date|true|"
                     r"Get-Content|Get-ChildItem|Get-Item|Get-Location|Select-String|Test-Path|Write-Host|Write-Output|"
                     r"git\s+(?:status|log|diff|show|branch|rev-parse|remote|config\s+--get|ls-files|blame)|"
                     r"gh\s+(?:pr|issue|run|repo)\s+(?:view|list|status|checks|diff))\b", re.I)
INVOKE = re.compile(r"""^\s*(?:&\s*)?(?:["']?[^\s"']*python[\w.]*["']?\s+(?:-m\s+dibs|["']?[^\s"']*run_dibs\.py["']?)"""
                    r"""|["']?(?:[^\s"']*[\\/])?dibs(?:\.cmd)?["']?)\s+([a-z-]+)(.*)$""", re.I)
PASSIVE = {"status", "wait", "release", "extend", "disk", "nice", "doctor", "help", "version", "session-start"}
VALUE_FLAGS = {"--label", "--tag", "--eta", "--path", "--disk"}


def segments(cmd: str) -> list:
    return [s for s in re.split(r"&&|\|\||[;|\n]", cmd) if s.strip()]


def invocation(cmd: str) -> tuple:
    # only a segment that starts by running dibs counts; a mention inside a string or heredoc doesn't
    for s in segments(cmd):
        m = INVOKE.match(s)
        if not m:
            continue
        pre = m.group(2).split(" -- ", 1)[0]
        try:
            toks = shlex.split(pre)
        except ValueError:
            toks = pre.split()
        names, skip = [], False
        for t in toks:
            if skip:
                skip = False
            elif t in VALUE_FLAGS:
                skip = True
            elif not t.startswith("-"):
                names.append(t.lower())
        return m.group(1).lower(), names, "--measure" in toks
    return None, [], False


def blockers(cmd: str, sid: str, cfg) -> list:
    # each entry: (reason, clears_at epoch | None | 'pressure', what to pass to `dibs wait`)
    verb, names, measure = invocation(cmd)
    if verb in PASSIVE or (verb is None and all(TRIVIAL.match(s) for s in segments(cmd))):
        return []
    out, held = [], locks.held_names()
    table = system.procs() if held else {}
    m = locks.blocking(MACHINE, 1, sid, table) if MACHINE in held else []
    if m:
        out.append((f"the whole machine is reserved for a measurement by {core.describe(m[0])}",
                    m[0].get("eta"), "--measure"))
    else:
        for name in held:
            if name == MACHINE:
                continue
            rx = cfg["resource"].get(name, {}).get("_match")
            if not (name in names or measure or (rx and rx.search(cmd))):
                continue
            b = locks.blocking(name, core.capacity(cfg, name), sid, table)
            if b:
                out.append((f"{name} is held by {core.describe(b[0])}", min((h.get("eta") or 9e99) for h in b), name))
    if cfg["hook"]["pressure"] and not (held and locks.session_holds(sid)):
        p = probes.pressure(cfg)                      # a holder owns the machine, so pressure doesn't hold it back
        if p:
            out.append((f"the machine is under pressure: {', '.join(p)}", "pressure", "--pressure"))
    return out


def deny(reasons: list) -> None:
    waits = " ".join(dict.fromkeys(r[2] for r in reasons if r[2] != "--pressure"))
    cmd = f"dibs wait {waits or '--pressure'}"
    if waits and any(r[2] == "--pressure" for r in reasons):
        cmd += " && dibs wait --pressure"
    msg = (f"WAIT, not a refusal: {'; '.join(r[0] for r in reasons)}.\n"
           f"Run in a background Bash: {cmd}\n"
           "When it prints FREE, rerun this exact command unchanged. Tell the user what you're waiting on and until when.\n"
           "Don't work around it (different tool, renamed command, or skipping the check). "
           "Meanwhile, work that doesn't need the machine (reading, editing) is fine.")
    print(json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny",
                                             "permissionDecisionReason": msg}}))


def seen(sid: str) -> None:
    s = core.read_json(core.HOOK_SEEN) or {}
    if time.time() - s.get(sid, 0) > 60:
        s = {k: v for k, v in s.items() if time.time() - v < 86400}
        s[sid] = time.time()
        core.write_json(core.HOOK_SEEN, s)


def main() -> int:
    try:
        data = json.loads(sys.stdin.buffer.read().decode("utf-8", "replace") or "{}")
        cmd = str((data.get("tool_input") or {}).get("command", ""))
        sid = str(data.get("session_id", ""))
        cfg = core.load_config(data.get("cwd"))
        if not cfg["hook"]["enabled"] or not cmd.strip():
            return 0
        seen(sid)
        deadline = time.time() + cfg["hook"]["wait_inline_s"]
        while True:
            r = blockers(cmd, sid, cfg)
            if not r:
                return 0
            if time.time() >= deadline or any(x[1] != "pressure" and (x[1] is None or x[1] > deadline) for x in r):
                deny(r)
                return 0
            time.sleep(10)
    except (Exception, SystemExit) as e:             # a broken guard must never stop work
        print(f"dibs hook error (command allowed): {e}", file=sys.stderr)
        return 0
