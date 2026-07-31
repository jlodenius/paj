---
name: paj-subagents
description: Spawn and manage tmux-backed Pi subagents for parallel work and reduced parent-session context use.
---

# Paj Subagents

## Spawn requests are literal

When the user says to **spawn**, **start**, **create**, or **launch** agents, create new child agents with this skill's `scripts/tmux-agent spawn` command. A named project specifies where the new children must run; it is not a request to find or message an existing agent in that project.

- Do not substitute an already-running agent for a requested new agent.
- Do not ask another agent to perform the spawn on your behalf.
- Spawn one child per requested agent. Treat “some” or “a few” as three unless context indicates another count.
- Use Paj discovery or messaging only if the user explicitly asks to contact or reuse existing agents.

Use `scripts/tmux-agent` from the directory containing this `SKILL.md` (`paj-subagents`, not the sibling `paj` skill). Before invoking it, resolve that relative path against this skill's own location and use the resulting absolute path. Run `resolve` when a project reference is not already an exact path; never guess among candidates. Before spawning, clarify the task and project when either is ambiguous, and follow the target repository's rules.

## Script reference

| Command | Purpose |
| --- | --- |
| `scripts/tmux-agent resolve PROJECT` | Resolve one exact project or fail with candidates |
| `scripts/tmux-agent spawn [--project REF] [--cwd DIR] [--model MODEL] (--task TEXT \| --task-file FILE)` | Spawn a subagent with a literal or private-file task |
| `scripts/tmux-agent list` | List active children owned by this session |
| `scripts/tmux-agent attach SPAWN_ID_OR_NAME` | Attach to a child session |
| `scripts/tmux-agent stop SPAWN_ID_OR_NAME` | Stop one child |
| `scripts/tmux-agent stop --all` | Stop all children owned by this session |

## Spawning

- `--cwd DIR` selects a directory within the project. With `--project`, a relative directory starts at the project root.
- Pass `--model MODEL` only when requested. Use `provider/id` when specified and preserve any `:thinking` suffix.
- Spawn output includes the child IDs, name, project root, and attach command.

## Completion

Give each child a finite task. Never tell it to wait, sleep, or remain active; the harness keeps completed sessions attached for follow-ups. Children report completion to the stable parent Pi session ID. Do not manually acknowledge Paj messages.
