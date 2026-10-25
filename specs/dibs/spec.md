# dibs: canonical spec

Prefix: `DIBS`. Home repo: dibs. Tests: `specs/dibs/tests/`. `cargo test --release` runs the unit tests (`unit.rs`); `python -m pytest` from the repo root runs the black-box tests against `target/release/dibs` (or `DIBS_BIN`).

## Resources and locks

### DIBS-ANY-NAME: Any name is a resource
**Applies to:** dibs
**Verification:** test

A resource is any name that matches `[a-z0-9][a-z0-9._-]{0,63}` after lowercasing. It needs no config. `machine` is reserved for `--measure`.

**Acceptance criteria:**
- `dibs run network -- <cmd>` with no config runs `<cmd>`.
- `dibs run GPU! -- <cmd>` exits non-zero with "isn't a resource name".
- `dibs run machine -- <cmd>` exits non-zero with "reserved".

### DIBS-TYPO-GUARD: Near-miss names are refused
**Applies to:** dibs
**Verification:** test

A name that is not declared, not held and not used in the last 90 days, but is within edit distance 1 (names of 4 characters or fewer) or 2 (longer names) of a known name, is refused with "Did you mean '<known>'". `--new` accepts it.

**Acceptance criteria:**
- With `[resource.gpu]` declared, `dibs run gpuu` is refused with "Did you mean 'gpu'"; with `--new` it runs.
- After `dibs run testdb` once, `dibs run testbd` is refused with "Did you mean 'testdb'".

### DIBS-EXCLUSIVE: A held resource makes other sessions wait
**Applies to:** dibs
**Verification:** test

**Acceptance criteria:**
- While session A runs `dibs run gpu -- <long cmd>`, session B's `dibs run gpu -- <cmd>` does not start, prints "held by <A's name>", and runs after A's command exits.

### DIBS-CAPACITY: N holders share a resource
**Applies to:** dibs
**Verification:** test

For exclusive resources (`shared = false`), `capacity = N` in config admits N holders at once. Each holder takes one slot. `status` shows `<name> <held>/<N>` when N > 1.

**Acceptance criteria:**
- With `shared = false` and `capacity = 2`, two runs hold `network` at once, a third waits until one ends, and `status` shows `network 2/2`.

### DIBS-SIBLINGS: Subagents of one session take turns
**Applies to:** dibs
**Verification:** test

Run locks are owned by their process. On exclusive resources, a run lock held by another process of the same session blocks, unless that process is an ancestor of the caller. Shared activity records do not block sibling runs.

**Acceptance criteria:**
- Two `dibs run gpu` commands from one session run one after the other; the second prints "held by <session>/<tag> ('<purpose>')".

### DIBS-HOLD-REENTRANT: A hold covers the whole session
**Applies to:** dibs
**Verification:** test

A `hold` taken by the main agent belongs to the session. Runs, waits and nested `dibs run` calls inside the holder's session, including its subagents, or inside an ancestor's run, go at once. A subagent's hold is narrower: see DIBS-AGENT-HOLD-SCOPE.

**Acceptance criteria:**
- After `dibs hold gpu --eta 5m`, `dibs run gpu -- <cmd>` in the same session runs without waiting, and `dibs wait gpu` prints `FREE: gpu`.
- After the main agent's `dibs hold gpu`, the hook lets a subagent's `ollama run x` through.
- `dibs run gpu -- dibs run gpu -- <cmd>` runs `<cmd>`.

### DIBS-AGENT-IDENTITY: A subagent is told apart from its siblings
**Applies to:** dibs
**Verification:** test

Subagents share their session id. The guard hook sees the calling subagent in the hook payload's `agent_id`; the CLI does not. The CLI reads `DIBS_AGENT` (empty for the main agent) and records it as `agent` in the owner record. For a `dibs hold` run by a subagent, the hook notes the call before it runs and, after it, records the subagent's id on the hold it took. An existing hold, taken before the call began, is left alone.

**Acceptance criteria:**
- With `DIBS_AGENT=a1`, `core.who()["agent"]` is `"a1"`; unset, it is `""`.
- `dibs hold gpu` by subagent `a1` (the hook sees the call before and after) leaves a hold whose owner record has `agent: "a1"`.
- A subagent's `dibs hold gpu` that finds the main agent's hold already in place leaves that hold's owner record without an `agent`.

**Verification (manual):** In a Claude Code session with the plugin enabled, have a subagent run `dibs hold gpu --eta 5m`. `dibs status` shows the hold, and `owner.json` under the dibs home's `state/held/gpu/0/` has the subagent's `agent`. If it has none, the PostToolUse payload lacks `agent_id` or `tool_use_id`.

### DIBS-AGENT-HOLD-SCOPE: A subagent's hold covers only that subagent
**Applies to:** dibs
**Verification:** test

A hold with an `agent` covers that agent only. Its siblings and the main agent wait like another session would, in the hook and in `dibs run`. The holder's own agent, or the main agent of the session, may release or extend it; a sibling may not, and the hook refuses its `dibs release` and `dibs extend` unless `--force`.

**Acceptance criteria:**
- `core.mine(hold, "S", {}, agent)` for a hold of session `S` and agent `a1` is true for `a1` and false for `a2`, for the main agent (`""`), and for another session. For a hold with no agent it is true for every agent of `S`.
- While `a1` holds `gpu`, the hook lets `a1`'s `ollama run x` through and denies `a2`'s and the main agent's.
- While `a1` holds `gpu`, `dibs run gpu -- <cmd>` by `a2` waits until `a1` releases.
- `dibs release gpu` by `a2` prints "only that agent" and the hold stays; by the main agent it releases.
- The hook denies `dibs release gpu` and `dibs extend gpu` by `a2` with "only that agent"; it lets `a1`, the main agent, and `--force` through.

### DIBS-AGENT-STOP-RELEASE: A stopping subagent frees its holds
**Applies to:** dibs
**Verification:** manual

When a subagent stops, the hook ends every hold that subagent took.

**Verification (manual):** Have a subagent run `dibs hold gpu --eta 30m` and finish without releasing. From the main agent, `dibs status` no longer shows `gpu` held. If it still does, the SubagentStop payload lacks `agent_id`.

### DIBS-STALE: Dead and overdue holders are taken over
**Applies to:** dibs
**Verification:** test

A run lock is stale when its process is gone. A hold is stale when its session's Claude process is gone, or at ETA + max(10 min, 25% of the span). The next caller takes over a stale lock and says whose it was.

**Acceptance criteria:**
- A slot whose owner pid has exited is taken over, and stderr names the old holder.
- A hold with an ETA an hour past is taken over.
- `stale_at` is ETA + 600 s for a 10-minute span and ETA + 2000 s for an 8000 s span.
- A hold with a live `--pid` is not taken over at ETA + grace (DIBS-HOLD-WATCH).

### DIBS-RELEASE: Only the holder's session releases, unless forced
**Applies to:** dibs
**Verification:** test

**Acceptance criteria:**
- Session B's `dibs release gpu` on A's hold prints "only that session" and the hold stays; with `--force` it is released.
- A subagent's hold follows DIBS-AGENT-HOLD-SCOPE.
- If a slot changed owner after `release` listed it (the hold went stale and another session took it), `release` leaves the new owner's lock in place.

### DIBS-HOLD-WATCH: A hold can last as long as a process
**Applies to:** dibs
**Verification:** test

`dibs hold <res> --eta <d> --pid <n>` ties the hold to process `n`. While `n` is running the hold is not stale at ETA + grace, so a benchmark that overruns its estimate keeps its lock. When `n` exits the hold is stale at once. A hold whose session's Claude process is gone is still stale. Without `--pid`, DIBS-STALE applies unchanged.

**Acceptance criteria:**
- With `--pid` of a live process and the owner's ETA long past, another session's `run` keeps waiting.
- After that process exits, the waiting `run` takes the slot and stderr names the old holder.
- `--pid` of a process that isn't running is refused and nothing is held.

### DIBS-PREEMPT: A preemptible run is asked to yield when another session queues
**Applies to:** dibs
**Verification:** test

`dibs run <res> --preemptible -- <cmd>` passes `DIBS_YIELD_FILE` (a path that doesn't exist yet) to `<cmd>`. When a session other than the holder's is waiting for a named resource, dibs creates that file; with `--yield-signal` it also sends SIGTERM (Unix) or CTRL_BREAK (Windows). dibs never kills the command: one that ignores the request keeps the lock. `status` marks the holder `[preemptible]`. Not allowed with `--measure`.

**Acceptance criteria:**
- Session B queues for a resource held by session A's preemptible run: the file appears, A's command sees it, and B then runs.
- A waiter from the holder's own session creates no file.
- `--requeue` or `--yield-signal` without `--preemptible` is refused.

**Verification (manual, `--yield-signal`):** run `dibs run gpu --preemptible --yield-signal -- <a script that prints on SIGTERM / CTRL_BREAK>`, queue `dibs run gpu -- echo hi` from another session, and confirm the script prints within a few seconds. On Windows the script must handle CTRL_BREAK (`signal.SIGBREAK` in Python).

### DIBS-REQUEUE: A yielding run reruns after the waiter has had its turn
**Applies to:** dibs
**Verification:** test

With `--preemptible --requeue`, if the command exits 75 dibs releases the lock, waits until a waiting session holds a slot (or no other session waits, or 60 s pass), then queues again and reruns the command. Any other exit code ends `run` and becomes its exit code.

**Acceptance criteria:**
- A's command exits 75 on its first run and 0 on its second; B queued during the first. B's command starts before A's second run, and A's final exit code is 0.

### DIBS-WAIT: `wait` blocks until free and prints one line
**Applies to:** dibs
**Verification:** test

**Acceptance criteria:**
- While A holds `gpu`, B's `dibs wait gpu` blocks, then prints `FREE: gpu` after A's run exits.

## Identity

### DIBS-IDENTITY: Holders show by session name, tag and purpose
**Applies to:** dibs
**Verification:** test

Inside Claude Code, a holder is named by its session's name (from Claude Code's session record, falling back to `claude agents --json`, then to the session id prefix). `--tag` adds a worker name after `/`; it is display only. A subagent with no tag shows the first 7 characters of its `agent`. `--label` is the purpose; without one, `status` shows the command with each path cut to its file name. A label of the form `<session>/<worker>: <purpose>` splits into tag and purpose. Outside Claude Code the name is `DIBS_NAME`, the tag, or `<user> (<folder>)`.

**Acceptance criteria:**
- `status` shows `alpha-1f/d01 ('dataset shards 1-4')` for `--tag d01 --label "dataset shards 1-4"` in session alpha-1f.
- `split_label("alpha-1f/d2w: vax probe build", "alpha-1f")` is `("d2w", "vax probe build")`.
- `short_cmd("C:/Users/x/AppData/Temp/scratch/gen.py --shards 1-4")` is `"gen.py --shards 1-4"`.

## Measurement

### DIBS-MEASURE: `--measure` takes the machine and reports CLEAN or NOISY
**Applies to:** dibs
**Verification:** test

`dibs run --measure -- <cmd>` takes `machine` first, discovers live resource names, drains shared activity and takes declared resources exclusively, waits until probes show no outside work and total CPU is under `quiet_cpu_max`, prints `dibs: CLEAN`, runs `<cmd>` (at normal priority on Windows), then prints `dibs: CLEAN after` or `dibs: NOISY after [...]`. While it runs, the hook holds back every other session's non-trivial command.

**Acceptance criteria:**
- A measured run prints `dibs: CLEAN` and `dibs: CLEAN after`, and during it the hook denies session B's `python train.py` with "measurement by <A's name>".

## Disk

### DIBS-DISK: Disk claims reserve space
**Applies to:** dibs
**Verification:** test

`dibs disk claim <GB>` succeeds only when free space minus other sessions' outstanding claims minus `floor_gb` is at least `<GB>`. A claim with `--path` shrinks as that path fills. Otherwise it prints `WAIT: not enough disk` and exits 3.

**Acceptance criteria:**
- A claim far larger than the disk exits 3 with "WAIT: not enough disk".
- After A claims all but 5 GB, B's `dibs disk check 10` exits 3, and `status` shows A's claim with its purpose.

## Config

### DIBS-REPO-CONFIG: A repo's `.dibs.toml` adds resources
**Applies to:** dibs
**Verification:** test

Config merges defaults, the user config in the dibs home, and the nearest `.dibs.toml` at or above the working directory.

**Acceptance criteria:**
- From a subfolder of a repo whose `.dibs.toml` declares `testdb` with `capacity = 3`, `status` shows `testdb 0/3`.

### DIBS-CONFIG-ERRORS: A bad config names its file
**Applies to:** dibs
**Verification:** test

**Acceptance criteria:**
- A config with a TOML syntax error makes `status` exit non-zero with the file name in the message.

### DIBS-EXAMPLES: Shipped examples load
**Applies to:** dibs
**Verification:** test

**Acceptance criteria:**
- `status` exits 0 with each `examples/*.toml` as the user config.

## Claude Code guard

### DIBS-HOOK-GUARD: The hook holds back colliding commands
**Applies to:** dibs
**Verification:** test

The PreToolUse hook denies a Bash or PowerShell command, with a message that starts `WAIT, not a refusal` and names `dibs wait <res>`, when another session (or a sibling run) holds a full exclusive resource that the command names in a `dibs run`/`dibs hold`, or whose `match` pattern (Rust regex syntax: no lookaround or backreferences, case-insensitive) the command matches. It never blocks plain reads, `dibs` status/wait/release/extend/disk/doctor commands, or the holder's own session's hold. `[hook] enabled = false` turns it off. Any internal error allows the command.

**Acceptance criteria:**
- While A holds gpu, B's `dibs run gpu -- python train.py` is denied with "gpu is held by alpha-1f" and "dibs wait gpu".
- With `match = '\bollama\b'`, B's `ollama run llama3` is denied and `npm test` is allowed.
- With A's hold on gpu: A's `ollama run llama3`, B's `cat ollama.log`, `dibs status` and `dibs wait gpu` are allowed.
- With `enabled = false`, nothing is denied.

### DIBS-HOOK-PRESSURE: The hook holds back commands under pressure
**Applies to:** dibs
**Verification:** test

When `[hook] pressure` is on, the hook denies recognized new heavy launches after observed pressure lasts `pressure_grace_s` (default 15 seconds), while CPU is over `cpu_max`, free RAM under `ram_floor_gb`, or memory commit over `commit_max`, and names `dibs wait --pressure`, unless the command carries an override (DIBS-HOOK-OVERRIDE).

**Acceptance criteria:**
- With `ram_floor_gb = 100000` and `pressure_grace_s = 0`, `npm run build` is denied with "under pressure" and "dibs wait --pressure".

### DIBS-HOOK-OVERRIDE: An override with a reason gets one command past pressure, and is logged
**Applies to:** dibs
**Verification:** test

A command that starts with `DIBS_OVERRIDE='<reason>'` (Bash; double quotes or a bare word also work) or `$env:DIBS_OVERRIDE = '<reason>';` (PowerShell), with a non-empty reason, is not held back by pressure. It is still held back by a `--measure` reservation and by another session's hold on a resource it names or matches. When the override is what let the command through, the hook appends a line (time, session, agent, session name, reason, the first 120 characters of the command) to `state/overrides.jsonl`, and `dibs status` prints the last hour's overrides, newest first. A pressure denial names the override and when it is allowed: waiting costs money, or the user said not to wait.

**Acceptance criteria:**
- With `ram_floor_gb = 100000` and `pressure_grace_s = 0`, `DIBS_OVERRIDE='pod idles at $0.22/hr' npm run build` and `$env:DIBS_OVERRIDE = 'pod idles'; npm run build` are allowed; `npm run build`, `DIBS_OVERRIDE='' npm run build` and `DIBS_OVERRIDE='  ' npm run build` are denied with "under pressure" and "DIBS_OVERRIDE=".
- After the allowed override from session bravo-2e, `overrides.jsonl` has one line with that reason and name, and `dibs status` prints "bravo-2e" and "'pod idles at $0.22/hr'".
- While A holds gpu (`match = '\bollama\b'`), B's `DIBS_OVERRIDE='pod idles' ollama run llama3` is denied with "gpu is held by alpha-1f".
- While A holds the machine with `--measure`, B's `DIBS_OVERRIDE='pod idles' python train.py` is denied with "measurement by alpha-1f".
- The prefix is read only at the start: `ssh h DIBS_OVERRIDE='late'` and `echo DIBS_OVERRIDE=x` carry no override.

### DIBS-PLUGIN: The plugin puts dibs on PATH and runs the hooks
**Applies to:** dibs
**Verification:** manual

**Verification (manual):**
1. `claude -p --plugin-dir <repo> --allowedTools "Bash(dibs:*)" "Run dibs doctor and print its output"`.
2. The output shows the session's name and "hook ran N s ago in this session".
3. Repeat on Windows (Git Bash and PowerShell tool), macOS and Linux.

### DIBS-NO-BINARY: A missing binary never blocks work
**Applies to:** dibs
**Verification:** manual

The plugin's `bin/dibs` launcher runs the build for the OS (`bin/<os>-x64/dibs`), then `target/release/dibs`. With neither, it lets every hook through.

**Verification (manual):**
1. Copy `bin/dibs` alone to an empty folder.
2. `sh dibs hook < /dev/null` exits 0 with no output.
3. `sh dibs session-start` prints one line that says the guard is off and where to get a build, and exits 0.
4. `sh dibs status` exits 127 with the same hint.

### DIBS-HOOK-FAST: The hook costs milliseconds
**Applies to:** dibs
**Verification:** manual

With nothing held, `dibs hook` finishes in under 15 ms (median) when run directly. The plugin's hook command on Windows execs the binary from the one Git Bash that Claude Code already starts, so it adds no process of its own. (The `bin/dibs` launcher starts a second shell and costs about 40 ms more, so hooks.json does not use it on Windows.)

**Verification (manual):**
1. Build with `cargo build --release`.
2. Run `target/release/dibs hook` 20 times with `{"session_id":"x","tool_name":"Bash","tool_input":{"command":"python train.py"}}` on stdin, and take the median wall time. Expect under 15 ms.
3. Time `bash -c true` the same way, then the `command` string from hooks/hooks.json (with `CLAUDE_PLUGIN_ROOT` set) through `bash -c`. Expect the hook command within 15 ms of `bash -c true`.

### DIBS-PROBES: Probes see work outside dibs
**Applies to:** dibs
**Verification:** manual

**Verification (manual):**
1. With `examples/amd-ryzen-ai.toml` on a Ryzen AI machine, start an NPU model outside dibs; `dibs status --activity` lists it as an NPU context.
2. Start a GPU load outside dibs; `dibs status --activity` lists it with its GPU %.
3. With `net-counters` and a download running, `dibs status --activity` shows "link busy".


### DIBS-NORMAL-SHARING: Share throughput, isolate measurements

CPU, build and network default to shared activity records with no slot limit. Other resources retain capacity enforcement. `resource.<name>.shared` overrides this choice; capacity is ignored while shared. Normal shared runs skip device/activity quiet probes. Benchmarks acquire the machine gate first, discover live resources, drain their records, then wait for quiet. New normal runs and holds respect a benchmark gate. Status labels shared resources and shows their active count.

The hook returns immediately, including when an old config sets `wait_inline_s`. Sustained pressure requires observations spanning `hook.pressure_grace_s` (default 15 seconds), with gaps shorter than 30 seconds. It delays only recognized heavy launches; existing holds no longer exempt new heavy launches. Tests can set a zero grace. This supersedes earlier hook pressure and inline wait behavior.
