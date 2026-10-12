import json, os, subprocess, sys, time
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[3]
RUN = ROOT / "src" / "run_dibs.py"
sys.path.insert(0, str(ROOT / "src"))

LAX = """
[machine]
cpu_max = 101
ram_floor_gb = 0
commit_max = 101
quiet_cpu_max = 101
[hook]
pressure = false
wait_inline_s = 0
"""


class Box:
    def __init__(self, tmp: Path):
        self.tmp, self.home, self.claude = tmp, tmp / "home", tmp / "claude"
        (self.claude / "sessions").mkdir(parents=True)
        self.home.mkdir()
        (self.home / "config.toml").write_text(LAX)
        self.sleepers = []
        self.a = self.session("sess-aaaa-1111", "alpha-1f", os.getpid())
        self.b = self.session("sess-bbbb-2222", "bravo-2e", self.sleeper())

    def sleeper(self) -> int:
        p = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(600)"])
        self.sleepers.append(p)
        return p.pid

    def session(self, sid: str, name: str, pid: int) -> dict:
        (self.claude / "sessions" / f"{pid}.json").write_text(json.dumps({"pid": pid, "sessionId": sid, "name": name,
                                                                           "cwd": str(self.tmp)}))
        env = {**os.environ, "DIBS_HOME": str(self.home), "CLAUDE_CONFIG_DIR": str(self.claude),
               "CLAUDE_CODE_SESSION_ID": sid, "CLAUDE_PID": str(pid), "DIBS_AGENTS_LOOKUP": "0",
               "PYTHONIOENCODING": "utf-8"}
        env.pop("DIBS_SESSION", None)
        return env

    def config(self, text: str) -> None:
        (self.home / "config.toml").write_text(LAX + text)

    def run(self, *args, env=None, cwd=None, input=None, timeout=60):
        return subprocess.run([sys.executable, str(RUN), *args], env=env or self.a, cwd=cwd or self.tmp, input=input,
                              capture_output=True, text=True, timeout=timeout)

    def bg(self, *args, env=None):
        return subprocess.Popen([sys.executable, str(RUN), *args], env=env or self.a, cwd=self.tmp, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT)

    def hook(self, command: str, sid: str, cwd=None, env=None):
        data = json.dumps({"session_id": sid, "cwd": str(cwd or self.tmp), "tool_name": "Bash",
                           "tool_input": {"command": command}})
        r = self.run("hook", input=data, env=env)
        return json.loads(r.stdout) if r.stdout.strip() else None

    def until(self, cond, timeout=20.0) -> bool:
        end = time.time() + timeout
        while time.time() < end:
            if cond():
                return True
            time.sleep(0.2)
        return False

    def held(self, name: str) -> bool:
        d = self.home / "state" / "held" / name
        return d.is_dir() and any(p.is_dir() and p.name.isdigit() for p in d.iterdir())

    def close(self):
        for p in self.sleepers:
            p.kill()


def sleep_cmd(s: float) -> list:
    return ["--", sys.executable, "-c", f"import time; time.sleep({s})"]


@pytest.fixture
def box(tmp_path):
    b = Box(tmp_path)
    yield b
    b.close()
