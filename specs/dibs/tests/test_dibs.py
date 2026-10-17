import json, subprocess, sys, time

from pathlib import Path

from conftest import BIN, sleep_cmd


# Traces: DIBS-ANY-NAME
def test_any_name_works_without_config(box):
    r = box.run("run", "network", "--", sys.executable, "-c", "print('ran')")
    assert r.returncode == 0 and "ran" in r.stdout


# Traces: DIBS-ANY-NAME
def test_bad_name_is_refused(box):
    r = box.run("run", "GPU!", "--", sys.executable, "-c", "pass")
    assert r.returncode != 0 and "isn't a resource name" in r.stderr
    r = box.run("run", "machine", "--", sys.executable, "-c", "pass")
    assert "reserved" in r.stderr


# Traces: DIBS-TYPO-GUARD
def test_near_miss_name_is_refused_until_new(box):
    box.config("[resource.gpu]\n")
    r = box.run("run", "gpuu", "--", sys.executable, "-c", "pass")
    assert r.returncode != 0 and "Did you mean 'gpu'" in r.stderr
    r = box.run("run", "gpuu", "--new", "--", sys.executable, "-c", "print('ok')")
    assert r.returncode == 0 and "ok" in r.stdout


# Traces: DIBS-TYPO-GUARD
def test_names_seen_before_count_as_known(box):
    assert box.run("run", "testdb", "--", sys.executable, "-c", "pass").returncode == 0
    r = box.run("run", "testbd", "--", sys.executable, "-c", "pass")
    assert "Did you mean 'testdb'" in r.stderr


# Traces: DIBS-EXCLUSIVE
def test_other_session_waits_for_a_held_resource(box):
    first = box.bg("run", "gpu", *sleep_cmd(4))
    assert box.until(lambda: box.held("gpu"))
    second = box.bg("run", "gpu", "--", sys.executable, "-c", "print('second ran')", env=box.b)
    time.sleep(1.5)
    assert second.poll() is None
    out = second.communicate(timeout=60)[0]
    first.wait(timeout=60)
    assert "held by alpha-1f" in out and "second ran" in out


# Traces: DIBS-CAPACITY
def test_capacity_two_admits_two_holders(box):
    box.config("[resource.network]\ncapacity = 2\n")
    a = box.bg("run", "network", *sleep_cmd(4))
    b = box.bg("run", "network", *sleep_cmd(4), env=box.b)
    assert box.until(lambda: len(list((box.home / "state/held/network").glob("[0-9]"))) == 2)
    c = box.bg("run", "network", "--tag", "third", "--", sys.executable, "-c", "print('third ran')", env=box.b)
    time.sleep(1.5)
    assert c.poll() is None
    st = box.run("status").stdout
    assert "network 2/2" in st
    for p in (a, b):
        p.wait(timeout=60)
    assert "third ran" in c.communicate(timeout=60)[0]


# Traces: DIBS-SIBLINGS
def test_sibling_runs_in_one_session_take_turns(box):
    t1 = box.bg("run", "gpu", "--tag", "t1", "--label", "shards 1-4", *sleep_cmd(4))
    assert box.until(lambda: box.held("gpu"))
    t2 = box.bg("run", "gpu", "--tag", "t2", "--", sys.executable, "-c", "print('t2 ran')")
    time.sleep(1.5)
    assert t2.poll() is None
    out = t2.communicate(timeout=60)[0]
    t1.wait(timeout=60)
    assert "held by alpha-1f/t1 ('shards 1-4')" in out and "t2 ran" in out


# Traces: DIBS-HOLD-REENTRANT
def test_hold_covers_later_runs_in_the_session(box):
    assert "HELD: gpu" in box.run("hold", "gpu", "--eta", "5m").stdout
    r = box.run("run", "gpu", "--", sys.executable, "-c", "print('inside')", timeout=15)
    assert "inside" in r.stdout and "waiting" not in r.stdout
    assert "FREE: gpu" in box.run("wait", "gpu", timeout=15).stdout
    assert "gpu: released" in box.run("release", "gpu").stdout


# Traces: DIBS-HOLD-REENTRANT
def test_nested_run_is_reentrant(box):
    r = box.run("run", "gpu", "--", BIN, "run", "gpu", "--", sys.executable, "-c",
                "print('nested ok')", timeout=30)
    assert "nested ok" in r.stdout


AGENT_GUARD = "[resource.gpu]\nmatch = 'ollama'\n"
SESSION_A = "sess-aaaa-1111"


def denied(d) -> bool:
    return bool(d) and d["hookSpecificOutput"]["permissionDecision"] == "deny"


# Traces: DIBS-AGENT-HOLD-SCOPE
def test_hook_holds_back_siblings_of_a_subagents_hold(box):
    box.config(AGENT_GUARD)
    box.run("hold", "gpu", "--eta", "5m", env=box.as_agent("a1"))
    assert box.hook("ollama run x", SESSION_A, agent="a1") is None
    assert denied(box.hook("ollama run x", SESSION_A, agent="a2"))
    assert denied(box.hook("ollama run x", SESSION_A))


# Traces: DIBS-AGENT-HOLD-SCOPE
def test_main_hold_still_covers_every_subagent(box):
    box.config(AGENT_GUARD)
    box.run("hold", "gpu", "--eta", "5m")
    assert box.hook("ollama run x", SESSION_A, agent="a1") is None
    assert box.hook("ollama run x", SESSION_A) is None


# Traces: DIBS-AGENT-HOLD-SCOPE
def test_run_waits_behind_a_siblings_hold(box):
    box.run("hold", "gpu", "--eta", "5m", env=box.as_agent("a1"))
    second = box.bg("run", "gpu", "--", sys.executable, "-c", "print('a2 ran')", env=box.as_agent("a2"))
    time.sleep(1.5)
    assert second.poll() is None
    box.run("release", "gpu", env=box.as_agent("a1"))
    out = second.communicate(timeout=60)[0]
    assert "a2 ran" in out


# Traces: DIBS-AGENT-HOLD-SCOPE
def test_only_the_owning_agent_or_main_releases_an_agents_hold(box):
    box.run("hold", "gpu", "--eta", "5m", env=box.as_agent("a1"))
    r = box.run("release", "gpu", env=box.as_agent("a2"))
    assert "only that agent" in r.stdout and box.held("gpu")
    assert "gpu: released" in box.run("release", "gpu").stdout


# Traces: DIBS-AGENT-HOLD-SCOPE
def test_hook_denies_a_sibling_ending_an_agents_hold(box):
    box.run("hold", "gpu", "--eta", "5m", env=box.as_agent("a1"))
    d = box.hook("dibs release gpu", SESSION_A, agent="a2")
    assert denied(d) and "only that agent" in d["hookSpecificOutput"]["permissionDecisionReason"]
    assert denied(box.hook("dibs extend gpu --eta 10m", SESSION_A, agent="a2"))
    assert box.hook("dibs release gpu", SESSION_A, agent="a1") is None
    assert box.hook("dibs release gpu", SESSION_A) is None
    assert box.hook("dibs release gpu --force", SESSION_A, agent="a2") is None


# Traces: DIBS-AGENT-IDENTITY
def test_post_hook_stamps_the_hold_a_subagent_just_took(box):
    box.config(AGENT_GUARD)
    cmd = "dibs hold gpu --eta 5m"
    assert box.hook(cmd, SESSION_A, agent="a1", call="t1") is None
    box.run("hold", "gpu", "--eta", "5m")
    box.post_hook(cmd, SESSION_A, agent="a1", call="t1")
    assert box.owner("gpu")["agent"] == "a1"
    assert denied(box.hook("ollama run x", SESSION_A, agent="a2"))


# Traces: DIBS-AGENT-IDENTITY
def test_post_hook_leaves_an_existing_main_hold_alone(box):
    box.run("hold", "gpu", "--eta", "5m")
    cmd = "dibs hold gpu --eta 5m"
    box.hook(cmd, SESSION_A, agent="a1", call="t2")
    box.run("hold", "gpu", "--eta", "5m")
    box.post_hook(cmd, SESSION_A, agent="a1", call="t2")
    assert not box.owner("gpu").get("agent")


# Traces: DIBS-STALE
def test_lock_of_a_dead_process_is_taken_over(box):
    dead = subprocess.Popen([sys.executable, "-c", "pass"])
    dead.wait()
    d = box.home / "state" / "held" / "gpu" / "0"
    d.mkdir(parents=True)
    (d / "owner.json").write_text(json.dumps({"pid": dead.pid, "session": "gone", "name": "ghost", "hold": False,
                                              "t0": time.time()}))
    r = box.run("run", "gpu", "--", sys.executable, "-c", "print('took it')", timeout=30)
    assert "took it" in r.stdout and "ghost" in r.stderr


# Traces: DIBS-STALE
def test_hold_past_its_eta_and_grace_is_taken_over(box):
    d = box.home / "state" / "held" / "gpu" / "0"
    d.mkdir(parents=True)
    now = time.time()
    (d / "owner.json").write_text(json.dumps({"pid": 0, "claude_pid": 0, "session": "x", "name": "late", "hold": True,
                                              "t0": now - 7200, "eta": now - 3600}))
    r = box.run("run", "gpu", "--", sys.executable, "-c", "print('took it')", timeout=30)
    assert "took it" in r.stdout


# Traces: DIBS-IDENTITY
def test_status_names_the_session_tag_and_purpose(box):
    p = box.bg("run", "gpu", "--tag", "d01", "--label", "dataset shards 1-4", *sleep_cmd(3))
    assert box.until(lambda: box.held("gpu"))
    time.sleep(0.5)
    st = box.run("status").stdout
    p.wait(timeout=30)
    assert "alpha-1f/d01 ('dataset shards 1-4')" in st


# Traces: DIBS-RELEASE
def test_release_only_frees_your_own_hold_unless_forced(box):
    box.run("hold", "gpu", "--eta", "5m")
    r = box.run("release", "gpu", env=box.b)
    assert "only that session" in r.stdout and box.held("gpu")
    assert "gpu: released" in box.run("release", "gpu", "--force", env=box.b).stdout
    assert not box.held("gpu")


# Traces: DIBS-WAIT
def test_wait_returns_free_when_the_holder_ends(box):
    p = box.bg("run", "gpu", *sleep_cmd(3))
    assert box.until(lambda: box.held("gpu"))
    t0 = time.time()
    r = box.run("wait", "gpu", env=box.b, timeout=60)
    p.wait(timeout=30)
    assert "FREE: gpu" in r.stdout and time.time() - t0 > 1


# Traces: DIBS-DISK
def test_disk_claim_too_big_is_refused(box):
    r = box.run("disk", "claim", "999999", "--eta", "1h")
    assert r.returncode == 3 and "WAIT: not enough disk" in r.stdout


# Traces: DIBS-DISK
def test_disk_claim_counts_against_other_sessions(box):
    import shutil
    box.config("[machine.disk]\nfloor_gb = 0\n")
    free = shutil.disk_usage(Path.home()).free / 2**30
    want = max(1, int(free - 5))
    assert "CLAIMED" in box.run("disk", "claim", str(want), "--eta", "1h", "--label", "weights").stdout
    assert box.run("disk", "check", "10", env=box.b).returncode == 3
    assert "alpha-1f ('weights')" in box.run("status").stdout
    assert "released 1" in box.run("disk", "release").stdout


# Traces: DIBS-REPO-CONFIG
def test_repo_config_adds_resources(box):
    repo = box.tmp / "repo"
    (repo / "sub").mkdir(parents=True)
    (repo / ".dibs.toml").write_text('[resource.testdb]\ncapacity = 3\nmatch = "pytest tests/integration"\n')
    st = box.run("status", cwd=repo / "sub").stdout
    assert "testdb 0/3" in st


# Traces: DIBS-CONFIG-ERRORS
def test_bad_config_names_the_file(box):
    (box.home / "config.toml").write_text("[resource.gpu\n")
    r = box.run("status")
    assert r.returncode != 0 and "config.toml" in r.stderr


# Traces: DIBS-EXAMPLES
def test_every_example_config_loads(box):
    from conftest import ROOT
    for ex in sorted((ROOT / "examples").glob("*.toml")):
        (box.home / "config.toml").write_text(ex.read_text())
        r = box.run("status")
        assert r.returncode == 0, f"{ex.name}: {r.stderr}"


# Traces: DIBS-MEASURE
def test_measure_reports_clean_and_holds_the_machine(box):
    box.config("[resource.gpu]\n")
    p = box.bg("run", "--measure", *sleep_cmd(6))
    assert box.until(lambda: box.held("machine"), 40)
    d = box.hook("python train.py", "sess-bbbb-2222")
    out = p.communicate(timeout=90)[0]
    assert d and "measurement by alpha-1f" in d["hookSpecificOutput"]["permissionDecisionReason"]
    assert "dibs: CLEAN" in out and "dibs: CLEAN after" in out


# Traces: DIBS-HOOK-GUARD
def test_hook_denies_a_wrapped_run_of_a_held_resource(box):
    p = box.bg("run", "gpu", *sleep_cmd(4))
    assert box.until(lambda: box.held("gpu"))
    d = box.hook("dibs run gpu -- python train.py", "sess-bbbb-2222")
    p.wait(timeout=30)
    reason = d["hookSpecificOutput"]["permissionDecisionReason"]
    assert d["hookSpecificOutput"]["permissionDecision"] == "deny"
    assert reason.startswith("WAIT, not a refusal: gpu is held by alpha-1f") and "dibs wait gpu" in reason


# Traces: DIBS-HOOK-GUARD
def test_hook_denies_an_unwrapped_command_matching_the_pattern(box):
    box.config("[resource.gpu]\nmatch = '\\bollama\\b'\n")
    p = box.bg("run", "gpu", *sleep_cmd(4))
    assert box.until(lambda: box.held("gpu"))
    denied = box.hook("ollama run llama3", "sess-bbbb-2222")
    allowed = box.hook("npm test", "sess-bbbb-2222")
    p.wait(timeout=30)
    assert denied and denied["hookSpecificOutput"]["permissionDecision"] == "deny"
    assert allowed is None


# Traces: DIBS-HOOK-GUARD
def test_hook_lets_reads_dibs_itself_and_your_own_hold_through(box):
    box.config("[resource.gpu]\nmatch = 'ollama'\n")
    box.run("hold", "gpu", "--eta", "5m")
    assert box.hook("ollama run llama3", "sess-aaaa-1111") is None
    assert box.hook("cat ollama.log", "sess-bbbb-2222") is None
    assert box.hook("dibs status", "sess-bbbb-2222") is None
    assert box.hook("dibs wait gpu", "sess-bbbb-2222") is None
    assert box.hook("ollama run llama3", "sess-bbbb-2222") is not None
    box.run("release", "gpu")


# Traces: DIBS-HOOK-GUARD
def test_hook_can_be_turned_off(box):
    box.config("[resource.gpu]\nmatch = 'ollama'\n")
    (box.home / "config.toml").write_text((box.home / "config.toml").read_text().replace("pressure = false",
                                                                                         "pressure = false\nenabled = false"))
    box.run("hold", "gpu", "--eta", "5m")
    assert box.hook("ollama run llama3", "sess-bbbb-2222") is None


# Traces: DIBS-HOOK-PRESSURE
def test_hook_holds_commands_back_under_pressure(box):
    (box.home / "config.toml").write_text("[machine]\nram_floor_gb = 100000\n[hook]\nwait_inline_s = 0\n")
    d = box.hook("npm run build", "sess-bbbb-2222")
    assert d and "under pressure" in d["hookSpecificOutput"]["permissionDecisionReason"]
    assert "dibs wait --pressure" in d["hookSpecificOutput"]["permissionDecisionReason"]


PRESSED = "[machine]\nram_floor_gb = 100000\n[hook]\nwait_inline_s = 0\n"


# Traces: DIBS-HOOK-OVERRIDE
def test_override_passes_pressure_and_an_empty_reason_does_not(box):
    (box.home / "config.toml").write_text(PRESSED)
    assert box.hook("DIBS_OVERRIDE='pod idles at $0.22/hr' ssh host true", "sess-bbbb-2222") is None
    assert box.hook("$env:DIBS_OVERRIDE = 'pod idles'; ssh host true", "sess-bbbb-2222") is None
    for cmd in ("ssh host true", "DIBS_OVERRIDE='' ssh host true", "DIBS_OVERRIDE='  ' ssh host true"):
        d = box.hook(cmd, "sess-bbbb-2222")
        assert d and "under pressure" in d["hookSpecificOutput"]["permissionDecisionReason"], cmd
        assert "DIBS_OVERRIDE=" in d["hookSpecificOutput"]["permissionDecisionReason"]


# Traces: DIBS-HOOK-OVERRIDE
def test_override_is_logged_and_shown_in_status(box):
    (box.home / "config.toml").write_text(PRESSED)
    assert box.hook("DIBS_OVERRIDE='pod idles at $0.22/hr' ssh host true", "sess-bbbb-2222", env=box.b) is None
    lines = (box.home / "state" / "overrides.jsonl").read_text().splitlines()
    assert len(lines) == 1
    entry = json.loads(lines[0])
    assert entry["reason"] == "pod idles at $0.22/hr" and entry["name"] == "bravo-2e"
    assert "bravo-2e" in (st := box.run("status").stdout) and "'pod idles at $0.22/hr'" in st


# Traces: DIBS-HOOK-OVERRIDE
def test_override_does_not_pass_another_sessions_hold(box):
    box.config("[resource.gpu]\nmatch = '\\bollama\\b'\n")
    box.run("hold", "gpu", "--eta", "5m")
    d = box.hook("DIBS_OVERRIDE='pod idles' ollama run llama3", "sess-bbbb-2222")
    box.run("release", "gpu")
    assert d and "gpu is held by alpha-1f" in d["hookSpecificOutput"]["permissionDecisionReason"]


# Traces: DIBS-HOOK-OVERRIDE
def test_override_does_not_pass_a_measurement(box):
    box.config("[resource.gpu]\n")
    p = box.bg("run", "--measure", *sleep_cmd(6))
    assert box.until(lambda: box.held("machine"), 40)
    d = box.hook("DIBS_OVERRIDE='pod idles' python train.py", "sess-bbbb-2222")
    p.communicate(timeout=90)
    assert d and "measurement by alpha-1f" in d["hookSpecificOutput"]["permissionDecisionReason"]


def backdate(box, name, slot=0):
    o = box.owner(name, slot)
    o["t0"], o["eta"] = time.time() - 7200, time.time() - 3600
    (box.home / "state" / "held" / name / str(slot) / "owner.json").write_text(json.dumps(o))


# Traces: DIBS-HOLD-WATCH
def test_watched_hold_outlives_its_eta_until_the_process_exits(box):
    worker = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(600)"])
    try:
        assert "while pid" in box.run("hold", "gpu", "--eta", "5m", "--pid", str(worker.pid)).stdout
        backdate(box, "gpu")
        second = box.bg("run", "gpu", "--", sys.executable, "-c", "print('second ran')", env=box.b)
        time.sleep(2)
        assert second.poll() is None
        worker.kill()
        worker.wait()
        out = second.communicate(timeout=60)[0]
        assert "second ran" in out and "which is gone" in out
    finally:
        worker.kill()


# Traces: DIBS-HOLD-WATCH
def test_hold_refuses_a_pid_that_is_not_running(box):
    dead = subprocess.Popen([sys.executable, "-c", "pass"])
    dead.wait()
    r = box.run("hold", "gpu", "--eta", "5m", "--pid", str(dead.pid))
    assert r.returncode != 0 and "isn't running" in r.stderr + r.stdout and not box.held("gpu")


YIELD_WAIT = """
import os, sys, time
f = os.environ['DIBS_YIELD_FILE']
end = time.time() + 30
while not os.path.exists(f) and time.time() < end:
    time.sleep(0.1)
print('yielded' if os.path.exists(f) else 'no yield')
"""


# Traces: DIBS-PREEMPT
def test_waiter_from_another_session_creates_the_yield_file(box):
    first = box.bg("run", "gpu", "--preemptible", "--", sys.executable, "-c", YIELD_WAIT)
    assert box.until(lambda: box.held("gpu"))
    second = box.bg("run", "gpu", "--", sys.executable, "-c", "print('second ran')", env=box.b)
    out1, out2 = first.communicate(timeout=60)[0], second.communicate(timeout=60)[0]
    assert "yielded" in out1 and "second ran" in out2


# Traces: DIBS-PREEMPT
def test_waiter_from_the_same_session_does_not_ask_for_a_yield(box):
    code = "import os, time; time.sleep(3); print('yielded' if os.path.exists(os.environ['DIBS_YIELD_FILE']) else 'no yield')"
    first = box.bg("run", "gpu", "--preemptible", "--", sys.executable, "-c", code)
    assert box.until(lambda: box.held("gpu"))
    second = box.bg("run", "gpu", "--", sys.executable, "-c", "print('second ran')")
    out1, out2 = first.communicate(timeout=60)[0], second.communicate(timeout=60)[0]
    assert "no yield" in out1 and "second ran" in out2


def test_requeue_without_preemptible_is_refused(box):
    r = box.run("run", "gpu", "--requeue", "--", sys.executable, "-c", "pass")
    assert r.returncode != 0 and "--preemptible" in r.stderr + r.stdout


YIELDER = """
import os, pathlib, sys, time
log = pathlib.Path(sys.argv[1])
first = not log.exists()
with log.open('a') as f:
    f.write('A' + chr(10))
if first:
    y = os.environ['DIBS_YIELD_FILE']
    end = time.time() + 30
    while not os.path.exists(y) and time.time() < end:
        time.sleep(0.1)
    sys.exit(75)
"""


# Traces: DIBS-REQUEUE
def test_requeue_lets_the_waiter_run_before_the_yielder_reruns(box):
    log = box.tmp / "order.log"
    a = box.bg("run", "gpu", "--preemptible", "--requeue", "--", sys.executable, "-c", YIELDER, str(log))
    assert box.until(lambda: log.exists())
    b = box.bg("run", "gpu", "--", sys.executable, "-c", f"open({str(log)!r}, 'a').write('B' + chr(10))", env=box.b)
    a.communicate(timeout=90)
    b.communicate(timeout=90)
    assert a.returncode == 0 and log.read_text().split() == ["A", "B", "A"]
