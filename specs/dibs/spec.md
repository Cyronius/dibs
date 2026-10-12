# dibs: canonical spec

Prefix: `DIBS`. Home repo: dibs. Tests: `specs/dibs/tests/` (`python -m pytest` from the repo root).

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

`capacity = N` in config admits N holders at once. Each holder takes one slot. `status` shows `<name> <held>/<N>` when N > 1.

**Acceptance criteria:**
- With `capacity = 2`, two runs hold `network` at once, a third waits until one ends, and `status` shows `network 2/2`.

### DIBS-SIBLINGS: Subagents of one session take turns
**Applies to:** dibs
**Verification:** test

Run locks are owned by their process. A run lock held by another process of the same session blocks, unless that process is an ancestor of the caller.

**Acceptance criteria:**
- Two `dibs run gpu` commands from one session run one after the other; the second prints "held by <session>/<tag> ('<purpose>')".

### DIBS-HOLD-REENTRANT: A hold covers the whole session
**Applies to:** dibs
**Verification:** test

A `hold` belongs to the session. Runs, waits and nested `dibs run` calls inside the holder's session, or inside an ancestor's run, go at once.

**Acceptance criteria:**
- After `dibs hold gpu --eta 5m`, `dibs run gpu -- <cmd>` in the same session runs without waiting, and `dibs wait gpu` prints `FREE: gpu`.
- `dibs run gpu -- dibs run gpu -- <cmd>` runs `<cmd>`.

### DIBS-STALE: Dead and overdue holders are taken over
**Applies to:** dibs
**Verification:** test

A run lock is stale when its process is gone. A hold is stale when its session's Claude process is gone, or at ETA + max(10 min, 25% of the span). The next caller takes over a stale lock and says whose it was.

**Acceptance criteria:**
- A slot whose owner pid has exited is taken over, and stderr names the old holder.
- A hold with an ETA an hour past is taken over.
- `stale_at` is ETA + 600 s for a 10-minute span and ETA + 2000 s for an 8000 s span.

### DIBS-RELEASE: Only the holder's session releases, unless forced
**Applies to:** dibs
**Verification:** test

**Acceptance criteria:**
- Session B's `dibs release gpu` on A's hold prints "only that session" and the hold stays; with `--force` it is released.

### DIBS-WAIT: `wait` blocks until free and prints one line
**Applies to:** dibs
**Verification:** test

**Acceptance criteria:**
- While A holds `gpu`, B's `dibs wait gpu` blocks, then prints `FREE: gpu` after A's run exits.

## Identity

### DIBS-IDENTITY: Holders show by session name, tag and purpose
**Applies to:** dibs
**Verification:** test

Inside Claude Code, a holder is named by its session's name (from Claude Code's session record, falling back to `claude agents --json`, then to the session id prefix). `--tag` adds a worker name after `/`. `--label` is the purpose; without one, `status` shows the command with each path cut to its file name. A label of the form `<session>/<worker>: <purpose>` splits into tag and purpose. Outside Claude Code the name is `DIBS_NAME`, the tag, or `<user> (<folder>)`.

**Acceptance criteria:**
- `status` shows `alpha-1f/d01 ('dataset shards 1-4')` for `--tag d01 --label "dataset shards 1-4"` in session alpha-1f.
- `split_label("alpha-1f/d2w: vax probe build", "alpha-1f")` is `("d2w", "vax probe build")`.
- `short_cmd("C:/Users/x/AppData/Temp/scratch/gen.py --shards 1-4")` is `"gen.py --shards 1-4"`.

## Measurement

### DIBS-MEASURE: `--measure` takes the machine and reports CLEAN or NOISY
**Applies to:** dibs
**Verification:** test

`dibs run --measure -- <cmd>` takes `machine` and every declared resource exclusively, waits until probes show no outside work and total CPU is under `quiet_cpu_max`, prints `dibs: CLEAN`, runs `<cmd>` (at normal priority on Windows), then prints `dibs: CLEAN after` or `dibs: NOISY after [...]`. While it runs, the hook holds back every other session's non-trivial command.

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

The PreToolUse hook denies a Bash or PowerShell command, with a message that starts `WAIT, not a refusal` and names `dibs wait <res>`, when another session (or a sibling run) holds a full resource that the command names in a `dibs run`/`dibs hold`, or whose `match` pattern the command matches. It never blocks plain reads, `dibs` status/wait/release/extend/disk/doctor commands, or the holder's own session's hold. `[hook] enabled = false` turns it off. Any internal error allows the command.

**Acceptance criteria:**
- While A holds gpu, B's `dibs run gpu -- python train.py` is denied with "gpu is held by alpha-1f" and "dibs wait gpu".
- With `match = '\bollama\b'`, B's `ollama run llama3` is denied and `npm test` is allowed.
- With A's hold on gpu: A's `ollama run llama3`, B's `cat ollama.log`, `dibs status` and `dibs wait gpu` are allowed.
- With `enabled = false`, nothing is denied.

### DIBS-HOOK-PRESSURE: The hook holds back commands under pressure
**Applies to:** dibs
**Verification:** test

When `[hook] pressure` is on and the session holds nothing, the hook denies non-trivial commands while CPU is over `cpu_max`, free RAM under `ram_floor_gb`, or memory commit over `commit_max`, and names `dibs wait --pressure`.

**Acceptance criteria:**
- With `ram_floor_gb = 100000`, `npm run build` is denied with "under pressure" and "dibs wait --pressure".

### DIBS-PLUGIN: The plugin puts dibs on PATH and runs the hooks
**Applies to:** dibs
**Verification:** manual

**Verification (manual):**
1. `claude -p --plugin-dir <repo> --allowedTools "Bash(dibs:*)" "Run dibs doctor and print its output"`.
2. The output shows the session's name and "hook ran N s ago in this session".
3. Repeat on Windows (Git Bash and PowerShell tool), macOS and Linux.

### DIBS-NO-PYTHON: A missing Python never blocks work
**Applies to:** dibs
**Verification:** manual

**Verification (manual):**
1. Set `PATH` so no Python 3.11+ is found and unset `DIBS_PYTHON`.
2. `bin/dibs hook < /dev/null` exits 0 with no output.
3. `bin/dibs session-start` prints one line that says the guard is off and how to fix it, and exits 0.
4. `bin/dibs status` exits 127 with the install hint.

### DIBS-PROBES: Probes see work outside dibs
**Applies to:** dibs
**Verification:** manual

**Verification (manual):**
1. With `examples/amd-ryzen-ai.toml` on a Ryzen AI machine, start an NPU model outside dibs; `dibs status --activity` lists it as an NPU context.
2. Start a GPU load outside dibs; `dibs status --activity` lists it with its GPU %.
3. With `net-counters` and a download running, `dibs status --activity` shows "link busy".
