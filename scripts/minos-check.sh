#!/bin/sh
# Runs a dispatch through a real minosd, minosb and minosa, built from the
# minos source, with a scripted `claude` that speaks stream-json. A second
# broker, as `demo`, posts a correction mid-run; the check is that it reaches
# the agent as a second turn, and that the room carries the agent's replies.
# Run it yourself: the fixture commits in its seed repository.
#
#   MINOS_SRC=~/projects/personal/minos sh scripts/minos-check.sh
set -e
root="$(cd "$(dirname "$0")/.." && pwd)"
src="${MINOS_SRC:-$HOME/projects/personal/minos}"
[ -d "$src/go" ] || { echo "no minos source at $src; set MINOS_SRC" >&2; exit 2; }
cargo build --quiet --manifest-path "$root/Cargo.toml"
tmp="$(mktemp -d)"
mkdir -p "$tmp/minos/bin" "$tmp/minos/run" "$tmp/minos/vfs"
(cd "$src/go" && go build -o "$tmp/minos/bin/" ./cmd/minos ./cmd/minosb ./cmd/minosa ./cmd/minosd)
port="$(python3 -I -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')"
server="http://127.0.0.1:$port"
(
  cd "$tmp/minos"
  MINOS_PORT=$port MINOS_RUN="$tmp/minos/run" MINOS_VFS="$tmp/minos/vfs" \
    MINOS_TIMELINE_DB="$tmp/minos/timeline.db" exec "$tmp/minos/bin/minosd"
) >"$tmp/minos/minosd.log" 2>&1 &
minosd=$!
dir="$tmp/fixture"
sh "$root/scripts/web-fixture.sh" "$dir" >/dev/null
. "$dir/env.sh"
# No passwords set: pma falls back to minosd's development accounts.
export PATH="$tmp/minos/bin:$PATH"
unset MINOS_PASSWORD MINOS_ADMIN_PASSWORD
pma="$root/target/debug/pma"
fails=0
check() {
  if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fails=$((fails + 1)); fi
}
demo=
cleanup() {
  [ -z "$demo" ] || kill "$demo" 2>/dev/null
  kill "$minosd" 2>/dev/null
  wait 2>/dev/null
  rm -rf "$tmp"
}
trap cleanup EXIT

# Each frame on stdin is a turn: the first edits and takes long enough for a
# correction to arrive while it runs.
frames="$dir/frames"
cat > "$dir/bin/claude" <<EOF
#!/bin/sh
n=0
while IFS= read -r frame; do
  n=\$((n + 1))
  printf '%s\n' "\$frame" >> "$frames"
  [ \$n = 1 ] && { echo hi > hello.txt; sleep 8; }
  printf '{"type":"result","subtype":"success","is_error":false,"result":"turn %s done","total_cost_usd":0.%s}\n' \$n \$n
done
EOF
chmod +x "$dir/bin/claude"
# A failed check is counted, not the end of the run.
set +e

for _ in $(seq 50); do
  python3 -I -c "import socket; socket.create_connection(('127.0.0.1', $port), 1)" 2>/dev/null && break
  sleep 0.2
done
"$pma" config minos.server "$server" >/dev/null
"$pma" config minos.linger 3 >/dev/null
"$pma" integrate minos | grep -q 'minos answers'; check "integrate finds the server" $?

"$pma" dispatch alpha --worktree --prompt 'add a greeting' >"$tmp/dispatch.out" 2>&1 &
dispatch=$!
db="$PMA_HOME/projects.db"
id=4
room=
for _ in $(seq 100); do
  room="$(sqlite3 "$db" "select room from runs where id = $id" 2>/dev/null)"
  [ -n "$room" ] && break
  sleep 0.1
done
[ -n "$room" ]; check "the run recorded its room while running" $?

# A person in the room, through a broker of their own.
MINOS_PASSWORD=demo minosb -server "$server" -user demo -room "$room" \
  -socket "$tmp/demo.sock" -events "$tmp/demo.events" >"$tmp/demo.log" 2>&1 &
demo=$!
for _ in $(seq 50); do
  grep -q '"ready"' "$tmp/demo.events" 2>/dev/null && break
  sleep 0.1
done
MINOS_SOCKET="$tmp/demo.sock" minosa say "also say goodbye" >/dev/null
check "a person posts a correction" $?

wait "$dispatch"; check "the dispatch finished: $(tail -3 "$tmp/dispatch.out")" $?
grep -q 'goodbye' "$frames"; check "the correction reached the agent" $?
detail="$("$pma" review "$id")"
printf '%s\n' "$detail" | grep -q "room $room"; check "the review names the room" $?
printf '%s\n' "$detail" | grep -q 'Turn 2: turn 2 done'; check "the review holds both turns" $?
printf '%s\n' "$detail" | grep -q 'ready'; check "the run is ready for review" $?
events="$PMA_HOME/state/runs/$id/minos-1.events"
grep -q '"closed"' "$events"; check "the linger closed the agent's input" $?
MINOS_SOCKET="$tmp/demo.sock" minosa messages | grep -q 'turn 1 done'
check "the room carries the agent's reply" $?
[ "$fails" = 0 ]
