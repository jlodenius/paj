---
name: paj-subagents
description: Spawn or delegate work to tmux-backed Pi subagents, including when the user says to use subagents if necessary. Use for explicit spawning, delegation, parallel work, or subagent lifecycle management.
---

# Paj Subagents

Use `scripts/tmux-agent` from this skill directory. Run `resolve` when a project reference is not already an exact path; never guess among candidates. Before spawning, clarify the task and project when either is ambiguous, and follow the target repository's rules.

Pass tasks with `--task` or, for complex text, a private `--task-file`:

```sh
scripts/tmux-agent spawn --project PROJECT --task-file FILE
scripts/tmux-agent list
scripts/tmux-agent attach SPAWN_ID
scripts/tmux-agent stop SPAWN_ID
scripts/tmux-agent stop --all
```

`--cwd DIR` selects a directory within the resolved project; with `--project`, relative cwd values start at the project root. The spawn output includes the stable spawn ID, synchronized Pi/Paj name, tmux ID, project root, and cross-server attach command. Children remain attached to their tmux sessions after completing the initial task so they can receive follow-up messages. They report completion to the stable parent Pi session ID; do not manually acknowledge Paj messages.
