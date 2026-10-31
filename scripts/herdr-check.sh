#!/bin/sh
# Runs a dispatch through the real herdr server, with a fake `claude`, against
# a fresh scratch store, then closes what it opened. Needs a running server.
# Run it yourself: the fixture commits in its seed repository.
set -e
root="$(cd "$(dirname "$0")/.." && pwd)"
cargo build --quiet --manifest-path "$root/Cargo.toml"
herdr workspace list >/dev/null || { echo "no herdr server" >&2; exit 2; }
tmp="$(mktemp -d)"
dir="$tmp/fixture"
sh "$root/scripts/web-fixture.sh" "$dir" >/dev/null
. "$dir/env.sh"
pma="$root/target/debug/pma"
fails=0
check() {
  if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fails=$((fails + 1)); fi
}
workspace() {
  herdr workspace list | python3 -I -c '
import json, sys
for w in json.load(sys.stdin)["result"]["workspaces"]:
    if w.get("label") == "pma: alpha":
        print(w["workspace_id"])'
}
cleanup() {
  id="$(workspace)"
  [ -z "$id" ] || herdr workspace close "$id" >/dev/null
  rm -rf "$tmp"
}
trap cleanup EXIT
# A failed check is counted, not the end of the run.
set +e

"$pma" integrate herdr | grep -q 'herdr answers'; check "integrate finds the server" $?
out="$("$pma" dispatch alpha --worktree --prompt 'add a greeting' 2>&1)" || true
id="$(printf '%s\n' "$out" | sed -n 's/^#\([0-9]*\).*/\1/p' | head -1)"
[ -n "$id" ]; check "the dispatch made a run: $out" $?
[ -n "$(workspace)" ]; check "the project has a workspace" $?
log="$PMA_HOME/state/runs/$id/agent-1.log"
grep -q '"type":"result"' "$log"; check "the log holds the agent's result" $?
detail="$("$pma" review "$id")"
printf '%s\n' "$detail" | grep -q 'greeting'; check "the run reports its summary" $?
pane="$(sqlite3 "$PMA_HOME/projects.db" "select pane from runs where id = $id")"
[ -n "$pane" ]; check "the run recorded its pane" $?
herdr pane get "$pane" >/dev/null 2>&1; check "the pane stays open for review" $?
"$pma" review "$id" --reject >/dev/null
! herdr pane get "$pane" >/dev/null 2>&1; check "the decision closes the pane" $?
[ "$fails" = 0 ]
