# Shared helpers for the click-through E2E scripts. Source it from a scenario:
#   . "$(dirname "$0")/lib.sh"; start_app --sample; ...; stop_app
#
# Drives the real release binary under Xvfb with xdotool and grabs the screen with
# ImageMagick `import`. The app writes a JSON state summary to $STATE (enabled by
# CASEWORK_STATE_FILE) that `st` / `wait_for` read with jq.

set -u
E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$E2E_DIR/../../.." && pwd)"
BIN="${CASEWORK_BIN:-$ROOT/target/release/casework}"
SHOTS="${SHOTS:-$E2E_DIR/shots}"
WORK="${E2E_WORK:-$(mktemp -d /tmp/casework-e2e.XXXXXX)}"
STATE="$WORK/state.json"
SCENARIO="${SCENARIO:-$(basename "$0" .sh)}"
FAILS=0
mkdir -p "$SHOTS" "$WORK/config"

log() { printf '[%s] %s\n' "$SCENARIO" "$*"; }

# start_app [args...]: Xvfb on a free display, isolated config dir, wait for the window.
start_app() {
    local d=99
    while [ -e "/tmp/.X11-unix/X$d" ] || [ -e "/tmp/.X$d-lock" ]; do d=$((d + 1)); done
    export DISPLAY=":$d"
    Xvfb "$DISPLAY" -screen 0 1440x900x24 -nolisten tcp >"$WORK/xvfb.log" 2>&1 &
    XVFB_PID=$!
    for _ in $(seq 50); do xdpyinfo >/dev/null 2>&1 && break; sleep 0.1; done
    rm -f "$STATE"
    XDG_CONFIG_HOME="$WORK/config" XDG_DATA_HOME="$WORK/data" CASEWORK_STATE_FILE="$STATE" \
        "$BIN" "$@" >"$WORK/app.log" 2>&1 &
    APP_PID=$!
    wait_for '.model_id != null or (.features | length) == 0' 120 || { log "app did not start"; tail -20 "$WORK/app.log"; return 1; }
    WIN=$(xdotool search --sync --name Casework | head -1)
    # no window manager: put the window at the origin so screen = window coordinates
    xdotool windowmove "$WIN" 0 0 windowsize "$WIN" 1440 900 2>/dev/null
    # no window manager either to hand out keyboard focus; a window that maps late
    # (an empty document starts fast) can miss the first request, so insist
    focus_win
    idle
}

focus_win() {
    for _ in $(seq 50); do
        xdotool windowfocus --sync "$WIN" 2>/dev/null
        [ "$(xdotool getwindowfocus 2>/dev/null)" = "$WIN" ] && break
        sleep 0.1
    done
}

stop_app() {
    [ -n "${APP_PID:-}" ] && kill "$APP_PID" 2>/dev/null
    wait "${APP_PID:-}" 2>/dev/null
    [ -n "${XVFB_PID:-}" ] && kill "$XVFB_PID" 2>/dev/null
    wait "${XVFB_PID:-}" 2>/dev/null
    APP_PID=""
}

# Kill the app abruptly (crash-recovery tests); Xvfb stays up.
crash_app() { kill -9 "$APP_PID"; wait "$APP_PID" 2>/dev/null; APP_PID=""; }

# Restart the app on the same display and config dir.
restart_app() {
    rm -f "$STATE"
    XDG_CONFIG_HOME="$WORK/config" XDG_DATA_HOME="$WORK/data" CASEWORK_STATE_FILE="$STATE" \
        "$BIN" "$@" >>"$WORK/app.log" 2>&1 &
    APP_PID=$!
    wait_for '.revision != null' 120
    WIN=$(xdotool search --sync --name Casework | head -1)
    xdotool windowmove "$WIN" 0 0 windowsize "$WIN" 1440 900 2>/dev/null
    # no window manager either to hand out keyboard focus; a window that maps late
    # (an empty document starts fast) can miss the first request, so insist
    focus_win
    idle
}

# st <jq filter>: query the app state.
st() { jq -r "$1" "$STATE" 2>/dev/null; }

# wait_for <jq boolean filter> [seconds]: poll the state file.
wait_for() {
    local f="$1" t="${2:-30}" i=0
    while [ "$i" -lt $((t * 10)) ]; do
        [ "$(jq -r "$f" "$STATE" 2>/dev/null)" = "true" ] && return 0
        sleep 0.1
        i=$((i + 1))
    done
    return 1
}

# idle: wait until nothing regenerates and the camera rests.
idle() {
    sleep 0.3
    wait_for '(.regenerating | not) and (.camera.animating | not)' 120
    sleep 0.3
}

# check <description> <jq boolean filter> [seconds]
check() {
    if wait_for "$2" "${3:-10}"; then
        log "ok   $1"
    else
        log "FAIL $1  ($2)"
        FAILS=$((FAILS + 1))
    fi
}

shot() { import -window root "$SHOTS/${SCENARIO}_$1.png"; log "shot ${SCENARIO}_$1.png"; }

move() { xdotool mousemove "$1" "$2"; sleep "${3:-0.15}"; }
click() { xdotool mousemove "$1" "$2"; sleep 0.12; xdotool click "${3:-1}"; sleep 0.25; }
dclick() { xdotool mousemove "$1" "$2"; sleep 0.12; xdotool click --repeat 2 --delay 80 1; sleep 0.25; }

# drag x0 y0 x1 y1 [button] [modifier]: press, move in steps, release.
drag() {
    local x0=$1 y0=$2 x1=$3 y1=$4 b=${5:-1} m=${6:-}
    xdotool mousemove "$x0" "$y0"
    sleep 0.1
    [ -n "$m" ] && xdotool keydown "$m"
    xdotool mousedown "$b"
    for i in $(seq 1 12); do
        xdotool mousemove $((x0 + (x1 - x0) * i / 12)) $((y0 + (y1 - y0) * i / 12))
        sleep 0.03
    done
    xdotool mouseup "$b"
    [ -n "$m" ] && xdotool keyup "$m"
    sleep 0.3
}

# wheel x y n up|down
wheel() {
    xdotool mousemove "$1" "$2"
    sleep 0.1
    local b=4
    [ "$4" = down ] && b=5
    for _ in $(seq "$3"); do xdotool click "$b"; sleep 0.05; done
    sleep 0.3
}

key() { xdotool key --delay 60 "$@"; sleep 0.25; }
typ() { xdotool type --delay 40 "$1"; sleep 0.2; }

# Select all in the focused text field and type a new value.
retype() { key ctrl+a; typ "$1"; }

finish() {
    stop_app
    if [ "$FAILS" -gt 0 ]; then
        log "$FAILS check(s) failed; log: $WORK/app.log"
        exit 1
    fi
    log "passed"
}
trap 'stop_app' INT TERM
