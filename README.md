# paj

`paj` provides local session discovery, agent-to-agent messaging, and an editor bridge for the
[Pi coding agent](https://github.com/badlogic/pi-mono).

## Install

The CLI and the Pi integration are installed separately. These commands install only the `paj`
binary:

```sh
nix profile install github:jlodenius/paj
# or, from a source checkout
cargo install --path .
```

For the full setup, also install the Pi package, which loads this repository's extension and skill:

```sh
pi install git:github.com/jlodenius/paj
```

Alternatively, link `extensions/paj`, `skills/paj`, and `skills/paj-subagents` into their matching
paths under `~/.pi/agent`. From a checkout, `pi -e .` loads the package temporarily. Every option
still requires the `paj` binary on `PATH`. The subagent workflow also requires `tmux` and Python 3;
the Nix package propagates both.

## Session discovery

Pi sessions using the Paj extension register automatically. List live sessions in the current
project or across all projects:

```sh
paj session list
paj session list --all
paj session show <session-id>
```

The extension sends heartbeats, recovers lost registrations, and unregisters during clean shutdown.
Remove stale registrations and tmux subagents orphaned by crashed parent processes with:

```sh
paj gc --stale-after 60
```

Garbage collection only kills `tmux -L paj` sessions named in Paj's private spawn records, and
preserves records whose parent PID is alive. It does not inspect or affect unrelated tmux servers or
sessions.

For integrations and lifecycle testing, sessions can also be managed directly:

```sh
paj session register --pid "$PID" --name primary
paj session heartbeat <session-id>
paj session rename <session-id> reviewer
paj session status <session-id> busy
paj session unregister <session-id>
```

Commands use `$XDG_RUNTIME_DIR/paj`. Set `PAJ_RUNTIME_DIR` to override the runtime root, primarily
for testing.

## Agent messaging

```sh
paj message send <agent> --from <session-id> --text "Please review parser.rs"
paj --json message pending <session-id>
paj message ack <session-id> <message-id>
```

Recipients can be addressed by exact name, Paj registration ID prefix, or exact stable Pi session
ID. Messages carry a kind (`request`, `question`, `answer`, `update`, or `result`), optional task
ID, and reply/supersession references. Busy sessions retain messages in Paj's inbox. Once idle, the
extension delivers one actionable batch rather than a separate Pi follow-up for every message.
Updates are included on the next turn without independently waking Pi. Final results can wake a
parent once, but are terminal reports, not requests for confirmation.

Replies retain the original conversation's `taskId`. Paj records `senderTaskId` and
`recipientTaskId` separately to bind messages to the participants' assignments. This permits direct
replies between children without delivering answers into a later assignment.

The extension handles transport acknowledgements after handing messages over to Pi. ACK means handed
over, not read or completed. Acknowledged messages remain inspectable with
`paj --json message history <session-id>`. Explicit supersession replaces only the specified pending
message for the same sender, recipient, and task; unrelated messages are preserved.

The Pi extension provides:

```text
/agent-name
/agent-rename <new-name>
/agents [all]
/agent-send <agent> <message>
```

`/agent-name` displays the current agent's registered Paj name. `/agent-rename` renames the agent
and persists that name as the Pi session name. Changes made with Pi's built-in `/name` command are
also reflected in Paj. `/agents` shows idle/busy state, role, and subagent parent metadata. Agents
use `get_agent_name`, `list_agents`, and `send_agent_message`. Incoming batches are marked as agent
communication rather than user instructions. The `paj` skill defines the communication contract.

## tmux subagents

Inside Pi, the `paj-subagents` skill delegates finite work through `spawn_sub_agent`. The tool
resolves the model policy, passes an exact model to the launcher, and returns the child's identity,
requested model, task ID, and attach command. Scripts can use the lower-level helper directly:

```sh
skills/paj-subagents/scripts/tmux-agent spawn --project paj --task "Run the parser tests"
skills/paj-subagents/scripts/tmux-agent spawn --project paj \
  --model anthropic/claude-sonnet-4 --task "Review the parser"
skills/paj-subagents/scripts/tmux-agent spawn --cwd /exact/project/path --task-file /private/task
skills/paj-subagents/scripts/tmux-agent list
skills/paj-subagents/scripts/tmux-agent attach <spawn-id-or-name>
skills/paj-subagents/scripts/tmux-agent stop <spawn-id-or-name>
skills/paj-subagents/scripts/tmux-agent stop --all
```

Sessions are scoped to `tmux -L paj`. The helper's `spawn --model MODEL` forwards Pi's model
pattern, including `provider/id` and optional `:thinking` syntax. Without that flag the low-level
helper uses Pi's normal startup selection, not Paj's routing policy. Use the Pi tool for automatic
routing. `spawn --json` returns the spawn record. Tasks are copied into mode-0600 runtime files and
passed as data, never evaluated as shell. Attach commands use `TMUX= tmux -L paj attach-session ...`
so they also work from another tmux server.

Children finish with `complete_subagent_task`, which records completion and delivers one idempotent
result to the stable parent Pi session ID. Retries preserve the first result. A completed assignment
is separate from Pi's idle/busy activity. Late messages for closed or previous tasks are archived
without restarting the child. Only the owner can use `assign_subagent_task` to give a completed
child a new finite assignment and task ID. Completed tmux sessions remain open until stopped;
closing a task does not kill its process.

`/subagents` and `list_sub_agents` show the current parent's active children, including activity,
task completion, actual provider/model, and attach commands. Model switches inside a child update
its metadata. Before registration, the requested model is labelled unconfirmed; older records
without model metadata show unknown.

Normal parent shutdown stops owned children. `/reload` preserves them and rebinds ownership after
registration. There is no parent watcher; `paj gc` handles crash recovery.

### Model defaults

Personal policy lives in `paj.json` in Pi's agent directory (`~/.pi/agent` by default, or
`PI_CODING_AGENT_DIR`). For example:

```json
{
  "subagents": {
    "model": {
      "rules": [
        { "parentFamily": "astra", "childFamily": "sol" },
        { "parentFamily": "fable", "childFamily": "opus" }
      ],
      "provider": "same",
      "version": "latest",
      "fallback": "inherit"
    }
  }
}
```

This selects the latest available SOL for Astra parents and Opus for Fable parents, on the exact
same provider. Rules are personal configuration, not built-in mappings. Without a configuration
file, children inherit the parent model. Selection uses Pi's available model registry; it does not
make a provider request to verify entitlement or refresh the catalog over the network.

Precedence is explicit spawn model, then parent-session override, then matching family rule, then
inheritance when no rule matches. A matching rule with no available target fails clearly; it never
silently switches provider or falls back to the parent. Canonical GPT and Claude family IDs are
versioned numerically. Preview and ambiguous alias IDs are not automatic latest candidates. An
explicit model must be an exact available provider/model ID or a unique model ID.

```text
/subagent-model
/subagent-model claude-bridge/claude-opus-5
/subagent-model auto
```

The command shows the resolved choice, overrides future children in this parent session, or restores
automatic routing. Overrides are stored with the Pi session and survive reload/resume; they do not
change the parent model, existing children, other sessions, or the global policy file. Automatic
model routing leaves thinking levels to Pi's configured defaults; an explicit model can include a
`:thinking` suffix.

### Project resolution

`tmux-agent resolve REF` and `paj project resolve REF` use the same implementation. Search roots
come from comma-separated `PAJ_PROJECT_DIRS`; unset, empty, and whitespace-only values default to
`~/Development`. Each root is trimmed, a leading `~` or `~/` is expanded from `HOME`, missing roots
are ignored, and canonical duplicate roots are removed.

Resolution first checks an absolute directory, each search root itself by exact basename, and
`ROOT/REF`. Only when there are no direct matches does it recursively search for exact directory
names or matching relative path suffixes. Recursive search prunes `.git`, `node_modules`, `.direnv`,
and `target`. Canonical paths and containing Git roots are deduplicated, including results reached
through overlapping roots. Zero matches fail; multiple distinct project roots fail and print
candidates. Paj never selects a fuzzy or arbitrary match.

`--project REF` selects the resolved project root. With `--project`, a relative `--cwd DIR` is
resolved from that root; absolute cwd values must still be inside it. When `--cwd` is used alone,
its containing Git root (or the directory itself outside Git) becomes the project root.

## Editor and external-client bridge

Each live Pi session exposes a private Unix socket for structured editor requests from Neovim and
other local clients:

```sh
paj bridge status agent-38ad3abf
printf '%s' '{"kind":"followup","question":"Explain further"}' \
  | paj bridge request agent-38ad3abf --request-stdin
paj --json bridge request agent-38ad3abf --request-file request.json
```

Source requests carry `path`, `startLine`, `endLine`, and `content` as structured data. The Pi
extension exposes that source to the agent through the read-only `paj_editor_context` tool rather
than embedding it in a user prompt. Supported request kinds are `query`, `explain`, `review`,
`followup`, and `acceptAction`.

Bridge requests emit `accepted`, `delta`, and `complete` JSON events. Only one request can run per
Pi session; requests are rejected with a `busy` error while Pi is working. If a bridge client
disconnects after acceptance, the extension cancels the Pi turn that it started. Socket paths are
advertised by the session registry and removed during clean shutdown or stale-session garbage
collection.

## Pi extension

The extension in `extensions/paj` registers sessions, maintains their lifecycle and idle/busy
status, delivers messages, hosts the bridge, and exposes the commands and tools described above.

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
