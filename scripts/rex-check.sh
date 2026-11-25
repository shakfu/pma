#!/bin/sh
# Runs a dispatch through a Rex server, with a fake `claude`, against a fresh
# scratch store: once under `rex.sessions single`, once under `project`. It
# closes only the tabs it opened, and a session only if it made it. Run it
# yourself: the fixture commits in its seed repository.
set -e
root="$(cd "$(dirname "$0")/.." && pwd)"
cargo build --quiet --manifest-path "$root/Cargo.toml"
. "$root/scripts/rex-lib.sh"
tmp="$(mktemp -d)"
dir="$tmp/fixture"
sh "$root/scripts/web-fixture.sh" "$dir" >/dev/null
. "$dir/env.sh"
pma="$root/target/debug/pma"
fails=0
check() {
  if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fails=$((fails + 1)); fi
}
had_single="$(session_id pma)"
had_project="$(session_id 'pma: alpha')"
cleanup() {
  rex_cleanup pma "$had_single" 'alpha: '
  rex_cleanup 'pma: alpha' "$had_project" ''
  rm -rf "$tmp"
}
trap cleanup EXIT
# A failed check is counted, not the end of the run.
set +e

# One dispatch, then checks on where its tab went. $1 is the session label,
# $2 the tab title's expected start.
dispatch_in() {
  out="$("$pma" dispatch alpha --worktree --prompt 'add a greeting' 2>&1)" || true
  id="$(printf '%s\n' "$out" | sed -n 's/^#\([0-9]*\).*/\1/p' | head -1)"
  [ -n "$id" ]; check "the dispatch made a run: $out" $?
  log="$PMA_HOME/state/runs/$id/agent-1.log"
  grep -q '"type":"result"' "$log"; check "the log holds the agent's result" $?
  "$pma" review "$id" | grep -q 'greeting'; check "the run reports its summary" $?
  pane="$(sqlite3 "$PMA_HOME/projects.db" "select pane from runs where id = $id")"
  case "$pane" in rex:session:*/block:*) true ;; *) false ;; esac; check "the run recorded its block: $pane" $?
  session="${pane#rex:}"; session="${session%%/*}"
  block="${pane#*/}"
  [ "$session" = "$(session_id "$1")" ]; check "the tab is in session '$1'" $?
  tab="$(tab_label "$session" "$block")"
  case "$tab" in "$2"*) true ;; *) false ;; esac; check "the tab is titled '$2...': $tab" $?
  live "$block"; check "the block stays open for review" $?
  state="$("$rex" block call -b "$block" program_status | python3 -I -c 'import json, sys; print(json.load(sys.stdin)["records"][0]["state"])')"
  [ "$state" = done ]; check "the block reports done: $state" $?
  "$pma" review "$id" --reject -y >/dev/null
  sleep 1
  ! live "$block"; check "the decision closes the block" $?
}

"$pma" integrate rex | grep -q "tab \`<project>: <task>\` in session \`pma\`"; check "integrate names the single session" $?
echo "-- rex.sessions single (the default)"
dispatch_in pma 'alpha: #'
"$pma" config rex.sessions project >/dev/null
"$pma" integrate | grep -q "session \`pma: <project>\`"; check "integrate names the project session" $?
echo "-- rex.sessions project"
dispatch_in 'pma: alpha' '#'
[ "$fails" = 0 ]
