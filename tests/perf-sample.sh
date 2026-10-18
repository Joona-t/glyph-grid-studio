#!/bin/bash
# tests/perf-sample.sh — XPC-aware CPU sampler for headless renders (v0.2 Phase 0, F1).
#
# Usage: tests/perf-sample.sh <out.ndjson> -- <binary> <args...>
#
# Why: `time`/rusage only see the Tauri host process. WebKit's WebContent and
# GPU processes are launchd-spawned XPC services (ppid 1), so every headless
# number recorded before v0.2 under-counted CPU by ~10x. This wrapper diffs
# `pgrep -f 'WebKit\.(WebContent|GPU)'` before/after launch to find OUR XPC
# pids, samples `ps -o pid,%cpu,time,state,pri,ni` at 1 Hz for the host + XPC
# pids, appends one JSON line per sample to <out.ndjson>, and finishes with a
# summary line (also appended, and printed to stdout as the last line):
#   {"type":"summary","wall_s":..,"app_cpu_s":..,"webcontent_cpu_s":..,"gpu_cpu_s":..,"frames":..,"exit":..}
#
# The wrapped binary's own stdout/stderr pass through untouched.
# Exit status = the binary's exit status (124 on PERF_TIMEOUT).
#
# Env:
#   PERF_TIMEOUT   seconds before the binary is killed (default: none).
#   PERF_INTERVAL  sampling period in seconds (default 1).
#
# Caveats (documented, not hidden):
#   * CPU times come from `ps -o time` and are cumulative at the last sample,
#     so each process is under-counted by at most one interval (we take a
#     final sweep the moment the host exits, which usually closes the gap to
#     ~0.2 s).
#   * If another WebKit-hosting app spawns an XPC process in the same window,
#     the diff picks it up too; every sample carries the `command` path and
#     the per-pid lines let you exclude it by hand.  ITER-049: a single
#     webview owns exactly one WebContent and one GPU pid, so when more than
#     one of either is tracked the summary carries `suspect_foreign_xpc:true`
#     and a warning goes to stderr — the CPU sums are then an over-count.

set -u

usage() { echo "usage: $0 <out.ndjson> -- <binary> <args...>" >&2; exit 2; }

[[ $# -ge 3 ]] || usage
OUT="$1"; shift
[[ "$1" == "--" ]] || usage
shift
[[ $# -ge 1 ]] || usage

INTERVAL="${PERF_INTERVAL:-1}"
TIMEOUT="${PERF_TIMEOUT:-0}"
XPC_RE='WebKit\.(WebContent|GPU)'

now() { perl -MTime::HiRes=time -e 'printf "%.3f\n", time'; }
json_str() { python3 -c 'import json,sys; print(json.dumps(sys.argv[1]))' "$1"; }

mkdir -p "$(dirname "$OUT")"
: > "$OUT"

# `--frames N` is the one arg worth carrying into the summary (per-frame
# CPU is the number the perf tables divide by). Absent → null.
FRAMES=null
prev=""
for a in "$@"; do
  if [[ "$prev" == "--frames" && "$a" =~ ^[0-9]+$ ]]; then FRAMES="$a"; fi
  prev="$a"
done

# Snapshot the XPC pids that exist BEFORE launch so we can diff them out.
before=" $(pgrep -f "$XPC_RE" 2>/dev/null | tr '\n' ' ') "

T0=$(now)
"$@" &
APP=$!

# role lookup: "pid:role" pairs for the pids we track.
declare -a TRACK=()   # entries "pid role"
TRACK+=("$APP app")
declare -a KNOWN_XPC=()

discover_xpc() {
  local p cmd role
  # ITER-055: `batch --jobs N` forks N child hosts; each is one more app pid
  # (and brings its own WebContent + GPU pair).
  for p in $(pgrep -P "$APP" 2>/dev/null); do
    local seen=0
    for k in "${KNOWN_XPC[@]:-}"; do [[ "$k" == "$p" ]] && seen=1; done
    [[ $seen -eq 1 ]] && continue
    KNOWN_XPC+=("$p")
    TRACK+=("$p app")
  done
  for p in $(pgrep -f "$XPC_RE" 2>/dev/null); do
    [[ "$before" == *" $p "* ]] && continue
    local seen=0
    for k in "${KNOWN_XPC[@]:-}"; do [[ "$k" == "$p" ]] && seen=1; done
    [[ $seen -eq 1 ]] && continue
    cmd=$(ps -o command= -p "$p" 2>/dev/null)
    [[ -z "$cmd" ]] && continue
    case "$cmd" in
      *WebKit.WebContent*) role=webcontent ;;
      *WebKit.GPU*)        role=gpu ;;
      *)                   continue ;;
    esac
    KNOWN_XPC+=("$p")
    TRACK+=("$p $role")
    printf '{"type":"xpc","t":%s,"pid":%s,"role":"%s","command":%s}\n' \
      "$(now)" "$p" "$role" "$(json_str "$cmd")" >> "$OUT"
  done
}

# One sample line per tracked pid that is still alive.
sample() {
  local t pid role line
  t=$(now)
  for entry in "${TRACK[@]}"; do
    pid=${entry%% *}; role=${entry##* }
    line=$(ps -o %cpu=,time=,state=,pri=,ni= -p "$pid" 2>/dev/null | head -1)
    [[ -z "$line" ]] && continue
    set -- $line
    printf '{"type":"sample","t":%s,"pid":%s,"role":"%s","pcpu":%s,"cputime":"%s","state":"%s","pri":%s,"ni":%s}\n' \
      "$t" "$pid" "$role" "$1" "$2" "$3" "$4" "$5" >> "$OUT"
  done
}

# Sampling loop: liveness checked every 0.2 s, samples on the interval.
# XPC discovery runs each tick for the first ~10 s (WebContent/GPU come
# up 0.5–3 s after the host), then on every sample.
tick=0
ticks_per_sample=$(python3 -c "import math;print(max(1,int(round($INTERVAL/0.2))))")
EXIT_CODE=""
while kill -0 "$APP" 2>/dev/null; do
  if (( tick % ticks_per_sample == 0 )); then
    discover_xpc
    sample
  elif (( tick < 50 )); then
    discover_xpc
  fi
  if (( TIMEOUT > 0 && SECONDS > TIMEOUT )); then
    echo "perf-sample: timeout after ${TIMEOUT}s — killing pid $APP" >&2
    pkill -TERM -P "$APP" 2>/dev/null   # ITER-055: --jobs children
    kill -TERM "$APP" 2>/dev/null; sleep 1; kill -KILL "$APP" 2>/dev/null
    EXIT_CODE=124
    break
  fi
  sleep 0.2
  tick=$((tick + 1))
done
# Final sweep: the XPC services outlive the host by a few hundred ms —
# grab their last cputime before launchd reaps them.
discover_xpc
sample
if [[ -z "$EXIT_CODE" ]]; then
  wait "$APP"; EXIT_CODE=$?
else
  wait "$APP" 2>/dev/null
fi
T1=$(now)

# Summary: last cumulative cputime per role (sum over pids of that role —
# a batch that respawns WebContent would otherwise lose the first one).
python3 - "$OUT" "$T0" "$T1" "$FRAMES" "$EXIT_CODE" "$APP" <<'PY' | tee -a "$OUT"
import json, sys
out, t0, t1, frames, code, app = sys.argv[1], float(sys.argv[2]), float(sys.argv[3]), sys.argv[4], int(sys.argv[5]), int(sys.argv[6])

def cpu_s(s):
    # ps `time`: M:SS.ss or H:MM:SS(.ss)
    parts = s.split(':')
    total = 0.0
    for p in parts:
        total = total * 60 + float(p)
    return total

last = {}    # pid -> (role, cputime_s)
n = 0
for line in open(out):
    try:
        d = json.loads(line)
    except ValueError:
        continue
    if d.get('type') != 'sample':
        continue
    n += 1
    last[d['pid']] = (d['role'], cpu_s(d['cputime']))

by_role = {'app': 0.0, 'webcontent': 0.0, 'gpu': 0.0}
pids = {'app': [], 'webcontent': [], 'gpu': []}
for pid, (role, c) in last.items():
    by_role[role] = by_role.get(role, 0.0) + c
    pids[role].append(pid)

# ITER-049: one webview == one WebContent + one GPU pid. More than one of
# either means a foreign WebKit app (or a respawn) landed in the diff window
# and the role sums are inflated — flag it rather than report a clean number.
# ITER-055: a `batch --jobs N` parent hosts no webview; each child host does.
hosts = max(1, len(pids['app']) - (1 if len(pids['app']) > 1 else 0))
suspect = len(pids['webcontent']) > hosts or len(pids['gpu']) > hosts
if suspect:
    print(f"perf-sample: WARNING — {len(pids['webcontent'])} WebContent / {len(pids['gpu'])} GPU pids tracked "
          f"for {hosts} webview(s); webcontent_cpu_s/gpu_cpu_s may include a foreign process "
          f"(see the xpc lines in {out})", file=sys.stderr)

summary = {
    'type': 'summary',
    'wall_s': round(t1 - t0, 3),
    'app_cpu_s': round(by_role['app'], 2),
    'webcontent_cpu_s': round(by_role['webcontent'], 2),
    'gpu_cpu_s': round(by_role['gpu'], 2),
    'frames': None if frames == 'null' else int(frames),
    'exit': code,
    'samples': n,
    'pids': pids,
    'suspect_foreign_xpc': suspect,
}
print(json.dumps(summary, separators=(",", ":")))
PY

exit "$EXIT_CODE"
