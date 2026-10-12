---
name: dibs
description: Take turns with other Claude sessions on this machine's shared resources (GPU, NPU, network, test databases, ports, heavy builds, disk) with the `dibs` command. Use before GPU or NPU runs, benchmarks or any number you will report, heavy builds, big downloads, anything a project's .dibs.toml names, when the user says lock, reserve, dibs or "machine busy", and whenever a command comes back "WAIT, not a refusal".
---

# dibs

Several Claude sessions, and the user's own work, share this machine. `dibs` lets them take turns. It is on your PATH. A resource is any lowercase name (`gpu`, `npu`, `network`, `testdb`, `port-3000`). Your session's name is recorded for you, so other sessions and the user can see who holds what.

## Which command

| Work | Run it as |
|---|---|
| A run on one resource | `dibs run gpu --label "<what it's for>" --eta 10m -- <cmd>` |
| A run on several resources | `dibs run gpu npu --label "<what>" --eta 10m -- <cmd>` |
| Any number you will report (tok/s, latency, A/B) | `dibs run --measure --label "<what>" --eta 30m -- <cmd>` |
| A series of commands (an A/B pair, a server plus a client) | `dibs hold gpu --eta 45m --label "<what>"`, the commands, then `dibs release gpu` |
| A series of measured runs | `dibs hold --measure --eta 45m`, each run as `dibs run --measure -- <cmd>`, then `dibs release --measure` |
| A download or build output over 10 GB | `dibs disk claim <GB> --eta 2h --path <target dir>`, then `dibs disk release` |

- `--label` is a short purpose ("dataset shards 1-4"). Waiters and the user see it.
- In a subagent or worker, also pass `--tag <short name>`. Status then shows `<session>/<tag>`.
- Use the names the project uses. `dibs status` and `.dibs.toml` show them. A new name close to a known one is refused ("did you mean `gpu`?"); pass `--new` only if you really mean a new resource.
- `--measure` takes the whole machine. It waits until no other declared work runs and total CPU is low, then prints `dibs: CLEAN` before and `dibs: CLEAN after` or `dibs: NOISY after [...]` after. Rerun a NOISY run. Never report its number.
- Measured and long runs go in a background Bash. Waiting for a quiet machine can take longer than the Bash tool's timeout.
- A `hold` takes its whole set at once. Asking for more while you hold some can deadlock two sessions until an ETA passes.
- Give a real ETA. Run `dibs extend <res> --eta 20m` before you overrun. A hold goes stale at ETA + max(10 min, 25% of the span), or when your session exits.
- Release as soon as the series ends or the user interrupts you.

## When a resource is busy

- `dibs status` shows who holds what, until when, and who waits. `dibs status --activity` also looks for work outside dibs (about 15 s).
- Wait with a background Bash: `dibs wait <res>` (or `dibs wait --measure`, `dibs wait --pressure`, `dibs wait --disk <GB>`). It prints one `FREE:` line and exits, so you get one notification. Tell the user who holds the resource and the ETA.
- Never poll status in a loop or send "are you done?" messages. You may message a holder (the session name before any `/`) to ask to go first, with your reason and ETA. Its user decides.

## "WAIT, not a refusal"

A hook checks Bash and PowerShell commands, except plain reads (`ls`, `cat`, `git status`, ...) and `dibs` itself:

- Another session runs a measurement: every command waits.
- Another session holds a resource and the command names it or matches its `match` pattern: the command waits.
- CPU, free RAM or memory commit is past its limit: the command waits, unless your session holds a lock.

If the blocker should clear within 4 minutes, the hook holds the command and then lets it run. Otherwise you get a message that starts `WAIT, not a refusal`. That is not the user saying no:

1. Run the background wait it names.
2. Tell the user what you are waiting on and until when.
3. When the wait prints `FREE:`, rerun the same command unchanged.

Don't retry before the wait finishes. Never rename, reroute or split a command to get past the check. Reading and editing meanwhile is fine.

Subagents share their session's identity. A sibling's run blocks you like another session's does. A `hold` taken by your session covers all its subagents.
