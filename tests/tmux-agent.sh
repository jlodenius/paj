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

mkdir -p "$projects/group/example/src"
cat >"$bin/pi" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$PWD" "$PAJ_ROLE" "$PAJ_PARENT_PI_SESSION_ID" "$PAJ_SPAWN_ID" "$PAJ_AGENT_NAME" "$PAJ_TASK" >"$PAJ_RUNTIME_DIR/child-observed"
printf '%s\n' "$@" >"$PAJ_RUNTIME_DIR/child-args"
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
if $helper spawn --project example --task '  ' >/dev/null 2>&1; then
  echo "empty task was accepted" >&2
  exit 1
fi
if $helper spawn --project example --model '' --task test >/dev/null 2>&1; then
  echo "empty model was accepted" >&2
  exit 1
fi
output=$($helper spawn --project example --cwd src --model 'openai/gpt-5.4:high' --task 'literal $HOME; $(touch /tmp/paj-should-not-exist)')
spawn_id=$(awk '/^spawnId:/ {print $2}' <<<"$output")
tmux_id=$(awk '/^tmuxId:/ {print $2}' <<<"$output")
test -n "$spawn_id"
test -n "$tmux_id"
grep -q '^attach: TMUX= tmux -L paj attach-session -t =' <<<"$output"

for _ in $(seq 1 50); do [[ -f "$runtime/child-observed" && -f "$runtime/child-args" ]] && break; sleep 0.1; done
test -f "$runtime/child-observed"
test -f "$runtime/child-args"
test ! -e /tmp/paj-should-not-exist
grep -Fqx "$(realpath "$projects/group/example/src")" "$runtime/child-observed"
grep -qx 'subagent' "$runtime/child-observed"
grep -qx 'parent-test' "$runtime/child-observed"
grep -Fqx 'literal $HOME; $(touch /tmp/paj-should-not-exist)' "$runtime/child-observed"
mapfile -t child_args <"$runtime/child-args"
test "${child_args[0]}" = --name
test "${child_args[2]}" = --model
test "${child_args[3]}" = 'openai/gpt-5.4:high'
$helper list | grep -q "$spawn_id"
paj gc --stale-after 60 >/dev/null
tmux -L paj has-session -t "=$tmux_id"
paj --json subagent list --all | grep -q "$spawn_id"
$helper stop "$spawn_id"
! tmux -L paj has-session -t "=$tmux_id" 2>/dev/null
! paj --json subagent list --all | grep -q "$spawn_id"

rm "$runtime/child-observed" "$runtime/child-args"
output=$($helper spawn --project example --task 'default model')
spawn_id=$(awk '/^spawnId:/ {print $2}' <<<"$output")
for _ in $(seq 1 50); do [[ -f "$runtime/child-args" ]] && break; sleep 0.1; done
mapfile -t child_args <"$runtime/child-args"
test "${child_args[0]}" = --name
test "${child_args[2]}" != --model
$helper stop "$spawn_id"

printf 'orphan task' >"$runtime/orphan-task"
orphan=$(paj --json subagent create --parent-pi-session-id dead-parent --parent-pid 4294967295 --cwd "$resolved" --project-root "$resolved" --task-file "$runtime/orphan-task")
orphan_spawn=$(printf '%s' "$orphan" | python3 -c 'import json,sys; print(json.load(sys.stdin)["spawnId"])')
orphan_tmux=$(printf '%s' "$orphan" | python3 -c 'import json,sys; print(json.load(sys.stdin)["tmuxName"])')
tmux -L paj new-session -d -s "$orphan_tmux" sleep 30
paj gc --stale-after 60 >/dev/null
! tmux -L paj has-session -t "=$orphan_tmux" 2>/dev/null
! paj --json subagent list --all | grep -q "$orphan_spawn"

echo "tmux subagent lifecycle passed"
