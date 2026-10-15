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
        return m.group(1).lower(), names, toks
    return None, [], []


def blockers(cmd: str, sid: str, cfg, agent: str = "") -> list:
    # each entry: (reason, clears_at epoch | None | 'pressure', what to pass to `dibs wait`)
    verb, names, toks = invocation(cmd)
    measure = "--measure" in toks
    if verb in PASSIVE or (verb is None and all(TRIVIAL.match(s) for s in segments(cmd))):
        return []
    out, held = [], locks.held_names()
    table = system.procs() if held else {}
    m = locks.blocking(MACHINE, 1, sid, table, agent) if MACHINE in held else []
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
            b = locks.blocking(name, core.capacity(cfg, name), sid, table, agent)
            if b:
                out.append((f"{name} is held by {core.describe(b[0])}", min((h.get("eta") or 9e99) for h in b), name))
    if cfg["hook"]["pressure"] and not (held and locks.session_holds(sid, agent)):
        p = probes.pressure(cfg)                      # a holder owns the machine, so pressure doesn't hold it back
        if p:
            out.append((f"the machine is under pressure: {', '.join(p)}", "pressure", "--pressure"))
    return out


def foreign_holds(cmd: str, sid: str, cfg, agent: str) -> list:
    # holds a subagent must not end or extend: its siblings' and the main agent's
    verb, names, toks = invocation(cmd)
    if not agent or verb not in ("release", "extend") or "--force" in toks:
        return []
    if verb == "release":
        names += ([MACHINE, *cfg["resource"]] if "--measure" in toks else []) + (locks.held_names() if "--all" in toks else [])
    return [h for n in sorted(set(names)) for h in locks.holders(n)
            if h.get("session") == sid and h.get("hold") and h.get("agent", "") != agent]


def deny_text(msg: str) -> None:
    print(json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny",
                                             "permissionDecisionReason": msg}}))


def deny_foreign(verb: str, hs: list) -> None:
    h = hs[0]
    deny_text(f"{h.get('resource')} is held by {core.describe(h)}; only that agent, or the main agent, can {verb} it. "
              "Leave it, or ask the agent that took it. If the user told you to take it over, pass --force.")


def deny(reasons: list) -> None:
    waits = " ".join(dict.fromkeys(r[2] for r in reasons if r[2] != "--pressure"))
    cmd = f"dibs wait {waits or '--pressure'}"
    if waits and any(r[2] == "--pressure" for r in reasons):
        cmd += " && dibs wait --pressure"
    deny_text(f"WAIT, not a refusal: {'; '.join(r[0] for r in reasons)}.\n"
              f"Run in a background Bash: {cmd}\n"
              "When it prints FREE, rerun this exact command unchanged. Tell the user what you're waiting on and until when.\n"
              "Don't work around it (different tool, renamed command, or skipping the check). "
              "Meanwhile, work that doesn't need the machine (reading, editing) is fine.")


def call_file(data: dict):
    cid = re.sub(r"\W", "_", str(data.get("tool_use_id") or ""))
    return core.HOLD_CALLS / f"{cid}.json" if cid else None


def note_hold(cmd: str, data: dict, agent: str) -> None:
    # the CLI can't tell which subagent ran `dibs hold`; remember when the call began so PostToolUse can stamp it
    p = call_file(data)
    if not (agent and p and invocation(cmd)[0] == "hold"):
        return
    for old in core.HOLD_CALLS.glob("*.json") if core.HOLD_CALLS.is_dir() else []:
        if time.time() - old.stat().st_mtime > 3600:
            old.unlink(missing_ok=True)
    core.write_json(p, {"t": time.time()})


def stamp_hold(cmd: str, data: dict, sid: str, agent: str, cfg) -> None:
    p = call_file(data)
    began = (core.read_json(p) or {}).get("t") if p else None
    if began is None or not agent:
        return
    p.unlink(missing_ok=True)
    _, names, toks = invocation(cmd)
    if "--measure" in toks:
        names = [*names, MACHINE, *cfg["resource"]]
    locks.stamp_agent(sid, agent, sorted(set(names)), began)


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
        sid, agent = str(data.get("session_id", "")), str(data.get("agent_id") or "")
        event = data.get("hook_event_name") or "PreToolUse"
        cfg = core.load_config(data.get("cwd"))
        if not cfg["hook"]["enabled"]:
            return 0
        if event == "SubagentStop":
            if agent:
                locks.release_agent(sid, agent)
            return 0
        if not cmd.strip():
            return 0
        if event == "PostToolUse":
            stamp_hold(cmd, data, sid, agent, cfg)
            return 0
        seen(sid)
        if f := foreign_holds(cmd, sid, cfg, agent):
            deny_foreign(invocation(cmd)[0], f)
            return 0
        deadline = time.time() + cfg["hook"]["wait_inline_s"]
        while True:
            r = blockers(cmd, sid, cfg, agent)
            if not r:
                note_hold(cmd, data, agent)
                return 0
            if time.time() >= deadline or any(x[1] != "pressure" and (x[1] is None or x[1] > deadline) for x in r):
                deny(r)
                return 0
            time.sleep(10)
    except (Exception, SystemExit) as e:             # a broken guard must never stop work
        print(f"dibs hook error (command allowed): {e}", file=sys.stderr)
        return 0
