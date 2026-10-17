# Plan: pid-tied holds, safe release

**Spec impact:** modifies DIBS-STALE and DIBS-RELEASE; adds DIBS-HOLD-WATCH, DIBS-PREEMPT and DIBS-REQUEUE.

## Problem
A hold goes stale at ETA + max(10 min, 25% of span) even if the benchmark it protects is still running. The next caller then takes the slot and two jobs overlap (`locks.rs:41-42`). Separately, `release` lists holders and then deletes by path (`locks.rs:242`); if the slot is retaken in between, the new holder's lock is deleted.

## Changes

### 1. DIBS-HOLD-WATCH (new): `dibs hold --pid <n>`
- `hold` accepts `--pid <n>` (repeatable not needed). Stored as `watch_pid` in `owner.json`.
- `is_stale` for a hold with `watch_pid`: stale only when the Claude process is gone **or** `watch_pid` is gone. ETA expiry is ignored while `watch_pid` is alive.
- A hold with `watch_pid` that dies is stale immediately (frees without waiting for the ETA).
- Without `--pid`, behavior is unchanged.
- `dibs status` marks watched holds (`[hold, watching pid N]`) and shows no "stale at" time.
- README: document the pattern (`dibs hold --measure --pid $PID --eta 45m`) and that `dibs run --measure` is the alternative for a single command.

### 2. DIBS-STALE (modified)
Add: a hold with a live `watch_pid` is not taken over at ETA + grace.

### 3. DIBS-RELEASE (modified): release can't delete a retaken slot
- `release()` takes `(dir, expected_owner)` pairs; before removing it re-reads `owner.json` and skips if `t0`/`session` differ or the file is gone.
- Removal itself stays rename-then-delete (as `break_lock`) so a retake between check and delete is recoverable.
- Callers: `cmd_release`, `release_agent`, `acquire` error path.

### 4. DIBS-PREEMPT (new): `dibs run <res> --preemptible -- cmd`
Source: field report from a long-running GPU labeller that polled `dibs status` every ~100 s and string-matched `waiting:`; one handoff took 149 s.
- `owner.json` gets `preemptible: true`.
- While the command runs, the `run` process checks `waiters(name)` about once a second. When a waiter from another session appears, it tells the command once:
  - sets `$DIBS_YIELD_FILE` (a path dibs passes in the child's environment) by creating the file;
  - sends SIGTERM (Unix) or CTRL_BREAK (Windows; the child is spawned in its own process group).
- A command that ignores both just keeps the lock; nothing is forced. Waiters from the same session don't trigger it (DIBS-SIBLINGS ordering still applies).
- Decided: the file is the default; the signal is sent only with `--yield-signal`, since an unhandled SIGTERM kills the command.
- Implemented release check as `release_holders` for listing-based callers (`cmd_release`, `release_agent`); `acquire` and `run` release slots they just took, which can't be retaken while their process lives.

### 5. DIBS-REQUEUE (new): `--requeue`
- Only with `--preemptible`. If the command exits 75, `run` releases the lock instead of exiting.
- Guarantee: the waiter acquires before the yielder re-queues. After releasing, `run` registers its waiter file but does not enter the acquire loop until another session holds a slot of the resource, or no other waiter remains, or 60 s pass (so a vanished waiter can't strand it).
- Then it re-acquires normally and reruns the same command. Loops until the command exits with something other than 75; that exit code becomes `run`'s.
- Replaces hand-rolled wrapper loops with "sleep and hope the waiter grabs it first".

## Deferred (same report, not in this plan)
- Showing "holder will yield within ~2 min" in `status` and the hook's WAIT text, plus `--yield-within`, and letting a preemptible holder omit an ETA.
- `dibs status --json`.
- Waiter urgency (`--need`, `--priority`).
- Windows packaging: `bin/dibs` shim fails on Git Bash (MINGW64), and `dibs doctor` says the hook "hasn't run in this session yet".

## Tests (DIBS-HOLD-WATCH, DIBS-STALE, DIBS-RELEASE, DIBS-PREEMPT, DIBS-REQUEUE)
- DIBS-PREEMPT (test): session A runs `run gpu --preemptible` on a script that exits 0 when `$DIBS_YIELD_FILE` appears; session B `run gpu` starts; the file appears within a few seconds and B gets the lock. Signal delivery is manual: Windows CTRL_BREAK can't be asserted cheaply, so the spec documents a procedure.
- DIBS-REQUEUE (test): A's script exits 75 on yield the first time and 0 the second; B's command records a timestamp. Assert B's run starts before A's second run, and A's final exit code is 0.
- Waiter from A's own session does not trigger the yield.
- `test_dibs.py`: hold with `--eta 1s --pid <live sleep>`; wait past stale_at (use a past-dated `owner.json` or a tiny ETA); session B acquire still waits. Kill the pid; B acquires and stderr names A.
- Same without `--pid`: B takes over (regression for existing behavior).
- `unit.rs`: `release` with a mismatched owner leaves the directory in place; matching owner removes it.

## Out of scope
- Heartbeat/lease renewal; `--pid` for run locks (already pid-tied).
