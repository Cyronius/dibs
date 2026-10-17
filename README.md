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

That's it. The plugin puts `dibs` on Claude's PATH, teaches Claude when to use it, and adds the guard. `dibs` is one small native binary with nothing else to install. `dibs doctor` checks everything.

- **Update:** `/plugin marketplace update dibs`
- **Uninstall:** `/plugin uninstall dibs@dibs`, then delete the dibs home folder that `dibs doctor` shows.

**Without Claude Code** (scripts, cron, CI on the same machine): download the binary for your OS from the [releases page](https://github.com/Cyronius/dibs/releases) and put it on your PATH, or build it:

```sh
cargo install --git https://github.com/Cyronius/dibs
```

Builds are x64 for Windows, Linux and macOS. For another architecture, add it to the matrix in [release.yml](.github/workflows/release.yml).

**For a team,** commit this to the repo's `.claude/settings.json`. Claude Code then tells each teammate the plugin is enabled but not installed, and they run one install command.

```json
{
  "extraKnownMarketplaces": {
    "dibs": { "source": { "source": "github", "repo": "Cyronius/dibs" }, "autoUpdate": true }
  },
  "enabledPlugins": { "dibs@dibs": true }
}
```

## Commands

> **Using Claude Code? You can skip this section.** With the plugin installed, you never type these. The skill tells Claude when to wrap its own work in `dibs`, and the guard makes it wait its turn. This table is what Claude runs on your behalf, and what you'd use from scripts, cron jobs or CI outside Claude.

| You want to | Run |
|---|---|
| Use the GPU (or any resource) for one command | `dibs run gpu --label "llama finetune" --eta 15m -- python finetune.py` |
| Use several resources at once | `dibs run gpu npu -- python pipeline.py` |
| Keep a dev server's port | `dibs run port-3000 -- npm run dev` |
| Get a benchmark number you can trust | `dibs run --measure -- python bench.py` (prints CLEAN or NOISY) |
| Keep resources across several commands | `dibs hold gpu --eta 45m`, then `dibs release gpu` |
| Keep a hold while a benchmark runs, however long it takes | `dibs hold --measure --eta 45m --pid <benchmark pid>` (frees when that process exits) |
| Background job that steps aside for others | `dibs run gpu --preemptible --requeue -- python label.py`; when another session queues, dibs creates the file in `$DIBS_YIELD_FILE` (add `--yield-signal` for SIGTERM / CTRL_BREAK). With `--requeue`, exit 75 hands over and reruns afterwards |
| Keep the whole machine for an A/B series | `dibs hold --measure --eta 45m`, runs, `dibs release --measure` |
| Push back your ETA | `dibs extend gpu --eta 20m` |
| Wait until something is free | `dibs wait gpu` (or `--measure`, `--pressure`, `--disk 40`) |
| Reserve disk for a big download | `dibs disk claim 60 --eta 2h --path ~/models/qwen3-32b`, then `dibs disk release` |
| See who holds what | `dibs status` (`--activity` also looks for work outside dibs) |
| Check your setup | `dibs doctor` |
| Lower Claude Code's CPU priority by hand | `dibs nice` (`--all` for every Claude process) |
| Free someone else's stuck hold | `dibs release gpu --force` |

A resource is any name: `gpu`, `npu`, `network`, `testdb`, `port-3000`, `pixel-7`. `--label` says what the run is for, `--tag` names a worker, and `--new` creates a name that looks like a typo of an existing one.

Config is optional, and it's the one part you might still write yourself. It lets you share a resource between N holders (`capacity = 2` for the network), catch commands that forget to ask (`match = '\bollama\b'` for the GPU), or spot work outside dibs (`probe`). Copy from [examples/](examples/): a [general config](examples/config.toml), a [per-repo `.dibs.toml`](examples/repo.dibs.toml) for a test database and a port, and [the Ryzen AI setup](examples/amd-ryzen-ai.toml) dibs grew up on.

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
- Is CPU, free RAM or memory commit past its limit? Then the command waits, unless this session already holds a lock, or the command starts with an override: `DIBS_OVERRIDE='a billed pod is idle' ssh ...` (PowerShell: `$env:DIBS_OVERRIDE = '...'; ...`). An override never gets past a measurement or another session's lock, and `dibs status` shows the last hour's overrides with their reasons.

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
| Linux | `$XDG_STATE_HOME/dibs`, or `~/.local/state/dibs` |

dibs reads `config.toml` there, plus the nearest `.dibs.toml` at or above the working directory. See [examples/](examples/), including [the Ryzen AI setup](examples/amd-ryzen-ai.toml) that dibs grew up on.

| Key | Default | Meaning |
|---|---|---|
| `resource.<name>.capacity` | `1` | How many holders at once. |
| `resource.<name>.match` | none | Regex ([Rust syntax](https://docs.rs/regex-lite): no lookaround or backreferences). The guard holds back matching commands while the resource is full. |
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
| `hook.wait_inline_s` | `240` | The guard holds a command itself when the blocker should clear within this many seconds. |
| `measure.noise_core_pct` | `50` | A process outside the run that uses more than this % of a core makes it NOISY. |

Probes:

| Kind | Example | Sees |
|---|---|---|
| `process` | `{ kind = "process", names = ["ollama", "llama-server"] }` | Named processes that are using CPU. |
| `command` | `{ command = "nvidia-smi --query-compute-apps=pid,process_name --format=csv,noheader", parser = "nvidia" }` | Whatever a vendor tool reports. Parsers: `lines`, `nvidia`, `xrt`. |
| `gpu-counters` | `{ kind = "gpu-counters", min_pct = 20 }` | Per-process GPU load (Windows). |
| `net-counters` | `{ kind = "net-counters", busy_mbps = 400 }` | Total link throughput. |

Names are lowercase letters, digits, `.`, `_` and `-`. A new name one or two letters away from one dibs already knows is refused with "did you mean `gpu`?". Pass `--new` if you really mean it.

Environment: `DIBS_HOME` moves the home folder, `DIBS_SESSION` and `DIBS_NAME` group and name processes outside Claude Code, and `DIBS_AGENT` splits one session into agents.

## How it works

- A lock slot is a directory. `mkdir` is atomic on every OS, so two processes can't both create it. The holder's details sit in an `owner.json` inside.
- A `run` lock belongs to its process and frees when the process exits, even if it crashes.
- A `hold` belongs to the session. It frees on `release`, when the session exits, or at ETA + max(10 minutes, 25% of the span). Nobody stays stuck behind a forgotten hold. A hold taken with `--pid` instead lasts as long as that process, so a benchmark that overruns its ETA keeps the machine.
- Subagents share their session's id, but each `run` is its own process, so siblings still take turns. A `hold` taken by the main agent covers the whole session. A `hold` taken by a subagent covers only that subagent: the hook sees which subagent made each call, records it on the hold, and holds its siblings back. It ends when the subagent stops. Outside the hook, `DIBS_AGENT` names the agent.
- Several resources are always taken in sorted order, so two callers can't deadlock.
- No daemon and no server. One static binary of about 700 KB, starting in milliseconds.

## Limits

- **One machine.** Locks are files on the local disk. They don't coordinate across machines.
- **Cooperative.** A lock binds only processes that use dibs or go through the guard. `match` patterns catch the common commands, not every command.
- **Not a throttle.** `network` and `build` take turns. They don't cap bandwidth or cores.
- **macOS and Linux can't undo a priority drop,** so `--measure` there runs at whatever priority the session has. The quiet-machine check still applies.

## Development

```sh
cargo build --release
cargo test --release                   # unit tests
python -m pip install pytest && python -m pytest   # black-box tests against target/release/dibs
```

Requirements and their tests live in [specs/dibs/](specs/dibs/spec.md). Load the plugin from a checkout with `claude --plugin-dir .`; it runs `target/release/dibs` when no prebuilt binary is present. Set `DIBS_BIN` to test another binary.

## License

MIT
