# paj

`paj` provides local session discovery, agent-to-agent messaging, and an editor bridge for the [Pi coding agent](https://github.com/badlogic/pi-mono).

## Install

The CLI and the Pi integration are installed separately. These commands install only the `paj` binary:

```sh
nix profile install github:jlodenius/paj
# or, from a source checkout
cargo install --path .
```

For the full setup, also install the Pi package, which loads this repository's extension and skill:

```sh
pi install git:github.com/jlodenius/paj
```

Alternatively, link `extensions/paj`, `skills/paj`, and `skills/paj-subagents` into their matching paths under `~/.pi/agent`. From a checkout, `pi -e .` loads the package temporarily. Every option still requires the `paj` binary on `PATH`. The subagent workflow also requires `tmux` and Python 3; the Nix package propagates both.

## Session discovery

Pi sessions using the Paj extension register automatically. List live sessions in the current project or across all projects:

```sh
paj session list
paj session list --all
paj session show <session-id>
```

The extension sends heartbeats, recovers lost registrations, and unregisters during clean shutdown. Remove stale registrations and tmux subagents orphaned by crashed parent processes with:

```sh
paj gc --stale-after 60
```

Garbage collection only kills `tmux -L paj` sessions named in Paj's private spawn records, and preserves records whose parent PID is alive. It does not inspect or affect unrelated tmux servers or sessions.

For integrations and lifecycle testing, sessions can also be managed directly:

```sh
paj session register --pid "$PID" --name primary
paj session heartbeat <session-id>
paj session rename <session-id> reviewer
paj session status <session-id> busy
paj session unregister <session-id>
```

Commands use `$XDG_RUNTIME_DIR/paj`. Set `PAJ_RUNTIME_DIR` to override the runtime root, primarily for testing.

## Agent messaging

```sh
paj message send <agent> --from <session-id> --text "Please review parser.rs"
paj --json message pending <session-id>
paj message ack <session-id> <message-id>
```

Recipients can be addressed by exact name, Paj registration ID prefix, or exact stable Pi session ID. Messages remain pending until the extension acknowledges delivery; agents should never acknowledge messages manually.

The Pi extension provides:

```text
/agent-name
/agent-rename <new-name>
/agents [all]
/agent-send <agent> <message>
```

`/agent-name` displays the current agent's registered Paj name. `/agent-rename` renames the agent and persists that name as the Pi session name. Changes made with Pi's built-in `/name` command are also reflected in Paj. `/agents` shows idle/busy state, role, and subagent parent metadata. Agents can retrieve their own name through `get_agent_name`, inspect live sessions through `list_agents`, and send messages through `send_agent_message`. Incoming messages are delivered as user messages and queued as follow-ups while the recipient is busy.

## tmux subagents

The `paj-subagents` skill delegates explicit or optional parallel work through its private helper:

```sh
skills/paj-subagents/scripts/tmux-agent spawn --project paj --task "Run the parser tests"
skills/paj-subagents/scripts/tmux-agent spawn --project paj --model anthropic/claude-sonnet-4 --task "Review the parser"
skills/paj-subagents/scripts/tmux-agent spawn --cwd /exact/project/path --task-file /private/task
skills/paj-subagents/scripts/tmux-agent list
skills/paj-subagents/scripts/tmux-agent attach <spawn-id-or-name>
skills/paj-subagents/scripts/tmux-agent stop <spawn-id-or-name>
skills/paj-subagents/scripts/tmux-agent stop --all
```

Sessions are scoped to `tmux -L paj`. `spawn --model MODEL` forwards Pi's model pattern, including `provider/id` and optional `:thinking` syntax, to the child. Spawn output includes the spawn ID, synchronized Pi/Paj agent name, tmux ID, project root, and a copyable `TMUX= tmux -L paj attach-session ...` command that also works from another tmux server. Tasks are copied into mode-0600 runtime files and passed as data, never evaluated as shell. Completed children stay open for follow-up until stopped. Private records under the Paj runtime tree associate each spawn with its stable parent Pi session ID and PID, child identities, task, cwd/root, tmux name, and timestamps.

Inside Pi, `/subagents` and `list_sub_agents` show only active children owned by the current stable Pi session, including starting/idle/busy state and attach commands. On normal session shutdown the extension stops that parent's children. `/reload` preserves them and rebinds ownership after registration. There is intentionally no parent watcher; `paj gc` handles crash recovery.

### Project resolution

`tmux-agent resolve REF` and `paj project resolve REF` use the same implementation. Search roots come from comma-separated `PAJ_PROJECT_DIRS`; unset, empty, and whitespace-only values default to `~/Development`. Each root is trimmed, a leading `~` or `~/` is expanded from `HOME`, missing roots are ignored, and canonical duplicate roots are removed.

Resolution first checks an absolute directory, each search root itself by exact basename, and `ROOT/REF`. Only when there are no direct matches does it recursively search for exact directory names or matching relative path suffixes. Recursive search prunes `.git`, `node_modules`, `.direnv`, and `target`. Canonical paths and containing Git roots are deduplicated, including results reached through overlapping roots. Zero matches fail; multiple distinct project roots fail and print candidates. Paj never selects a fuzzy or arbitrary match.

`--project REF` selects the resolved project root. With `--project`, a relative `--cwd DIR` is resolved from that root; absolute cwd values must still be inside it. When `--cwd` is used alone, its containing Git root (or the directory itself outside Git) becomes the project root.

## Editor and external-client bridge

Each live Pi session exposes a private Unix socket for structured prompts from Neovim and other local clients:

```sh
paj bridge status agent-38ad3abf
paj bridge prompt agent-38ad3abf --prompt "Explain this module"
paj --json bridge prompt agent-38ad3abf --prompt-file request.md
printf '%s' "Review this change" | paj bridge prompt agent-38ad3abf --prompt-stdin
```

Bridge requests emit `accepted`, `delta`, and `complete` JSON events. Only one request can run per Pi session; requests are rejected with a `busy` error while Pi is working. If a bridge client disconnects after acceptance, the extension cancels the Pi turn that it started. Socket paths are advertised by the session registry and removed during clean shutdown or stale-session garbage collection.

## Pi extension

The extension in `extensions/paj` registers sessions, maintains their lifecycle and idle/busy status, delivers messages, hosts the bridge, and exposes the commands and tools described above.

The `paj` executable must be available on `PATH` before loading the extension.

## Development

```sh
cargo fmt --check
cargo test --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
npm test
./tests/lifecycle-recovery.sh
./tests/tmux-agent.sh
```
