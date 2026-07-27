# paj

`paj` is a local runtime and toolbox for the [Pi coding agent](https://github.com/badlogic/pi-mono). It will provide session discovery, agent messaging, observable background jobs, subagent orchestration, and editor integration.

The current milestone implements the local session registry.

## Install

With Nix:

```sh
nix profile install github:jlodenius/paj
```

From a source checkout:

```sh
cargo install --path .
```

## Session registry

Register a process as a session:

```sh
paj session register --pid "$PID" --name primary --task "Implement session discovery"
```

The command prints the session ID and generated or supplied name. Pass `--json` for structured output:

```sh
paj --json session register --pid "$PID"
```

Inspect and update sessions:

```sh
paj session list
paj session list --all
paj session show <session-id>
paj session heartbeat <session-id>
paj session unregister <session-id>
paj gc --stale-after 60
```

Commands use `$XDG_RUNTIME_DIR/paj`. Set `PAJ_RUNTIME_DIR` to override the runtime root, primarily for testing.

A session is stale when its heartbeat exceeds the configured threshold or its process no longer exists. Runtime data is private to the current user and is removed naturally when the user runtime directory is cleared.

## Agent messaging

```sh
paj message send <agent> --from <session-id> --text "Please review parser.rs"
paj --json message pending <session-id>
paj message ack <session-id> <message-id>
```

Recipients can be addressed by exact name or session ID prefix. Messages remain pending until the recipient acknowledges them.

## Background jobs

Jobs run in a dedicated tmux server and retain their pane after exit for inspection:

```sh
paj job start --name dev-server -- npm run dev
paj job list
paj job status dev-server
paj job log dev-server --lines 200
paj job log dev-server --follow
paj job send dev-server "r"
paj job interrupt dev-server
paj job stop dev-server
paj job remove dev-server
paj job attach dev-server
```

Job output and metadata live under the Paj runtime directory. Commands are passed directly to tmux without shell interpolation.

## Foreground subagents

Run a clean Pi process for bounded specialist work:

```sh
paj agent run \
  --role review \
  --prompt "Review changes since origin/master" \
  --artifact .agent/subagents/review.md
```

Prompts can also come from `--prompt-file`. `--provider`, `--model`, and `--thinking` select the model configuration. Subagents are instructed to work read-only and receive only the `read` and `bash` tools unless `--allow-write` is explicitly passed.

Paj records the effective prompt, output, stderr, metadata, timing, and exit status under its runtime directory. When an artifact is requested, the CLI prints only the run ID and artifact path.

## Background implementation agents

Spawn an interactive Pi session in an isolated Git worktree:

```sh
paj agent spawn \
  --branch feature/parser \
  --prompt-file .agent/handoffs/parser.md

paj agent list
paj agent attach implementation-12345678
paj agent stop implementation-12345678
paj agent remove implementation-12345678
```

Automatic worktrees live under `$XDG_STATE_HOME/paj/worktrees` so they survive logout and runtime cleanup. Existing branches are reused when available; otherwise Paj creates the requested branch from `HEAD`. `remove` preserves dirty worktrees unless `--force` is passed and never deletes the branch.

The `spawn_implementation_agent` tool sets the current Pi session as the parent. The child is instructed to commit coherent changes and send its parent a completion message.

## Pi extension

The extension in `extensions/paj` registers each Pi session automatically, sends a heartbeat every ten seconds, unregisters during clean shutdown, and provides:

```text
/agents [all]
/agent-send <agent> <message>
```

Agents can send messages through the `send_agent_message` tool. Incoming messages are delivered as user messages and queued as follow-ups when the recipient is busy.

Test it directly from this repository:

```sh
pi -e ./extensions/paj
```

The executable must be available on `PATH` before loading the extension.

## Development

```sh
cargo fmt --check
cargo test --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
```
