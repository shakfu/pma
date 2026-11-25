#!/bin/sh
# Checks with a person at Rex.app what rex-check.sh cannot: an interactive
# run, `H` and take-over, each in front, under the default `rex.sessions
# single`. Uses the scratch fixture and a fake interactive `claude`; closes
# only the `alpha: ` tabs it opened, and the `pma` session only if it made
# it. Switching the app's session needs Remote Control on in Rex.app
# (Settings, General). Run it yourself: the fixture commits in its seed
# repository.
set -e
root="$(cd "$(dirname "$0")/.." && pwd)"
cargo build --quiet --manifest-path "$root/Cargo.toml"
. "$root/scripts/rex-lib.sh"
tmp="$(mktemp -d)"
dir="$tmp/fixture"
sh "$root/scripts/web-fixture.sh" "$dir" >/dev/null
. "$dir/env.sh"
# Headless calls carry `-p` and go to the fixture's agent. Without it, a person
# drives: report blocked, wait for Enter, then make the change.
mkdir -p "$tmp/bin"
cat > "$tmp/bin/claude" <<EOF
#!/bin/sh
case " \$* " in *" -p "*) exec "$dir/bin/claude" "\$@" ;; esac
printf '\033]7501;state=blocked:kind=question:app=fake\033\\\\'
echo "rex manual check: the fake agent waits. Press Enter here."
read _
printf '\033]7501;state=done:app=fake\033\\\\'
echo hi > hello.txt
EOF
chmod +x "$tmp/bin/claude"
PATH="$tmp/bin:$PATH"
export PATH
pma="$root/target/debug/pma"
db="$PMA_HOME/projects.db"
fails=0
check() {
  if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fails=$((fails + 1)); fi
}
ask() {
  printf '%s [y/n] ' "$1"
  read answer </dev/tty
  [ "$answer" = y ]
}
pause() {
  printf '%s Then press Enter. ' "$1"
  read _ </dev/tty
}
had="$(session_id pma)"
cleanup() {
  [ -z "$job" ] || kill "$job" 2>/dev/null || true
  rex_cleanup pma "$had" 'alpha: '
  rm -rf "$tmp"
}
job=
trap cleanup EXIT
set +e

"$pma" integrate rex | grep -q 'Rex answers'; check "integrate finds the server" $?
before="$(sqlite3 "$db" 'select coalesce(max(id), 0) from runs')"

echo
echo "1. An interactive run comes to the front."
pause "In Rex.app, select a session other than 'pma'."
"$pma" dispatch alpha --worktree --prompt 'add a greeting' --interactive >"$tmp/dispatch.out" 2>&1 &
job=$!
id=
for _ in $(seq 50); do
  id="$(sqlite3 "$db" "select id from runs where id > $before and pane is not null order by id limit 1")"
  [ -n "$id" ] && break
  sleep 0.2
done
[ -n "$id" ]; check "the run recorded its block" $?
state=
for _ in $(seq 50); do
  state="$(sqlite3 "$db" "select agent_state from runs where id = ${id:-0}")"
  [ "$state" = blocked ] && break
  sleep 0.2
done
[ "$state" = blocked ]; check "the agent's blocked report reached the run: $state" $?
sed 's/^/  dispatch: /' "$tmp/dispatch.out"
ask "Does Rex.app show session 'pma', on a tab 'alpha: #$id ...' running the fake agent?"; check "the interactive run is in front" $?
pause "Press Enter in that tab, as the fake agent asks."
wait "$job"; code=$?
job=
sed 's/^/  dispatch: /' "$tmp/dispatch.out"
[ "$code" = 0 ]; check "the dispatch ended when the agent did (exit $code)" $?

echo
echo "2. H brings a run's tab to the front."
pause "In Rex.app, select a session other than 'pma'."
echo "pma tui starts now: on run #$id, press H, then q to quit."
pause "Ready?"
"$pma" tui </dev/tty
ask "Did Rex.app switch to run #$id's tab?"; check "H shows the run" $?

echo
echo "3. Take-over opens an agent in front."
pause "In Rex.app, select a session other than 'pma'."
"$pma" take-over "$id" 2>&1 | sed 's/^/  take-over: /'
ask "Did a tab 'alpha: #$id take over' open in front, with the fake agent waiting?"; check "take-over is in front" $?
pause "Press Enter in that tab."

echo
[ "$fails" = 0 ] && echo "all checks passed" || echo "$fails failed"
[ "$fails" = 0 ]
