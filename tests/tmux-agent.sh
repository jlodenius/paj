#!/usr/bin/env bash
set -euo pipefail

root=$(git rev-parse --show-toplevel)
runtime=$(mktemp -d)
projects=$(mktemp -d)
bin=$(mktemp -d)
cleanup() {
  PATH="$bin:$PATH" PAJ_RUNTIME_DIR="$runtime" PAJ_PI_SESSION_ID=parent-test PAJ_PI_SESSION_PID=$$ \
    "$root/skills/paj-subagents/scripts/tmux-agent" stop --all >/dev/null 2>&1 || true
  rm -rf "$runtime" "$projects" "$bin"
}
trap cleanup EXIT

mkdir -p "$projects/group/example"
cat >"$bin/pi" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$PAJ_ROLE" "$PAJ_PARENT_PI_SESSION_ID" "$PAJ_SPAWN_ID" "$PAJ_AGENT_NAME" "$PAJ_TASK" "$*" >"$PAJ_RUNTIME_DIR/child-observed"
sleep 30
EOF
chmod +x "$bin/pi"
ln -s "$root/target/debug/paj" "$bin/paj"

export PATH="$bin:$PATH"
export PAJ_RUNTIME_DIR="$runtime"
export PAJ_PROJECT_DIRS="$projects,$projects/group"
export PAJ_PI_SESSION_ID=parent-test
export PAJ_PI_SESSION_PID=$$
helper=$root/skills/paj-subagents/scripts/tmux-agent

resolved=$($helper resolve example)
test "$resolved" = "$(realpath "$projects/group/example")"
output=$($helper spawn --project example --task 'literal $HOME; $(touch /tmp/paj-should-not-exist)')
spawn_id=$(awk '/^spawnId:/ {print $2}' <<<"$output")
tmux_id=$(awk '/^tmuxId:/ {print $2}' <<<"$output")
test -n "$spawn_id"
test -n "$tmux_id"
grep -q '^attach: TMUX= tmux -L paj attach-session -t =' <<<"$output"

for _ in $(seq 1 50); do [[ -f "$runtime/child-observed" ]] && break; sleep 0.1; done
test -f "$runtime/child-observed"
test ! -e /tmp/paj-should-not-exist
grep -qx 'subagent' "$runtime/child-observed"
grep -qx 'parent-test' "$runtime/child-observed"
grep -Fqx 'literal $HOME; $(touch /tmp/paj-should-not-exist)' "$runtime/child-observed"
$helper list | grep -q "$spawn_id"
$helper stop "$spawn_id"
! tmux -L paj has-session -t "=$tmux_id" 2>/dev/null
! paj --json subagent list --all | grep -q "$spawn_id"

echo "tmux subagent lifecycle passed"
