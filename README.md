# paj

`paj` provides local session discovery, agent-to-agent messaging, and an editor bridge for the [Pi coding agent](https://github.com/badlogic/pi-mono).

## Install

With Nix:

```sh
nix profile install github:jlodenius/paj
```

From a source checkout:

```sh
cargo install --path .
```

## Session discovery

Pi sessions using the Paj extension register automatically. List live sessions in the current project or across all projects:

```sh
paj session list
paj session list --all
paj session show <session-id>
```

The extension sends heartbeats, recovers lost registrations, and unregisters during clean shutdown. Remove stale registrations with:

```sh
paj gc --stale-after 60
```

For integrations and lifecycle testing, sessions can also be managed directly:

```sh
paj session register --pid "$PID" --name primary
paj session heartbeat <session-id>
paj session unregister <session-id>
```

Commands use `$XDG_RUNTIME_DIR/paj`. Set `PAJ_RUNTIME_DIR` to override the runtime root, primarily for testing.

## Agent messaging

```sh
paj message send <agent> --from <session-id> --text "Please review parser.rs"
paj --json message pending <session-id>
paj message ack <session-id> <message-id>
```

Recipients can be addressed by exact name or session ID prefix. Messages remain pending until acknowledged.

The Pi extension provides:

```text
/agents [all]
/agent-send <agent> <message>
```

Agents can send messages through the `send_agent_message` tool. Incoming messages are delivered as user messages and queued as follow-ups while the recipient is busy.

## Editor and external-client bridge

Each live Pi session exposes a private Unix socket for structured prompts from Neovim and other local clients:

```sh
paj bridge status agent-38ad3abf
paj bridge prompt agent-38ad3abf --prompt "Explain this module"
paj --json bridge prompt agent-38ad3abf --prompt-file request.md
```

Bridge requests emit `accepted`, `delta`, and `complete` JSON events. Only one request can run per Pi session; requests are rejected with a `busy` error while Pi is working. Socket paths are advertised by the session registry and removed during clean shutdown or stale-session garbage collection.

## Pi extension

The extension in `extensions/paj` registers sessions, maintains their lifecycle, delivers messages, hosts the bridge, and exposes the commands and tool described above.

Test it directly from this repository:

```sh
pi -e ./extensions/paj
```

The `paj` executable must be available on `PATH` before loading the extension.

## Development

```sh
cargo fmt --check
cargo test --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
./tests/lifecycle-recovery.sh
```
