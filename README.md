# dibs 🙋

**Keep parallel sessions busy: share CPU, build and network capacity, lock exclusive resources, and isolate performance measurements.**

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

I tried rules in CLAUDE.md. An agent in the middle of a task skips a rule nothing enforces. What finally worked was a lock for resources that cannot be shared, a separate policy for performance measurements, and a status line that tells me in plain names who is doing what, and until when. That's dibs.

Normal work can run concurrently while capacity is available. Sustained CPU or memory pressure delays new heavy launches, and benchmark measurements get an isolated machine. On Windows, Claude runs at a lower priority than the things I'm actually touching.

It isn't only for NPUs. If your agents share something that needs exclusive access, like a GPU, a local Postgres, port 3000 or a phone on a USB cable, dibs lets them take turns. CPU, builds and network transfers share capacity by default.

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

Config is optional, and it's the one part you might still write yourself. It lets you share a resource between N holders (`shared = false` and `capacity = 2` for a connection pool), catch commands that forget to ask (`match = '\bollama\b'` for the GPU), or spot work outside dibs (`probe`). Copy from [examples/](examples/): a [general config](examples/config.toml), a [per-repo `.dibs.toml`](examples/repo.dibs.toml) for a test database and a port, and [the Ryzen AI setup](examples/amd-ryzen-ai.toml) dibs grew up on.

## Upgrading to 0.5

CPU, build and network reservations now record concurrent activity rather than filling exclusive slots. This applies even when an older config specifies `capacity` for those resources. To retain the previous slot limit, set `shared = false` explicitly:

```toml
[resource.network]
shared = false
capacity = 2
```

Update all clients that share the same Dibs home. Commands already running keep the policy of the binary they started with; new commands use the updated policy.

GPU/NPU, ports, databases and other resource names retain capacity enforcement by default. Set `shared = true` only when jobs can safely coexist.

Normal commands use `dibs run <resource> -- <command>`. Use `--measure` only for performance timing or throughput. Prepare inputs, download weights, build, train, test correctness and analyze results outside the measured reservation. Release a measured series between those phases.

Hooks now return a blocking reason immediately; the legacy `hook.wait_inline_s` setting has no effect. Pressure applies to recognized new heavy launches after observations span `hook.pressure_grace_s` (15 seconds by default). Lightweight work remains available under pressure. Existing jobs are not paused or killed.

## What it looks like

```
$ dibs status
gpu              npu-kernels-3a ('prefill tiling A/B') until ~14:20 (stale at 14:30) [hold]   waiting: llama-bench-7c
network shared (1) model-zoo-91/d01 ('Qwen3-32B weights') until ~14:05 (stale at 14:15)
npu              free
port-3000        web-ui-a2 ('npm run dev') since 09:12
disk             model-zoo-91/d01 ('Qwen3-32B weights'): 38 of 60 GB outstanding, ETA 15:30 -> D:\models\qwen3-32b
now 13:52        CPU 41%  free RAM 31.0 GB  commit 62%  disk free 212 GB   pressure: ok
```

Holders show by their Claude Code session name, the one you see in the session list. A subagent adds its tag (`model-zoo-91/d01`). The part in quotes is the `--label`, or the command when there is no label.

## How Claude uses it

The plugin adds three things.

**A skill** that tells Claude which commands to wrap, how to give a label and an ETA, and what to do when something is busy.

**Two admission policies**, selected by the existing `--measure` flag:

- Normal runs share CPU, build and network resources. Reservations record activity; the job count does not imply the machine is full. Other resources (GPU/NPU, ports, databases) retain capacity limits. Set `resource.<name>.shared = true` for another shareable resource, or `false` to retain capacity enforcement. Shared resources ignore `capacity` and activity probes during normal admission.
- Benchmarks reserve the machine first, drain declared work (including shared resources discovered from active records), wait for quiet, and check for noise afterward. Use `--measure` only while measuring performance, not for setup, downloads, training, correctness tests or analysis.

**A guard** checks shell commands. Plain reads and Dibs management commands pass. A benchmark reservation blocks competing commands; an exclusive resource blocks commands naming or matching it. Sustained CPU or memory pressure delays recognized heavy launches (wrapped runs, resource matches, build/train/benchmark commands), while lightweight commands remain available. By default pressure needs observations spanning 15 seconds, with no gap of 30 seconds. Undeclared commands cannot always be classified; wrap heavy work in `dibs run`.

The hook returns immediately with the blocker and a background wait command. It never silently holds a tool call. `hook.wait_inline_s` is retained as a legacy config key but has no effect.

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
| `resource.<name>.capacity` | `1` | Maximum holders when `shared = false`; ignored for shared activity. |
| `resource.<name>.shared` | CPU/build/network: `true`; others: `false` | Track concurrent activity without enforcing slot capacity. Benchmarks still drain it. |
| `resource.<name>.match` | none | Regex ([Rust syntax](https://docs.rs/regex-lite): no lookaround or backreferences). The guard holds back matching commands while the resource is full. |
| `resource.<name>.probe` | none | How to see work that doesn't use dibs. See below. |
| `resource.<name>.resident` | `[]` | Processes that keep the device busy while idle, like a loaded model server. |
| `machine.cpu_max` | `90` | Sustained readings above this total CPU % delay new heavy launches. |
| `machine.ram_floor_gb` | `8` | Sustained readings below this free RAM delay new heavy launches. |
| `machine.commit_max` | `95` | Sustained readings above this memory commit % delay new heavy launches (Windows). |
| `machine.quiet_cpu_max` | `25` | `--measure` waits until total CPU is under this. |
| `machine.disk` | home drive, `floor_gb = 15` | `disk claim` always leaves this much free. |
| `machine.lower_claude_priority` | on for Windows | The SessionStart priority drop. |
| `hook.enabled` | `true` | The Claude Code guard. |
| `hook.pressure` | `true` | Whether sustained CPU and RAM pressure delay recognized new heavy launches. |
| `hook.pressure_grace_s` | `15` | Observed overload duration before delaying new heavy work. |
| `hook.wait_inline_s` | `0` | Legacy key; hooks always return immediately. |
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
- Subagents share their session's id, but each `run` is its own process, so siblings take turns on exclusive resources and run concurrently on shared ones. A `hold` taken by the main agent covers the whole session. A `hold` taken by a subagent covers only that subagent: the hook sees which subagent made each call, records it on the hold, and holds its siblings back on exclusive resources. It ends when the subagent stops. Outside the hook, `DIBS_AGENT` names the agent.
- Normal resource sets are taken in sorted order. Measurements take the machine gate first, then drain the resource set. Request all resources together; adding resources while holding others can deadlock callers.
- No daemon and no server. One static binary of about 700 KB, starting in milliseconds.

## Limits

- **One machine.** Locks are files on the local disk. They don't coordinate across machines.
- **Cooperative.** A lock binds only processes that use dibs or go through the guard. `match` patterns catch the common commands, not every command.
- **Not a throttle.** Shared activity records do not cap bandwidth, cores or job counts. Pressure admission checks observed load; dibs does not divide CPU cores between jobs.
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
