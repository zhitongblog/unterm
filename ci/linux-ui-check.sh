#!/usr/bin/env bash
# Drive a built Unterm on a virtual X display and record what it looks like:
# the window, its GNOME-style caption buttons (rest, hover), a few tabs in
# git projects, and whether it stayed up. Screenshots and logs land in
# $OUT for review; what can be decided here fails the run.
set -uo pipefail
BIN=${1:-target/release}
OUT=${OUT:-ui-check-linux}
mkdir -p "$OUT"
fail() { echo "FAIL: $*" | tee -a "$OUT/report.txt"; FAILED=1; }
FAILED=0
shot() { import -window root "$OUT/$1.png"; }

export DISPLAY=:99
Xvfb :99 -screen 0 1280x800x24 -nolisten tcp &
sleep 2
openbox &
sleep 1
xsetroot -solid '#3b5f86'

# Projects for the strip to group, each on its own branch.
for p in payments-api web-dashboard; do
  d=$HOME/code/$p; mkdir -p "$d"; git -C "$d" init -q -b main
  echo "# $p" > "$d/README.md"; git -C "$d" add .
  git -C "$d" -c user.email=ci@example.com -c user.name=ci commit -qm init
done
git -C "$HOME/code/payments-api" checkout -qb feat/rate-limit

"$BIN/unterm" > "$OUT/stderr.log" 2>&1 &
APP=$!
for _ in $(seq 1 60); do
  WID=$(xdotool search --onlyvisible --pid "$APP" 2>/dev/null | head -1)
  [ -n "$WID" ] && break
  sleep 1
done
if [ -z "${WID:-}" ]; then
  fail "no window appeared"
  tail -40 "$OUT/stderr.log"
  exit 1
fi
sleep 5
C="$BIN/unterm-cli"
"$C" session create --cwd "$HOME/code/payments-api" >/dev/null || fail "session create"
"$C" session create --cwd "$HOME/code/payments-api" >/dev/null || true
"$C" session create --cwd "$HOME/code/web-dashboard" >/dev/null || true
sleep 3
xdotool windowmove "$WID" 60 40 windowsize "$WID" 1100 680
sleep 3
xdotool mousemove 1200 780
sleep 1
shot 01-window

eval "$(xdotool getwindowgeometry --shell "$WID")"
echo "window: ${WIDTH}x${HEIGHT} at ${X},${Y}" | tee -a "$OUT/report.txt"
# The close button is the rightmost of the three, in the bar's top strip.
xdotool mousemove $((X + WIDTH - 22)) $((Y + 16))
sleep 0.8
shot 02-hover-close
xdotool mousemove $((X + WIDTH - 90)) $((Y + 16))
sleep 0.8
shot 03-hover-maximise
xdotool mousemove 1200 780
kill -0 "$APP" 2>/dev/null || fail "unterm exited"
grep -iE "panic|error" "$OUT/stderr.log" | head -20 > "$OUT/errors.txt" || true
grep -iE "adapter|backend|gpu" "$OUT/stderr.log" | head -10 | tee -a "$OUT/report.txt"
kill "$APP" 2>/dev/null
[ "$FAILED" = 0 ] && echo "linux ui check ok"
exit "$FAILED"
