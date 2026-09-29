#!/usr/bin/env bash
# End-to-end check of pma dispatching through the `sanduk` worker.
#
# Usage: ANTHROPIC_API_KEY=... scripts/sanduk-e2e.sh [--kill-test]
#
#   1. A normal dispatch: the container runs `claude`, the edit lands in the
#      worktree, the cost is read, verify passes on the host, no container is
#      left behind.
#   2. --kill-test: pma is killed while the agent's container runs. Reports
#      every container left behind, running or stopped.
#
# Everything runs in a scratch PMA_HOME and scratch repositories. Spends API
# money: two short runs of $MODEL. Environment:
#   MODEL  model for the agent (default claude-haiku-4-5-20251001)
#   PMA    pma binary (default: built from this checkout)
#   KEEP=1 keep the scratch directory

set -euo pipefail

MODEL=${MODEL:-claude-haiku-4-5-20251001}
KILL_TEST=0
[[ ${1:-} == --kill-test ]] && KILL_TEST=1

fails=0
pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; fails=$((fails + 1)); }
step() { printf '\n== %s\n' "$1"; }
die() { printf 'error: %s\n' "$1" >&2; exit 2; }

# -- preconditions
[[ -n ${ANTHROPIC_API_KEY:-} ]] || die "ANTHROPIC_API_KEY is not set"
command -v sanduk >/dev/null || die "sanduk is not on PATH; cargo install sanduk"
docker info --format '{{.ServerVersion}}' >/dev/null 2>&1 || die "the docker daemon is not reachable"

repo=$(cd "$(dirname "$0")/.." && pwd)
if [[ -z ${PMA:-} ]]; then
    step "building pma from $repo"
    cargo build --release -q --manifest-path "$repo/Cargo.toml"
    PMA=$repo/target/release/pma
fi

scratch=$(mktemp -d "${TMPDIR:-/tmp}/pma-sanduk-e2e.XXXXXX")
export PMA_HOME=$scratch/home
# Identity for the scratch repositories' commits only.
export GIT_AUTHOR_NAME=pma-e2e GIT_AUTHOR_EMAIL=pma-e2e@example.invalid
export GIT_COMMITTER_NAME=pma-e2e GIT_COMMITTER_EMAIL=pma-e2e@example.invalid

sanduk_containers() { docker ps -a --filter name=sanduk- --format '{{.Names}}' | sort; }
before=$(sanduk_containers)
new_containers() { comm -13 <(printf '%s\n' "$before") <(sanduk_containers) | sed '/^$/d'; }

cleanup() {
    # Only containers this script caused; any the user had are left alone.
    local leftover
    leftover=$(new_containers || true)
    if [[ -n $leftover ]]; then
        printf '\nremoving containers this script left: %s\n' "$(echo $leftover)"
        echo "$leftover" | xargs -r docker rm -f >/dev/null
    fi
    if [[ ${KEEP:-0} == 1 || $fails -gt 0 ]]; then
        printf 'scratch kept: %s\n' "$scratch"
    else
        rm -rf "$scratch"
    fi
}
trap cleanup EXIT

# -- a project whose check needs the agent's edit
step "scratch project in $scratch"
seed=$scratch/seed
mkdir -p "$seed" "$scratch/root"
git -C "$seed" init -q -b main
cat >"$seed/TODO.md" <<'EOF'
# TODO

## High

- [ ] add greeting #agent
  create a file named hello.txt containing the word hi, and nothing else
- [ ] add farewell #agent
  create a file named bye.txt containing the word bye, and nothing else
EOF
printf 'test:\n\ttest -f hello.txt\n' >"$seed/Makefile"
git -C "$seed" add .
git -C "$seed" commit -qm init
git clone -q --bare "$seed" "$scratch/origin.git"
git clone -q "$scratch/origin.git" "$scratch/root/demo"

"$PMA" root add "$scratch/root" >/dev/null
"$PMA" project tier 1 demo >/dev/null
"$PMA" config projects.demo.verify "make test" >/dev/null
"$PMA" config timeout 10 >/dev/null
"$PMA" scan --offline >/dev/null

step "building sanduk's claude image (once; not timed)"
sanduk build --agent claude -q

# -- 1. a normal dispatch
step "dispatch through sanduk ($MODEL)"
out=$("$PMA" dispatch --auto -n 1 demo --agent sanduk --model "$MODEL" 2>&1) || true
echo "$out"
[[ $out == *"1 ready, 0 failed"* ]] && pass "the run is ready" || fail "the run is not ready"
cost=$(grep -oE '\$[0-9]+\.[0-9]+ spent' <<<"$out" | tr -dc '0-9.' || true)
if [[ -n $cost ]] && awk -v c="$cost" 'BEGIN { exit !(c > 0) }'; then
    pass "a cost was read: \$$cost"
else
    fail "no cost was read from the agent's stream"
fi

detail=$("$PMA" review 1 2>&1) || true
echo "$detail"
[[ $detail == *"hello.txt"* ]] && pass "the edit is in the worktree pma reviews" || fail "hello.txt is not in the diff"
[[ $detail == *"verify passed"* || $detail == *"every recorded gate is clean"* ]] \
    && pass "verify passed on the host" || fail "verify did not pass"
left=$(new_containers)
[[ -z $left ]] && pass "no container left after the run" || fail "containers left: $(echo $left)"

# -- 2. pma killed mid-run
if [[ $KILL_TEST == 1 ]]; then
    step "kill test: pma is killed while the container runs"
    "$PMA" dispatch --auto -n 1 demo --agent sanduk --model "$MODEL" >"$scratch/kill.log" 2>&1 &
    pid=$!
    # The agent's container, not the relay probe (`sanduk-probe-*`) that
    # runs before it: killing pma during the probe tests nothing.
    agent_re='^sanduk-[0-9a-f]{8}$'
    seen=
    for _ in $(seq 1 240); do
        seen=$(comm -13 <(printf '%s\n' "$before") \
            <(docker ps --filter name=sanduk- --format '{{.Names}}' | sort) | grep -E "$agent_re" || true)
        [[ -n $seen ]] && break
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.5
    done
    if [[ -z $seen ]]; then
        fail "no agent container was running before the dispatch ended; see $scratch/kill.log"
    else
        echo "agent container running: $seen; killing pma ($pid)"
        kill -KILL "$pid"
        wait "$pid" 2>/dev/null || true
        sleep 10
        # Every container this script caused, running or stopped.
        left=$(new_containers)
        if [[ -z $left ]]; then
            pass "nothing is left after pma was killed"
        else
            while read -r name; do
                state=$(docker inspect -f '{{.State.Status}}' "$name" 2>/dev/null || echo gone)
                fail "$name outlived pma: $state"
            done <<<"$left"
        fi
    fi
fi

step "result"
if [[ $fails -eq 0 ]]; then echo "all checks passed"; else echo "$fails check(s) failed"; fi
exit $((fails > 0))
