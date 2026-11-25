#!/bin/bash
# fusion-link — Start/Stop script (fusion-sv management contract)
# Usage: ./start.sh [start|stop|restart|status|log]

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

BIN="$SCRIPT_DIR/target/release/fusion-link"
PID_FILE="$HOME/.fusion-link/fusion-link.pid"
LOG_FILE="$HOME/.fusion-link/fusion-link.log"

ensure_state_dir() {
    mkdir -p "$HOME/.fusion-link"
}

ensure_built() {
    if [ ! -x "$BIN" ]; then
        echo "Binary missing, building (release)..."
        cargo build --release
    fi
}

is_running() {
    [ -f "$PID_FILE" ] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null
}

do_start() {
    ensure_state_dir
    if is_running; then
        echo "fusion-link already running (pid $(cat "$PID_FILE"))"
        return 0
    fi
    ensure_built
    nohup "$BIN" run >>"$LOG_FILE" 2>&1 &
    echo $! >"$PID_FILE"
    sleep 1
    if is_running; then
        echo "fusion-link started (pid $(cat "$PID_FILE"), log $LOG_FILE)"
    else
        echo "fusion-link failed to start, check $LOG_FILE" >&2
        rm -f "$PID_FILE"
        return 1
    fi
}

do_stop() {
    if is_running; then
        local pid
        pid="$(cat "$PID_FILE")"
        kill "$pid" 2>/dev/null || true
        for _ in 1 2 3 4 5; do
            kill -0 "$pid" 2>/dev/null || break
            sleep 1
        done
        if kill -0 "$pid" 2>/dev/null; then
            kill -9 "$pid" 2>/dev/null || true
        fi
        rm -f "$PID_FILE"
        echo "fusion-link stopped"
    else
        echo "fusion-link not running"
        rm -f "$PID_FILE"
    fi
}

do_status() {
    if is_running; then
        echo "running (pid $(cat "$PID_FILE"))"
        "$BIN" status || true
    else
        echo "not running"
        return 1
    fi
}

case "${1:-}" in
    start) do_start ;;
    stop) do_stop ;;
    restart) do_stop; do_start ;;
    status) do_status ;;
    log) tail -n 50 -f "$LOG_FILE" ;;
    *) echo "Usage: $0 [start|stop|restart|status|log]"; exit 1 ;;
esac
