---
name: paj-subagents
description: >
  Delegate finite tasks to new Pi agents, or list, attach to, and stop owned subagents.
---

# Paj Subagents

## Delegate a finite task

1. Establish the task, target project, constraints, and completion condition. Clarify ambiguous
   tasks or projects; follow the target repository's rules.
2. Use `spawn_sub_agent` to create one child per requested agent. A named project selects where the
   new child runs; it is not a request to reuse an existing agent there. Treat “some” or “a few” as
   three unless context specifies another count.
3. Pass `model` only when the user requests a particular model. Otherwise let Paj resolve the
   configured policy. Preserve an explicit provider and any `:thinking` suffix.
4. For a launch-only request, return after successful spawns. When delegation supports a larger
   task, continue independent work or yield for completion reports. Inspect children only when their
   state is needed for a concrete decision or failure diagnosis.

Each child loads the `paj` skill's communication contract. For messaging, completion, or a follow-up
assignment to an owned child, read that skill. Contact unrelated existing agents only when the user
requests it. Spawn new children directly rather than asking another agent to spawn them.

## Project and model selection

`project` accepts an exact path or an unambiguous project reference. `cwd` selects a directory
within that project; a relative `cwd` starts at its root. Omit both to use the current project root.
Resolution fails with candidates rather than picking an arbitrary match.

Model precedence is an explicit spawn model, then the parent-session override, then the configured
family rule, then inheritance when no rule matches. A matched rule with no available target fails
rather than silently switching providers or models. Version selection and thinking level are
separate: automatic routing preserves Pi's configured thinking defaults.

Personal defaults live in `paj.json` in Pi's agent directory. `/subagent-model` shows the resolved
choice; `/subagent-model provider/id[:thinking]` overrides future children in this parent session;
`/subagent-model auto` restores routing. Existing children and the parent's model stay unchanged.

## Inspect, attach, or stop

Use `list_sub_agents` for owned children, their activity, task state, actual model, and attach
commands. `/subagents` exposes the same information to the user. An unconfirmed model is the launch
request, not a model reported by a running child.

For attaching, stopping, or external scripts, use `scripts/tmux-agent` relative to this skill's own
directory. Resolve it to an absolute path before invoking it. Run it without arguments for usage.
The low-level spawn command requires an explicit `--model` to bypass Pi's ordinary startup default;
agent delegation should use `spawn_sub_agent` so policy resolution is applied.

Completed sessions remain attached for explicit follow-up assignments. Give children finite work;
the harness handles their lifetime. `/reload` preserves children, normal parent shutdown stops owned
children, and `paj gc` cleans up orphans.
