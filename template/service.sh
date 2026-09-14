MODDIR=${0%/*}
STATE_DIR=/data/adb/omk

mkdir -p "$STATE_DIR"

daemon_alive() {
  script=$1
  for pid in $(pgrep -f "$script" 2>/dev/null); do
    [ "$pid" = "$$" ] && continue
    cmdline=$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null)
    case "$cmdline" in
      *"$script"*) return 0 ;;
    esac
  done
  return 1
}

start_daemon() {
  script=$1
  pidfile=$2

  if daemon_alive "$script"; then
    return 0
  fi
  rm -f "$pidfile"

  setsid nohup sh "$script" >/dev/null 2>&1 &
  sleep 1
  if ! daemon_alive "$script"; then
    rm -f "$pidfile"
    return 1
  fi
  return 0
}

start_daemon "$MODDIR/daemon" "$STATE_DIR/keymint-daemon.pid"
start_daemon "$MODDIR/daemon-injector" "$STATE_DIR/injector-daemon.pid"
