#!/bin/sh
# For measuring on the real desktop: waits for a remoteman virtual monitor to appear in this
# session (a client connected), shows moving test content full screen on it, and takes it away
# when the monitor goes. Stops by itself after MINUTES (default 15).
#   scripts/live-content.sh [MINUTES]
root=$(dirname "$(dirname "$(readlink -f "$0")")")
end=$(( $(date +%s) + ${1:-15} * 60 ))
pid=""
while [ "$(date +%s)" -lt "$end" ]; do
  mon=$(gdctl show 2>/dev/null | grep -o 'Monitor Meta-[0-9]*' | head -1 | cut -d' ' -f2)
  if [ -n "$mon" ] && [ -z "$pid" ]; then
    sleep 1
    python3 "$root/scripts/animate.py" --monitor "$mon" >/dev/null 2>&1 &
    pid=$!
    echo "$(date +%T) content on $mon"
  elif [ -z "$mon" ] && [ -n "$pid" ]; then
    kill "$pid" 2>/dev/null; pid=""
    echo "$(date +%T) monitor gone, content stopped"
  fi
  sleep 0.5
done
[ -n "$pid" ] && kill "$pid" 2>/dev/null
echo "done"
