# Changelog

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
