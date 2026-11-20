#!/bin/sh
# Runs a dispatch through Herdr.app and herdr-core, with a fake `claude`,
# against a fresh scratch store, then closes what it opened. Needs the window
# open. Run it yourself: the fixture commits in its seed repository.
set -e
root="$(cd "$(dirname "$0")/.." && pwd)"
cargo build --quiet --manifest-path "$root/Cargo.toml"
herdr-app status >/dev/null || { echo "Herdr.app's window is not open" >&2; exit 2; }
tmp="$(mktemp -d)"
dir="$tmp/fixture"
sh "$root/scripts/web-fixture.sh" "$dir" >/dev/null
. "$dir/env.sh"
pma="$root/target/debug/pma"
fails=0
check() {
  if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fails=$((fails + 1)); fi
}
space() {
  herdr-app ls | python3 -I -c '
import json, sys
for s in json.load(sys.stdin)["spaces"]:
    if s.get("name") == "pma: alpha":
        print(s["id"])'
}
shown() {
  herdr-app status | python3 -I -c 'import json, sys; print(json.load(sys.stdin)["tab"])'
}
live() {
  hc get "$1" 2>/dev/null | python3 -I -c 'import json, sys; sys.exit(json.load(sys.stdin)["state"] != "live")'
}
cleanup() {
  id="$(space)"
  [ -z "$id" ] || herdr-app do space.close --id "$id" --confirmed true >/dev/null
  rm -rf "$tmp"
}
trap cleanup EXIT
# A failed check is counted, not the end of the run.
set +e

"$pma" integrate herdr-app | grep -q 'Herdr.app answers'; check "integrate finds the window" $?
before="$(shown)"
out="$("$pma" dispatch alpha --worktree --prompt 'add a greeting' 2>&1)" || true
id="$(printf '%s\n' "$out" | sed -n 's/^#\([0-9]*\).*/\1/p' | head -1)"
[ -n "$id" ]; check "the dispatch made a run: $out" $?
[ -n "$(space)" ]; check "the project has a space" $?
[ "$(shown)" = "$before" ]; check "the person's view is left as it was" $?
log="$PMA_HOME/state/runs/$id/agent-1.log"
grep -q '"type":"result"' "$log"; check "the log holds the agent's result" $?
detail="$("$pma" review "$id")"
printf '%s\n' "$detail" | grep -q 'greeting'; check "the run reports its summary" $?
pane="$(sqlite3 "$PMA_HOME/projects.db" "select pane from runs where id = $id")"
case "$pane" in hc:t*) true ;; *) false ;; esac; check "the run recorded its terminal: $pane" $?
live "${pane#hc:}"; check "the terminal stays open for review" $?
"$pma" review "$id" --reject -y >/dev/null
sleep 1
! live "${pane#hc:}"; check "the decision ends the terminal" $?
[ "$fails" = 0 ]
