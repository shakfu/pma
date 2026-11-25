# Shared by rex-check.sh and rex-manual-check.sh; sourced, not run.

rex="${REX_BIN_DIR:+$REX_BIN_DIR/rex}"
[ -x "$rex" ] || rex="$HOME/Library/Application Support/com.superlogical.rex/RexServer/Versions/Current/Rex Server.app/Contents/MacOS/rex"
[ -x "$rex" ] || rex=rex
"$rex" --autostart=false whoami --json >/dev/null || { echo "no Rex server answers" >&2; exit 2; }

# The id of the session labelled $1, or nothing.
session_id() {
  "$rex" ls --json | python3 -I -c '
import json, sys
for s in json.load(sys.stdin)["sessions"]:
    if s.get("label") == sys.argv[1]:
        print(s["session_id"])
        break' "$1"
}

# The title of block $2's tab in session $1.
tab_label() {
  window="$("$rex" block inspect "$2" --json | python3 -I -c 'import json, sys; print(json.load(sys.stdin)["block"]["window_id"])')"
  "$rex" session inspect "$1" --json | python3 -I -c '
import json, sys
for w in json.load(sys.stdin)["windows"]:
    if w["window_id"] == sys.argv[1]:
        print(w.get("label", ""))' "$window"
}

live() {
  "$rex" block inspect "$1" --json >/dev/null 2>&1
}

# Session $1 existed before as $2 (empty if not). A session this check made
# is killed; in one it found, only tabs whose title starts with $3 close.
rex_cleanup() {
  id="$(session_id "$1")"
  [ -n "$id" ] || return 0
  if [ -z "$2" ]; then
    "$rex" kill "$id" >/dev/null
    return 0
  fi
  [ -n "$3" ] || return 0
  "$rex" session inspect "$id" --json | python3 -I -c '
import json, sys
for w in json.load(sys.stdin)["windows"]:
    if w.get("label", "").startswith(sys.argv[1]):
        print(w["window_id"])' "$3" | while read -r w; do
    "$rex" window close "$w" >/dev/null
  done
}
