#!/bin/sh
# gantry-bench container entrypoint. Modes:
#   smoke            keyless plumbing check against the in-container mock upstream
#   live ARGS...     real benchmark run; requires ANTHROPIC_API_KEY and --model
#   shell            interactive shell for debugging
set -eu

OUT="/results/$(date -u +%Y%m%d-%H%M%S)"
MOCK_ADDR="127.0.0.1:18099"

case "${1:-}" in
smoke)
    mock_upstream >/tmp/mock_upstream.log 2>&1 &
    MOCK_PID=$!
    # Bounded readiness poll: ~10s.
    i=0
    until curl -fsS -o /dev/null -X POST "http://${MOCK_ADDR}/v1/messages" --data '{}'; do
        i=$((i + 1))
        if [ "$i" -ge 40 ]; then
            echo "bench-entrypoint: mock upstream not ready after 10s; log:" >&2
            cat /tmp/mock_upstream.log >&2
            exit 1
        fi
        sleep 0.25
    done
    RC=0
    GANTRY_BENCH_UPSTREAM="http://${MOCK_ADDR}" gantry-bench --smoke --out "$OUT" || RC=$?
    kill "$MOCK_PID" 2>/dev/null || true
    echo "bench-entrypoint: smoke artifacts in ${OUT}"
    exit "$RC"
    ;;
live)
    shift
    if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
        echo "bench-entrypoint: ANTHROPIC_API_KEY is required for live runs" \
            "(docker run -e ANTHROPIC_API_KEY ...)" >&2
        exit 2
    fi
    case "$*" in
    *--model*) ;;
    *)
        echo "bench-entrypoint: live mode requires --model <dated-model-id>" >&2
        exit 2
        ;;
    esac
    export GANTRY_BENCH_LIVE=1
    exec gantry-bench --out "$OUT" "$@"
    ;;
shell)
    exec bash
    ;;
*)
    echo "usage: <smoke | live --model <dated-id> [--task ID]... [--harness NAME]... [--reps N] | shell>" >&2
    exit 2
    ;;
esac
