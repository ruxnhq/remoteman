#!/bin/sh
# A throwaway Plasma desktop to test the host against: a headless KWin on a private D-Bus
# session, with the desktop portals (KDE's) running inside it, and its own config and data
# folders, so neither the live desktop's screens nor its permission store are touched.
#   scripts/kde-nested.sh COMMAND [ARGS...]
# runs COMMAND inside it (with WAYLAND_DISPLAY and the bus set) and tears it down after.
# RM_LOGS (default a new temporary folder) holds the logs and the folders.
# RM_KDE_AUTHORIZE=1 grants dev.remoteman.Host remote desktop without asking,
# in the nested permission store only.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}"
mkdir -p "$RM_LOGS/config" "$RM_LOGS/data/applications" "$RM_LOGS/cache"
# The portal only lets a program call itself dev.remoteman.Host if that app is installed.
sed "s|^Exec=.*|Exec=/usr/bin/true|" "$root/packaging/linux/dev.remoteman.Host.desktop" >"$RM_LOGS/data/applications/dev.remoteman.Host.desktop"
echo "logs: $RM_LOGS" >&2
exec dbus-run-session -- sh -c '
  export XDG_CONFIG_HOME="$RM_LOGS/config" XDG_DATA_HOME="$RM_LOGS/data" XDG_CACHE_HOME="$RM_LOGS/cache"
  export XDG_CURRENT_DESKTOP=KDE XDG_SESSION_TYPE=wayland KDE_FULL_SESSION=true QT_QPA_PLATFORM=wayland
  unset DISPLAY
  export WAYLAND_DISPLAY=rm-kde-$$
  kwin_wayland --virtual --no-lockscreen --no-global-shortcuts --socket "$WAYLAND_DISPLAY" --width 1920 --height 1080 >"$RM_LOGS/kwin.log" 2>&1 &
  kpid=$!
  for _ in $(seq 50); do [ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ] && break; sleep 0.1; done
  dbus-update-activation-environment XDG_CONFIG_HOME XDG_DATA_HOME XDG_CACHE_HOME XDG_CURRENT_DESKTOP XDG_SESSION_TYPE WAYLAND_DISPLAY QT_QPA_PLATFORM KDE_FULL_SESSION 2>/dev/null
  QT_LOGGING_RULES="xdp-kde-*=true" /usr/lib/xdg-desktop-portal-kde >"$RM_LOGS/portal-kde.log" 2>&1 &
  ppid1=$!
  sleep 1
  /usr/lib/xdg-desktop-portal -r >"$RM_LOGS/portal.log" 2>&1 &
  ppid2=$!
  sleep 1
  if [ -n "$RM_KDE_AUTHORIZE" ]; then
    gdbus call --session --dest org.freedesktop.impl.portal.PermissionStore --object-path /org/freedesktop/impl/portal/PermissionStore \
      --method org.freedesktop.impl.portal.PermissionStore.SetPermission kde-authorized true remote-desktop dev.remoteman.Host "[\"yes\"]" >/dev/null
  fi
  "$@"
  status=$?
  kill $ppid2 $ppid1 2>/dev/null; sleep 0.3; kill $kpid 2>/dev/null
  wait 2>/dev/null
  exit $status
' sh "$@"
