# paj

`paj` provides local session discovery, agent-to-agent messaging, and an editor bridge for the
[Pi coding agent](https://github.com/badlogic/pi-mono).

## Install

Requires Pi and a Rust toolchain. Subagents also require `tmux` and Python 3.

Install the CLI and the Pi extension/skills separately:

```sh
cargo install --git https://github.com/jlodenius/paj --locked
pi install git:github.com/jlodenius/paj
```

Keep `paj` on your `PATH`, then run `/reload` in already-open Pi sessions.

### NixOS

For NixOS with Home Manager, add the GitHub flake input:

```nix
inputs.paj = {
  url = "github:jlodenius/paj";
  inputs.nixpkgs.follows = "nixpkgs";
};
```

In your Home Manager user configuration, with `inputs` available in the module scope:

```nix
home.packages = [inputs.paj.packages.${pkgs.stdenv.hostPlatform.system}.default];
home.file.".pi/agent/extensions/paj".source = "${inputs.paj}/extensions/paj";
home.file.".pi/agent/skills/paj".source = "${inputs.paj}/skills/paj";
home.file.".pi/agent/skills/paj-subagents".source = "${inputs.paj}/skills/paj-subagents";
```

The flake includes `tmux` and Python 3. Rebuild NixOS, then run `/reload` in Pi. Skip `pi install`;
Home Manager loads the extension and skills.

## Model defaults

Subagents inherit the parent model by default. To route by model family, create
`~/.pi/agent/paj.json` (or `paj.json` under `PI_CODING_AGENT_DIR`):

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

Adjust the family rules to your models. Rules select the latest available child family on the same
provider. Use `/subagent-model` in Pi to inspect the choice.

## Usage

Pi sessions register automatically. Use `/agents all` to find agents,
`/agent-send <agent> <message>` to send a message, and `/subagents` to inspect children.

From the terminal:

```sh
paj session list --all
paj gc --stale-after 60
paj --help
```

Set `PAJ_PROJECT_DIRS` to comma-separated project search roots; the default is `~/Development`.

## Reference

- [Agent communication](skills/paj/SKILL.md)
- [Subagent workflow and model overrides](skills/paj-subagents/SKILL.md)
- [Neovim integration](https://github.com/jlodenius/paj.nvim)
- [Remote control from your phone or browser](https://github.com/jlodenius/paj-rc)

## Development

```sh
cargo fmt --check
cargo test --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
npm test
./tests/lifecycle-recovery.sh
./tests/tmux-agent.sh
```
