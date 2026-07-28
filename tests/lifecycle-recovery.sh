#!/usr/bin/env bash
set -euo pipefail

root=$(git rev-parse --show-toplevel)
name="lifecycle-recovery-$$"
runtime=$(mktemp -d)
log="$runtime/pi.log"
mkfifo "$runtime/input"
exec 3<>"$runtime/input"
pid=

cleanup() {
  if [[ -n "$pid" ]]; then
    kill "$pid" >/dev/null 2>&1 || true
    wait "$pid" >/dev/null 2>&1 || true
  fi
  exec 3>&-
  rm -rf "$runtime"
}
trap cleanup EXIT

env PAJ_AGENT_NAME="$name" \
  pi --mode rpc --no-session --offline --no-extensions -e "$root/extensions/paj" \
  <&3 >"$log" 2>&1 &
pid=$!
sleep 2

old_id=$(paj --json session list | jq -er --arg name "$name" '.[] | select(.name == $name) | .id')
paj session unregister "$old_id"
sleep 3

session=$(paj --json session list | jq -ec --arg name "$name" '.[] | select(.name == $name)')
new_id=$(jq -er .id <<<"$session")
socket=$(jq -er .bridgeSocket <<<"$session")

test "$new_id" != "$old_id"
test -S "$socket"
if grep -q 'message poll failed.*was not found' "$log"; then
  echo "message polling continued after registration loss" >&2
  exit 1
fi

printf 'recovered %s -> %s\n' "$old_id" "$new_id"
