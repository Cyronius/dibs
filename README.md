# dibs 🙋

**Call dibs on the GPU, the NPU, the network or the test database, so your parallel Claude Code sessions take turns instead of colliding.**

```sh
dibs run gpu -- python train.py
```

## Why I built this

I do NPU development on a Ryzen AI laptop. It's one chip with three engines: an XDNA NPU, a Radeon iGPU and 24 CPU threads. They share one power budget, one pool of memory bandwidth, and 88 GB of RAM that is a lot smaller than it sounds.

On a normal day I have eight or nine Claude Code sessions open. One tunes an NPU kernel. One benchmarks a llama.cpp build on the iGPU. One compiles an xclbin in WSL. One downloads 60 GB of weights. They're all good at their jobs, and not one of them knows the others exist.

So this kept happening:

- Two sessions load models onto the NPU at once. One fails. The other gets half the memory it planned for.
- A session proudly reports a speedup. The real cause: another session's compiler finished halfway through its baseline run. The NPU, the iGPU and the CPU all slow each other down, so a number measured next to someone else's work is noise.
- With that many sessions fighting over memory and CPU, the whole machine slows to a crawl. Memory commit creeps to 99% and everything drags, me included.
- I ask "who's on the GPU?" and the answer is `d2w`. That turns out to be a worker inside another session I'd forgotten about.

I tried rules in CLAUDE.md. An agent in the middle of a task skips a rule nothing enforces. What finally worked was a lock for each thing they fight over, a hook that makes them actually wait their turn, and a status line that tells me in plain names who has what, and until when. That's dibs.

Now all of those sessions stay busy at once, and the machine stays snappy. Heavy work waits its turn instead of piling up, the guard holds back new commands while CPU or RAM is maxed out, and Claude runs at a lower priority than the things I'm actually touching.

It isn't only for NPUs. If your agents share anything they can trip over, like a GPU, the network, a local Postgres, port 3000, a phone on a USB cable or a rate-limited API key, dibs lets them take turns.

## Install

In Claude Code:

```
/plugin marketplace add Cyronius/dibs
/plugin install dibs@dibs
```

That's it. The plugin puts `dibs` on Claude's PATH, teaches Claude when to use it, and adds the guard. You need Python 3.11 or newer on your PATH. `dibs doctor` checks everything.

- **Update:** `/plugin marketplace update dibs`
- **Uninstall:** `/plugin uninstall dibs@dibs`, then delete the dibs home folder that `dibs doctor` shows.

**Without Claude Code** (scripts, cron, CI on the same machine):

```sh
uv tool install git+https://github.com/Cyronius/dibs
```

**For a team,** commit this to the repo's `.claude/settings.json`. Claude Code then tells each teammate the plugin is enabled but not installed, and they run one install command.

```json
{
  "extraKnownMarketplaces": {
    "dibs": { "source": { "source": "github", "repo": "Cyronius/dibs" }, "autoUpdate": true }
  },
  "enabledPlugins": { "dibs@dibs": true }
}
```

## Quick start

```sh
dibs run gpu -- python train.py     # hold gpu while the command runs, free it when it exits
dibs status                         # who holds what, until when, who's waiting
dibs wait gpu                       # block until gpu is free, print one line, exit
```

A resource is any name: `gpu`, `npu`, `network`, `testdb`, `port-3000`, `pixel-7`. You don't declare anything first. Config only adds behavior, like letting two holders share a name, or catching commands that forget to ask.

## Examples

### One model on the GPU at a time

```sh
dibs run gpu --label "llama-3-8b finetune" --eta 15m -- python finetune.py --model llama-3-8b
```

A second session that asks for `gpu` waits, and can see who has it:

```
dibs: waiting for gpu, held by ml-train-9c ('llama-3-8b finetune') until ~14:20 (stale at 14:30)
```

Some commands won't ask. Give the resource a pattern, and the guard holds back any matching command while another session has the GPU:

```toml
[resource.gpu]
match = '\b(?:ollama|llama-server|vllm)\b'
```

### Keep agents from eating the network

Two sessions pulling weights at once each get half the link, and your video call gets nothing. Allow two big transfers at a time, and none while the link is already busy:

```toml
[resource.network]
capacity = 2
match = '\b(?:hf download|huggingface-cli download|docker pull|aws s3 cp)\b'
probe = { kind = "net-counters", busy_mbps = 400 }
```

```sh
dibs run network --label "Qwen3-32B weights" -- hf download Qwen/Qwen3-32B --local-dir ~/models/qwen3-32b
```

### A shared local test database

Commit a `.dibs.toml` at the repo root, so every session and every worktree plays by the same rules:

```toml
[resource.testdb]
match = '\b(?:npm run test:integration|pytest tests/integration)\b'
```

### A dev-server port

```sh
dibs run port-3000 -- npm run dev
```

The port stays taken while the server runs. The next session gets "held by web-ui-a2" instead of `EADDRINUSE`.

### Builds that use every core

Three sessions running `cargo build --release` at once each take three times as long, and the machine stops responding.

```toml
[resource.build]
match = '\b(?:cargo build|cmake --build|ninja|msbuild)\b'
```

### A device: emulator, phone, serial port

```sh
dibs run pixel-7 -- flutter test integration_test -d emulator-5554
dibs run com3 -- pio run -t upload --upload-port COM3
```

### A shared, rate-limited API key

```toml
[resource.staging-api]
capacity = 2
```

```sh
dibs run staging-api -- pytest tests/e2e
```

### Benchmarks you can trust

`--measure` takes the whole machine. It waits until nothing else declared is running and total CPU is low, runs your command (at normal priority on Windows), and tells you whether the machine stayed quiet:

```
$ dibs run --measure --label "prefill tiling A" -- python bench.py --variant a
dibs: CLEAN
...benchmark output...
dibs: CLEAN after
```

If anything else woke up during the run, you get `dibs: NOISY after ['link.exe:4412']`. Rerun it, and don't report that number.

For an A/B series, hold the machine across the runs:

```sh
dibs hold --measure --eta 45m
dibs run --measure -- python bench.py --variant a
dibs run --measure -- python bench.py --variant b
dibs release --measure
```

### Disk space for big downloads

Disk is an amount, not a lock. Claim space before a 60 GB download so another session doesn't fill the drive first:

```sh
dibs disk claim 60 --eta 2h --path ~/models/qwen3-32b
hf download Qwen/Qwen3-32B --local-dir ~/models/qwen3-32b
dibs disk release
```

The claim shrinks as the folder fills. Another session's `dibs disk check 40` counts what you still need.

### Without Claude

Scripts, cron jobs and CI runners on the same machine take the same locks:

```sh
dibs run gpu --tag nightly --eta 3h -- python retrain.py
```

## What it looks like

```
$ dibs status
gpu              npu-kernels-3a ('prefill tiling A/B') until ~14:20 (stale at 14:30) [hold]   waiting: llama-bench-7c
network 1/2      model-zoo-91/d01 ('Qwen3-32B weights') until ~14:05 (stale at 14:15)
npu              free
port-3000        web-ui-a2 ('npm run dev') since 09:12
disk             model-zoo-91/d01 ('Qwen3-32B weights'): 38 of 60 GB outstanding, ETA 15:30 -> D:\models\qwen3-32b
now 13:52        CPU 41%  free RAM 31.0 GB  commit 62%  disk free 212 GB   pressure: ok
```

Holders show by their Claude Code session name, the one you see in the session list. A subagent adds its tag (`model-zoo-91/d01`). The part in quotes is the `--label`, or the command when there is no label.

## How Claude uses it

The plugin adds three things.

**A skill** that tells Claude which commands to wrap, how to give a label and an ETA, and what to do when something is busy.

**A guard** (a PreToolUse hook on Bash and PowerShell). Before each command, except plain reads like `ls` and `git status`, it checks:

- Is another session measuring? Then every command waits.
- Does another session hold a resource this command names, or whose `match` pattern it fits? Then this command waits.
- Is CPU, free RAM or memory commit past its limit? Then the command waits, unless this session already holds a lock.

If the blocker should clear within 4 minutes, the guard just holds the command for a moment. Otherwise Claude gets this, and the skill tells it what to do:

```
WAIT, not a refusal: gpu is held by npu-kernels-3a ('prefill tiling A/B') until ~14:20 (stale at 14:30).
Run in a background Bash: dibs wait gpu
When it prints FREE, rerun this exact command unchanged. Tell the user what you're waiting on and until when.
```

**A priority drop** (Windows): each Claude Code process starts at Below Normal priority, so your editor stays responsive while eight agents compile. `--measure` runs its command at normal priority.

Don't want the guard? Set `[hook] enabled = false` and keep the CLI.

## Config

Every key is optional. `dibs doctor` prints where your config lives:

| OS | dibs home (config and lock state) |
|---|---|
| Windows | `%LOCALAPPDATA%\dibs` |
| macOS | `~/Library/Application Support/dibs` |
| Linux | `~/.local/state/dibs` |

dibs reads `config.toml` there, plus the nearest `.dibs.toml` at or above the working directory. See [examples/](examples/), including [the Ryzen AI setup](examples/amd-ryzen-ai.toml) that dibs grew up on.

| Key | Default | Meaning |
|---|---|---|
| `resource.<name>.capacity` | `1` | How many holders at once. |
| `resource.<name>.match` | none | Regex. The guard holds back matching commands while the resource is full. |
| `resource.<name>.probe` | none | How to see work that doesn't use dibs. See below. |
| `resource.<name>.resident` | `[]` | Processes that keep the device busy while idle, like a loaded model server. |
| `machine.cpu_max` | `90` | The guard waits above this total CPU %. |
| `machine.ram_floor_gb` | `8` | The guard waits below this much free RAM. |
| `machine.commit_max` | `95` | The guard waits above this memory commit % (Windows). |
| `machine.quiet_cpu_max` | `25` | `--measure` waits until total CPU is under this. |
| `machine.disk` | home drive, `floor_gb = 15` | `disk claim` always leaves this much free. |
| `machine.lower_claude_priority` | on for Windows | The SessionStart priority drop. |
| `hook.enabled` | `true` | The Claude Code guard. |
| `hook.pressure` | `true` | Whether CPU and RAM pressure hold commands back. |
| `measure.noise_core_pct` | `50` | A process outside the run that uses more than this % of a core makes it NOISY. |

Probes:

| Kind | Example | Sees |
|---|---|---|
| `process` | `{ kind = "process", names = ["ollama", "llama-server"] }` | Named processes that are using CPU. |
| `command` | `{ command = "nvidia-smi --query-compute-apps=pid,process_name --format=csv,noheader", parser = "nvidia" }` | Whatever a vendor tool reports. Parsers: `lines`, `nvidia`, `xrt`. |
| `gpu-counters` | `{ kind = "gpu-counters", min_pct = 20 }` | Per-process GPU load (Windows). |
| `net-counters` | `{ kind = "net-counters", busy_mbps = 400 }` | Total link throughput. |

Names are lowercase letters, digits, `.`, `_` and `-`. A new name one or two letters away from one dibs already knows is refused with "did you mean `gpu`?". Pass `--new` if you really mean it.

Environment: `DIBS_HOME` moves the home folder, `DIBS_PYTHON` picks the interpreter, `DIBS_SESSION` and `DIBS_NAME` group and name processes outside Claude Code.

## How it works

- A lock slot is a directory. `mkdir` is atomic on every OS, so two processes can't both create it. The holder's details sit in an `owner.json` inside.
- A `run` lock belongs to its process and frees when the process exits, even if it crashes.
- A `hold` belongs to the session. It frees on `release`, when the session exits, or at ETA + max(10 minutes, 25% of the span). Nobody stays stuck behind a forgotten hold.
- Subagents share their session's identity, but each `run` is its own process, so siblings still take turns. A `hold` covers the whole session.
- Several resources are always taken in sorted order, so two callers can't deadlock.
- No daemon and no server. Python's standard library only; psutil is used if it happens to be installed.

## Limits

- **One machine.** Locks are files on the local disk. They don't coordinate across machines.
- **Cooperative.** A lock binds only processes that use dibs or go through the guard. `match` patterns catch the common commands, not every command.
- **Not a throttle.** `network` and `build` take turns. They don't cap bandwidth or cores.
- **macOS and Linux can't undo a priority drop,** so `--measure` there runs at whatever priority the session has. The quiet-machine check still applies.

## Development

```sh
python -m pip install pytest
python -m pytest
```

Requirements and their tests live in [specs/dibs/](specs/dibs/spec.md). Load the plugin from a checkout with `claude --plugin-dir .`.

## License

MIT
