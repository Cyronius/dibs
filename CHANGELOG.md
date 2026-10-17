# Changelog

## Unreleased

- A command can carry an override past the pressure check: `DIBS_OVERRIDE='<reason>' <command>` (PowerShell: `$env:DIBS_OVERRIDE = '<reason>'; <command>`). It never gets past a measurement or another session's lock. Each override is logged to `state/overrides.jsonl`, and `dibs status` shows the last hour's. The pressure denial names the override and says when it is allowed.

## 0.3.0

- Rewritten in Rust: one small native binary, no Python. The hook runs in a few milliseconds instead of about 150.
- Builds for Windows, Linux and macOS (x64). The plugin ships them from a `dist` branch; grab a binary from the releases page to use dibs outside Claude Code.
- `match` patterns use Rust regex syntax: no lookaround or backreferences.
- Gone: the Python package, `uv tool install`, `DIBS_PYTHON`, and the `psutil` option.

## 0.2.0

- A subagent's `hold` covers only that subagent. Its siblings and the main agent wait for it, and only the owner or the main agent can release or extend it. A main-agent hold still covers every subagent.
- The guard hook reads the calling subagent from the hook payload, records it on holds a subagent takes, and ends a subagent's holds when it stops (new PostToolUse and SubagentStop hooks).
- `DIBS_AGENT` names an agent for processes the hook doesn't see.

## 0.1.0

First release.

- Any name is a resource: `dibs run gpu -- ...`, `dibs run network -- ...`, `dibs run port-3000 -- ...`.
- `capacity` lets N holders share a resource.
- A new name close to a known one is refused ("did you mean `gpu`?") unless you pass `--new`.
- Holders show by Claude Code session name, with an optional `--tag` for a subagent and `--label` for the purpose.
- Sibling subagents of one session take turns; a session's `hold` covers all of its subagents.
- `--measure` takes the whole machine, waits for quiet, and reports CLEAN or NOISY.
- `dibs disk claim` reserves disk space for big downloads.
- Claude Code plugin: `dibs` on the Bash PATH, a skill, a PreToolUse guard, and a SessionStart priority drop on Windows.
- Probes for work outside dibs: process names, any command (`xrt-smi`, `nvidia-smi`), Windows GPU counters, network throughput.
- Optional config in your dibs home and in a repo's `.dibs.toml`.
