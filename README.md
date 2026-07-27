# paj

`paj` is a local runtime and toolbox for the [Pi coding agent](https://github.com/badlogic/pi-mono). It will provide session discovery, agent messaging, observable background jobs, subagent orchestration, and editor integration.

The current milestone implements the local session registry.

## Install

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

## Pi extension

The extension in `extensions/paj` registers each Pi session automatically, sends a heartbeat every ten seconds, unregisters during clean shutdown, and provides `/agents [all]`.

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
