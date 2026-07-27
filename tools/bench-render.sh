#!/usr/bin/env bash
# Render benchmark — compares a terminal's cost to draw the same bytes.
# Run it in a terminite pane and again in Apple Terminal (NOT via Claude's
# Bash tool — that output is pipe-captured and never reaches a renderer,
# so it would measure nothing).
#
#   ./tools/bench-render.sh            # full run, ~1-2 min
#   ./tools/bench-render.sh --quick    # 1/5 payloads, ~20 s
#   ./tools/bench-render.sh --out ~/bench-terminite.txt
#
# For the comparison to mean anything, both apps need the SAME window size,
# font, and font size, and nothing else should be busy. Run them one at a
# time, not side by side.
#
# What each number means:
#   wall MB/s    bytes/second the terminal drained from the pty. Measured
#                with a DSR (cursor-position) round-trip so the clock stops
#                only once the terminal has consumed everything. Caveat: a
#                terminal that parses on one thread and paints on another can
#                answer DSR before the pixels land, which flatters it here —
#                that cost still shows up in app-CPU.
#   app CPU s    CPU seconds the terminal process burned on the workload.
#                This is the honest metric; it can't be hidden by async paint.
#   WS CPU s     WindowServer CPU seconds. A CPU renderer hands finished
#                pixels to WindowServer differently than a GPU one, so this
#                moves independently of app CPU and is worth watching.
#
# Footprint is reported before/after (phys_footprint, the metric that counts
# on macOS — `ps` RSS counts shared pages and ranks these apps backwards).

set -u

# ---------------------------------------------------------------- guards ---

if [ ! -t 1 ]; then
    echo "bench-render: stdout is not a terminal — run this in a real pane." >&2
    exit 1
fi
if [ -n "${TMUX:-}" ] || [ "${TERM:-}" = "screen" ]; then
    echo "bench-render: running under tmux/screen measures the multiplexer," >&2
    echo "              not the terminal. Detach and run in a bare pane." >&2
    exit 1
fi
if [ -n "${SSH_TTY:-}" ]; then
    echo "bench-render: warning — over ssh you are measuring the link, not the renderer." >&2
fi

QUICK=0
OUT=""
while [ $# -gt 0 ]; do
    case "$1" in
        --quick)   QUICK=1 ;;
        --out)     OUT="${2:?--out needs a path}"; shift ;;
        -h|--help) sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *)         echo "bench-render: unknown flag $1" >&2; exit 2 ;;
    esac
    shift
done

# ------------------------------------------------------------- host app ----

# Walk up the process tree to the first process reparented to launchd — that
# is the GUI terminal app hosting this pane.
find_host_app() {
    local pid=$$ ppid comm
    for _ in 1 2 3 4 5 6 7 8; do
        ppid=$(ps -o ppid= -p "$pid" 2>/dev/null | tr -d ' ')
        [ -z "$ppid" ] && { echo ""; return; }
        if [ "$ppid" -le 1 ]; then
            comm=$(ps -o comm= -p "$pid" 2>/dev/null)
            case "$comm" in
                */Contents/MacOS/*) echo "$pid"; return ;;
                *)                  echo "$pid"; return ;;
            esac
        fi
        pid=$ppid
    done
    echo ""
}

APP_PID=$(find_host_app)
APP_NAME="unknown"
if [ -n "$APP_PID" ]; then
    APP_NAME=$(basename "$(ps -o comm= -p "$APP_PID" 2>/dev/null)")
else
    echo "bench-render: could not identify the host app — CPU columns will read 0." >&2
fi
WS_PID=$(pgrep -x WindowServer | head -1)

# Cumulative CPU seconds for a pid. BSD ps prints [dd-][hh:]mm:ss.ff, so this
# keeps the hundredths — enough resolution for multi-second workloads.
cputime() {
    local t
    [ -z "${1:-}" ] && { echo 0; return; }
    t=$(ps -o time= -p "$1" 2>/dev/null | tr -d ' ')
    [ -z "$t" ] && { echo 0; return; }
    awk -v t="$t" 'BEGIN {
        d = 0; rest = t
        if (index(rest, "-") > 0) { split(rest, p, "-"); d = p[1]; rest = p[2] }
        n = split(rest, a, ":"); s = 0
        for (i = 1; i <= n; i++) s = s * 60 + a[i]
        printf "%.2f", s + d * 86400
    }'
}

phys_footprint_mb() {
    [ -z "${1:-}" ] && { echo 0; return; }
    footprint -p "$1" 2>/dev/null | awk '
        /phys_footprint:/ && !/peak/ { v = $2; u = $3 }
        END {
            if (u ~ /^KB/) v /= 1024
            if (u ~ /^GB/) v *= 1024
            printf "%.0f", v
        }'
}

# ------------------------------------------------------------ tty sync -----

# Round-trip a cursor-position report. The terminal cannot answer until it has
# read past every byte we wrote, so this pins down "output consumed".
sync_tty() {
    local old c i
    old=$(stty -g < /dev/tty 2>/dev/null) || return 0
    stty raw -echo min 0 time 30 < /dev/tty 2>/dev/null || return 0
    exec 3< /dev/tty
    printf '\033[6n' > /dev/tty
    for (( i = 0; i < 64; i++ )); do
        IFS= read -r -n1 -u 3 c || break
        [ -z "$c" ] && break            # min 0 / time 30 => empty on timeout
        [ "$c" = "R" ] && break
    done
    exec 3<&-
    stty "$old" < /dev/tty 2>/dev/null
}

# ------------------------------------------------------------- payloads ----

read -r ROWS COLS <<< "$(stty size < /dev/tty)"
: "${ROWS:=50}" "${COLS:=100}"

DIV=1
[ "$QUICK" = 1 ] && DIV=5

WORK_DIR=$(mktemp -d "${TMPDIR:-/tmp}/bench-render.XXXXXX")
trap 'rm -rf "$WORK_DIR"; printf "\033[?1049l\033[0m" > /dev/tty 2>/dev/null' EXIT INT TERM

echo "bench-render: generating payloads in $WORK_DIR ..."

# 1. Plain ASCII scrolling — the baseline path: glyph cache hits, scroll, blit.
awk -v cols="$COLS" -v n=$((120000 / DIV)) 'BEGIN {
    alpha = "abcdefghijklmnopqrstuvwxyz0123456789 "
    body = ""
    for (i = 0; i < cols - 8; i++) body = body substr(alpha, (i % 37) + 1, 1)
    for (k = 0; k < n; k++) printf "%6d %s\n", k, body
}' > "$WORK_DIR/ascii"

# 2. Wide + combining — CJK cells, emoji, a ZWJ sequence and a combining mark.
awk -v cols="$COLS" -v n=$((40000 / DIV)) 'BEGIN {
    chunk = "日本語テキスト 🚀 e\xcc\x81 👩\xe2\x80\x8d💻 "
    body = ""
    # awk counts bytes, not cells: CJK is 3 bytes/2 cells, emoji 4 bytes/2
    # cells, so ~1.7 bytes per display cell fills the row.
    while (length(body) < cols * 1.7) body = body chunk
    for (k = 0; k < n; k++) printf "%s\n", body
}' > "$WORK_DIR/wide"

# 3. SGR churn — a distinct 24-bit colour per cell defeats any run-merging and
#    forces per-cell attribute handling all the way to the blit.
awk -v cols="$COLS" -v n=$((8000 / DIV)) 'BEGIN {
    alpha = "abcdefghijklmnopqrstuvwxyz"
    for (k = 0; k < n; k++) {
        line = ""
        for (c = 0; c < cols; c++) {
            r = (k * 7 + c * 3) % 256; g = (k * 3 + c * 11) % 256; b = (k + c * 5) % 256
            line = line sprintf("\033[38;2;%d;%d;%dm%s", r, g, b, substr(alpha, (c % 26) + 1, 1))
        }
        printf "%s\033[0m\n", line
    }
}' > "$WORK_DIR/sgr"

# 4. Cursor addressing — scattered single-cell writes, no scrolling. This is
#    the damage-tracking stress: a renderer that repaints whole frames pays
#    far more here than one that tracks dirty cells.
awk -v rows="$ROWS" -v cols="$COLS" -v n=$((250000 / DIV)) 'BEGIN {
    srand(20260727)
    alpha = "abcdefghijklmnopqrstuvwxyz"
    for (i = 0; i < n; i++) {
        r = int(rand() * rows) + 1; c = int(rand() * cols) + 1
        printf "\033[%d;%dH%s", r, c, substr(alpha, int(rand() * 26) + 1, 1)
    }
    printf "\033[0m"
}' > "$WORK_DIR/cursor"

# 5. Full-frame repaint on the alt screen — no scrollback, no history growth,
#    just N complete frames. Closest thing to a "fps" number here.
awk -v rows="$ROWS" -v cols="$COLS" -v n=$((800 / DIV)) 'BEGIN {
    alpha = "abcdefghijklmnopqrstuvwxyz0123456789"
    printf "\033[?1049h"
    for (f = 0; f < n; f++) {
        printf "\033[H"
        for (r = 0; r < rows - 1; r++) {
            line = ""
            for (c = 0; c < cols; c++) line = line substr(alpha, ((f + r + c) % 36) + 1, 1)
            printf "%s\n", line
        }
    }
    printf "\033[?1049l"
}' > "$WORK_DIR/repaint"

WORKLOADS="ascii wide sgr cursor repaint"

# ---------------------------------------------------------------- run ------

FP_BEFORE=$(phys_footprint_mb "$APP_PID")
RESULTS=""

for w in $WORKLOADS; do
    f="$WORK_DIR/$w"
    bytes=$(wc -c < "$f" | tr -d ' ')

    app0=$(cputime "$APP_PID"); ws0=$(cputime "$WS_PID")
    elapsed=$( { TIMEFORMAT=%R; time { cat "$f" > /dev/tty; sync_tty; } } 2>&1 )
    app1=$(cputime "$APP_PID"); ws1=$(cputime "$WS_PID")

    RESULTS="$RESULTS$w $bytes $elapsed $app0 $app1 $ws0 $ws1
"
done

FP_AFTER=$(phys_footprint_mb "$APP_PID")
FP_PEAK=$(footprint -p "$APP_PID" 2>/dev/null | awk '/phys_footprint_peak:/ { print $2, $3 }')

# --------------------------------------------------------------- report ----

report() {
    printf '\n'
    printf '=== bench-render ===\n'
    printf 'app        : %s (pid %s)\n' "$APP_NAME" "${APP_PID:-?}"
    printf 'TERM_PROGRAM=%s  TERMINITE=%s  TERM=%s\n' \
        "${TERM_PROGRAM:-<unset>}" "${TERMINITE:-<unset>}" "${TERM:-<unset>}"
    printf 'window     : %sx%s cells%s\n' "$COLS" "$ROWS" \
        "$([ "$QUICK" = 1 ] && echo '   [--quick: payloads /5]')"
    printf 'footprint  : %s MB before -> %s MB after   (peak %s)\n' \
        "$FP_BEFORE" "$FP_AFTER" "${FP_PEAK:-?}"
    printf '\n'
    printf '%-9s %9s %8s %9s %9s %9s %11s\n' \
        workload MB wall_s MB/s app_cpu_s ws_cpu_s cpu_s_per_MB
    printf '%-9s %9s %8s %9s %9s %9s %11s\n' \
        --------- --------- -------- --------- --------- --------- -----------
    printf '%s' "$RESULTS" | while read -r w bytes elapsed a0 a1 w0 w1; do
        [ -z "$w" ] && continue
        awk -v w="$w" -v b="$bytes" -v e="$elapsed" -v a0="$a0" -v a1="$a1" \
            -v w0="$w0" -v w1="$w1" 'BEGIN {
            mb = b / 1048576; acpu = a1 - a0; wcpu = w1 - w0
            printf "%-9s %9.2f %8.2f %9.2f %9.2f %9.2f %11.3f\n",
                w, mb, e, (e > 0 ? mb / e : 0), acpu, wcpu, (mb > 0 ? acpu / mb : 0)
        }'
    done
    printf '\n'
    printf 'Lower cpu_s_per_MB is better; it is the number that cannot be faked\n'
    printf 'by answering DSR before the pixels land. Scrollback from this run is\n'
    printf 'still resident — clear it before reading footprint for anything else.\n'
}

report > /dev/tty
[ -n "$OUT" ] && { report | sed 's/\x1b\[[0-9;]*[A-Za-z]//g' > "$OUT"; echo "written: $OUT" > /dev/tty; }
exit 0
