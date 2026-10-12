import json, subprocess, sys, time

from conftest import sleep_cmd

from dibs import core


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
    from conftest import RUN
    r = box.run("run", "gpu", "--", sys.executable, str(RUN), "run", "gpu", "--", sys.executable, "-c",
                "print('nested ok')", timeout=30)
    assert "nested ok" in r.stdout


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


def test_stale_at_grace_is_ten_minutes_or_a_quarter_of_the_span():
    assert core.stale_at({"t0": 0, "eta": 600}) == 600 + 600
    assert core.stale_at({"t0": 0, "eta": 8000}) == 8000 + 2000


# Traces: DIBS-IDENTITY
def test_status_names_the_session_tag_and_purpose(box):
    p = box.bg("run", "gpu", "--tag", "d01", "--label", "dataset shards 1-4", *sleep_cmd(3))
    assert box.until(lambda: box.held("gpu"))
    time.sleep(0.5)
    st = box.run("status").stdout
    p.wait(timeout=30)
    assert "alpha-1f/d01 ('dataset shards 1-4')" in st


# Traces: DIBS-IDENTITY
def test_label_in_who_what_form_splits_into_tag_and_purpose():
    assert core.split_label("alpha-1f/d2w: vax probe build", "alpha-1f") == ("d2w", "vax probe build")
    assert core.split_label("alpha: tick timing A/B", "alpha-1f") == (None, "tick timing A/B")
    assert core.split_label("A/B series", "alpha-1f") == (None, "A/B series")


# Traces: DIBS-IDENTITY
def test_command_paths_shrink_to_file_names():
    assert core.short_cmd("C:/Users/x/AppData/Temp/scratch/gen.py --shards 1-4") == "gen.py --shards 1-4"
    assert core.short_cmd('"/usr/bin/python3" /tmp/a/b/run.py') == "python3 run.py"


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
    free = shutil.disk_usage(core.Path.home()).free / 2**30
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
