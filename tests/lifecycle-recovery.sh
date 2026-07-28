#!/usr/bin/env bash
set -euo pipefail

root=$(git rev-parse --show-toplevel)
name="lifecycle-recovery-$$"

cleanup() {
  paj job interrupt "$name" >/dev/null 2>&1 || true
  sleep 0.2
  paj job remove "$name" >/dev/null 2>&1 || true
}
trap cleanup EXIT

paj job start --name "$name" --cwd "$root" -- \
  env PAJ_AGENT_NAME="$name" \
  pi --mode rpc --no-session --offline --no-extensions -e "$root/extensions/paj" \
  >/dev/null
sleep 2

old_id=$(paj --json session list | jq -er --arg name "$name" '.[] | select(.name == $name) | .id')
paj session unregister "$old_id"
sleep 3

session=$(paj --json session list | jq -ec --arg name "$name" '.[] | select(.name == $name)')
new_id=$(jq -er .id <<<"$session")
socket=$(jq -er .bridgeSocket <<<"$session")

test "$new_id" != "$old_id"
test -S "$socket"
if paj job log "$name" --lines 200 | grep -q 'message poll failed.*was not found'; then
  echo "message polling continued after registration loss" >&2
  exit 1
fi

printf 'recovered %s -> %s\n' "$old_id" "$new_id"
